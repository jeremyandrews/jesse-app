# shellcheck shell=bash
#
# ci-runner.sh: the step plumbing of scripts/local-ci-macos.sh, sourced, so that
# scripts/test-ci-runner.sh can drive it with stub steps and no Xcode.
#
# A run is a list of steps. Each step is recorded as one of:
#
#   PASS  the step's command exited 0
#   FAIL  the step's command exited non-zero. The run CARRIES ON: one run reports
#         every failing step, not only the first. (Stopping at the first one hid ten
#         failures in five suites for days in September 2026: a crash in step 1
#         meant nothing after it ever ran.)
#   SKIP  a HOST skip (skip_step, note_known_skip): a check this host cannot
#         perform. Counted in the SKIPPED total of the final PASSED line.
#   SKIP (needs <step>, which failed)
#         a DEPENDENCY skip: the step consumes the output of a step that did not
#         pass, so running it would only repeat that failure. Not a host skip, and
#         not counted in that total.
#
# After the last step, ci_finish prints the summary, lists every FAIL, and exits 1
# if any step failed, 0 otherwise.
#
# FAIL FAST, the old behaviour, stops at the first FAIL instead. Turn it on with
# ci_parse_args --fail-fast, or JESSE_CI_FAIL_FAST=1 in the environment (the
# pre-push hook cannot take arguments).
#
# Bash 3.2 compatible (the macOS system bash): no mapfile, no associative arrays.
# Step names are free text; a need names a step by its exact name.

CI_LABEL="${CI_LABEL:-local-ci-macos}"
# Shown in the PASSED line when a host skip happened, e.g. "macOS 15.7".
CI_HOST_DESC="${CI_HOST_DESC:-this host}"

RESULTS=""
STARTED_AT=$SECONDS
SKIPS=0          # host skips only
DEP_SKIPS=0      # dependency skips
FAILS=0
FAILED_NAMES=""  # newline separated, in run order
# One line per finished step: "<STATUS><TAB><name>", STATUS in PASS FAIL SKIP DEPSKIP.
STEP_STATUS=""

case "${JESSE_CI_FAIL_FAST:-}" in
  1|yes|true) CI_FAIL_FAST=1 ;;
  *) CI_FAIL_FAST="${CI_FAIL_FAST:-0}" ;;
esac

TAB="$(printf '\t')"

ci_usage() {
  echo "usage: ${CI_LABEL} [--fail-fast]"
  echo "  --fail-fast   stop at the first failing step (also JESSE_CI_FAIL_FAST=1)"
}

# Parses the script's own arguments. Returns 2 on an unknown one.
ci_parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --fail-fast) CI_FAIL_FAST=1 ;;
      -h|--help) ci_usage; exit 0 ;;
      *) echo "${CI_LABEL}: unknown argument '$1'" >&2; ci_usage >&2; return 2 ;;
    esac
    shift
  done
}

summary() {
  echo ""
  echo "============================================================"
  printf '%s' "$RESULTS"
  echo "============================================================"
  echo "total: $((SECONDS - STARTED_AT))s"
}

# record STATUS TEXT [SECONDS]
record() {
  if [ $# -ge 3 ]; then
    RESULTS="${RESULTS}  $1  $2 ($3s)
"
  else
    RESULTS="${RESULTS}  $1  $2
"
  fi
}

_ci_set_status() {
  STEP_STATUS="${STEP_STATUS}$1${TAB}$2
"
}

# The status a finished step was recorded with, or nothing if it never was.
ci_step_status() {
  printf '%s' "$STEP_STATUS" | awk -F'\t' -v n="$1" '$2 == n { s = $1 } END { if (s != "") print s }'
}

# True when the step ran in THIS invocation, whether it passed or failed.
ci_step_ran() {
  case "$(ci_step_status "$1")" in
    PASS|FAIL) return 0 ;;
    *) return 1 ;;
  esac
}

# A check this host cannot perform, recorded as such rather than as a pass.
skip_step() {
  echo ""
  echo "==> $1"
  echo "SKIPPED: $2"
  record "SKIP" "$1 — $2" 0
  _ci_set_status SKIP "$1"
  SKIPS=$((SKIPS + 1))
}

# A test skipped inside a step that otherwise ran.
note_known_skip() {
  record "SKIP" "known on this host: $1" 0
  SKIPS=$((SKIPS + 1))
}

# ci_finish: the summary, every FAIL, the final line, and the exit code.
ci_finish() {
  local names
  summary
  echo ""
  if [ "$FAILS" -gt 0 ]; then
    echo "Failed steps:"
    printf '%s' "$FAILED_NAMES" | while IFS= read -r n; do echo "  FAIL  $n"; done
    if [ "$DEP_SKIPS" -gt 0 ]; then
      echo "${DEP_SKIPS} step(s) did not run because a step they need failed (SKIP (needs ...) above)."
    fi
    echo ""
    names="$(printf '%s' "$FAILED_NAMES" | awk 'NR > 1 { printf ", " } { printf "%s", $0 }')"
    echo "${CI_LABEL}: ${FAILS} FAILED (${names})"
    exit 1
  fi
  if [ "$SKIPS" -gt 0 ]; then
    echo "${CI_LABEL}: ALL CHECKS PASSED, ${SKIPS} SKIPPED on ${CI_HOST_DESC} (listed above) — the nightly ios-ci.yml covers those."
  else
    echo "${CI_LABEL}: ALL CHECKS PASSED — this is what the nightly would run."
  fi
  exit 0
}

# run_step [--needs STEP]... NAME COMMAND [ARGS...]
#
# Runs COMMAND in this shell (so a step may set variables a later step reads, as
# the simulator resolve steps do) when every STEP it needs has PASSED. Otherwise
# records a dependency skip naming the first need that failed (or, failing that,
# the first that did not pass).
run_step() {
  local needs="" name blocked blocked_why need st t0
  needs=""
  while [ "${1:-}" = "--needs" ]; do
    needs="${needs}$2
"
    shift 2
  done
  name="$1"; shift

  blocked=""; blocked_why=""
  if [ -n "$needs" ]; then
    while IFS= read -r need; do
      [ -n "$need" ] || continue
      st="$(ci_step_status "$need")"
      case "$st" in
        PASS) : ;;
        FAIL)
          if [ "$blocked_why" != "failed" ]; then blocked="$need"; blocked_why="failed"; fi ;;
        *)
          if [ -z "$blocked" ]; then
            blocked="$need"
            case "$st" in
              SKIP|DEPSKIP) blocked_why="was skipped" ;;
              *) blocked_why="did not run" ;;
            esac
          fi ;;
      esac
    done <<EOF
$needs
EOF
  fi
  if [ -n "$blocked" ]; then
    [ "$blocked_why" = "failed" ] && blocked_why="which failed" || blocked_why="which ${blocked_why}"
    echo ""
    echo "==> ${name}"
    echo "SKIPPED: needs ${blocked}, ${blocked_why}"
    record "SKIP" "${name} — SKIP (needs ${blocked}, ${blocked_why})"
    _ci_set_status DEPSKIP "$name"
    DEP_SKIPS=$((DEP_SKIPS + 1))
    return 0
  fi

  echo ""
  echo "==> ${name}"
  t0=$SECONDS
  if "$@"; then
    record "PASS" "$name" "$((SECONDS - t0))"
    _ci_set_status PASS "$name"
  else
    record "FAIL" "$name" "$((SECONDS - t0))"
    _ci_set_status FAIL "$name"
    FAILS=$((FAILS + 1))
    FAILED_NAMES="${FAILED_NAMES}${name}
"
    if [ "$CI_FAIL_FAST" = 1 ]; then
      echo ""
      echo "${CI_LABEL}: fail fast: stopped at '${name}'; later steps did not run."
      ci_finish
    fi
  fi
  return 0
}
