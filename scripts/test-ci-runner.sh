#!/usr/bin/env bash
#
# test-ci-runner.sh: tests scripts/lib/ci-runner.sh, the step plumbing of
# scripts/local-ci-macos.sh, with stub steps (`true`, `false`) and no Xcode, so it
# runs on Linux CI (ci.yml, job ios-deployment-target) as well as first thing in
# the local gate.
#
# Asserts: the summary lines, dependency skips versus host skips and their counts,
# the exit code, the final line, and that both --fail-fast and JESSE_CI_FAIL_FAST=1
# stop at the first failure.
#
# Bash 3.2 compatible.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIB="$ROOT/scripts/lib/ci-runner.sh"

failures=0
OUT=""
RC=0

# run_case NAME [VAR=VALUE ...] -- SCRIPT
# Runs SCRIPT in a fresh bash that has sourced the runner, capturing all output in
# OUT and the exit code in RC.
run_case() {
  case_name="$1"; shift
  envs=""
  while [ "$1" != "--" ]; do envs="$envs $1"; shift; done
  shift
  body="CI_LABEL=t; CI_HOST_DESC='test host'; . '$LIB'
$1"
  # shellcheck disable=SC2086 # envs is a list of VAR=VALUE words by construction
  OUT="$(env -u JESSE_CI_FAIL_FAST $envs bash -c "set -uo pipefail
$body" 2>&1)"
  RC=$?
}

fail() {
  echo "FAIL [$case_name]: $1"
  echo "---- output ----"
  printf '%s\n' "$OUT"
  echo "----------------"
  failures=$((failures + 1))
}

expect_rc() { [ "$RC" -eq "$1" ] || fail "exit code $RC, expected $1"; }
expect_line() { printf '%s\n' "$OUT" | grep -Fxq -- "$1" || fail "no line: $1"; }
expect_no() { printf '%s\n' "$OUT" | grep -Fq -- "$1" && fail "unexpected: $1"; return 0; }
expect_last() {
  last="$(printf '%s\n' "$OUT" | tail -n 1)"
  [ "$last" = "$1" ] || fail "last line '$last', expected '$1'"
}

# --- every step passes -------------------------------------------------------
run_case "all pass" -- '
run_step "A" true
run_step --needs "A" "B" true
ci_finish'
expect_rc 0
expect_line "  PASS  A (0s)"
expect_line "  PASS  B (0s)"
expect_last "t: ALL CHECKS PASSED — this is what the nightly would run."

# --- host skips are counted, a dependency skip is not --------------------------
run_case "host skips" -- '
run_step "A" true
skip_step "H" "this host cannot"
note_known_skip "Suite/Case"
run_step --needs "H" "D" true
ci_finish'
expect_rc 0
expect_line "  SKIP  H — this host cannot (0s)"
expect_line "  SKIP  known on this host: Suite/Case (0s)"
expect_line "  SKIP  D — SKIP (needs H, which was skipped)"
expect_last "t: ALL CHECKS PASSED, 2 SKIPPED on test host (listed above) — the nightly ios-ci.yml covers those."

# --- a failure carries on; dependents are dependency skips ----------------------
run_case "carry on" -- '
run_step "A" false
run_step --needs "A" "B" true
run_step --needs "B" "C" true
run_step "I" true
skip_step "H" "this host cannot"
run_step "E" false
run_step --needs "I" --needs "E" "F" true
run_step --needs "I" "G" true
echo "counts: SKIPS=$SKIPS DEP_SKIPS=$DEP_SKIPS FAILS=$FAILS"
ci_step_ran "A" && echo "ran: A"
ci_step_ran "I" && echo "ran: I"
ci_step_ran "B" || echo "not ran: B"
ci_step_ran "H" || echo "not ran: H"
ci_step_ran "nope" || echo "not ran: nope"
ci_finish'
expect_rc 1
expect_line "  FAIL  A (0s)"
expect_line "  SKIP  B — SKIP (needs A, which failed)"
expect_line "  SKIP  C — SKIP (needs B, which was skipped)"
expect_line "  PASS  I (0s)"
expect_line "  FAIL  E (0s)"
expect_line "  SKIP  F — SKIP (needs E, which failed)"
expect_line "  PASS  G (0s)"
expect_line "counts: SKIPS=1 DEP_SKIPS=3 FAILS=2"
expect_line "ran: A"
expect_line "ran: I"
expect_line "not ran: B"
expect_line "not ran: H"
expect_line "not ran: nope"
expect_line "Failed steps:"
expect_line "  FAIL  A"
expect_line "  FAIL  E"
expect_no "ALL CHECKS PASSED"
expect_last "t: 2 FAILED (A, E)"

# --- a need that failed is named ahead of one merely skipped ---------------------
run_case "need order" -- '
skip_step "H" "this host cannot"
run_step "X" false
run_step --needs "H" --needs "X" "Y" true
ci_finish'
expect_rc 1
expect_line "  SKIP  Y — SKIP (needs X, which failed)"
expect_last "t: 1 FAILED (X)"

# --- a step runs in the runner's shell, so it can hand a value on ---------------
run_case "shared shell" -- '
set_it() { HANDED=on; }
use_it() { [ "${HANDED:-}" = on ]; }
run_step "set" set_it
run_step --needs "set" "use" use_it
ci_finish'
expect_rc 0
expect_line "  PASS  use (0s)"

# --- fail fast, from the command line --------------------------------------------
run_case "--fail-fast" -- '
ci_parse_args --fail-fast || exit 9
run_step "A" true
run_step "B" false
run_step "C" true
ci_finish'
expect_rc 1
expect_line "  PASS  A (0s)"
expect_line "  FAIL  B (0s)"
expect_no "==> C"
expect_line "t: fail fast: stopped at 'B'; later steps did not run."
expect_last "t: 1 FAILED (B)"

# --- fail fast, from the environment (the pre-push hook takes no arguments) ------
run_case "JESSE_CI_FAIL_FAST=1" JESSE_CI_FAIL_FAST=1 -- '
run_step "A" false
run_step "B" true
ci_finish'
expect_rc 1
expect_no "==> B"
expect_last "t: 1 FAILED (A)"

# --- without either, the same script carries on ----------------------------------
run_case "no fail fast" JESSE_CI_FAIL_FAST=0 -- '
run_step "A" false
run_step "B" true
ci_finish'
expect_rc 1
expect_line "  PASS  B (0s)"
expect_last "t: 1 FAILED (A)"

# --- an unknown argument is refused -----------------------------------------------
run_case "bad argument" -- '
ci_parse_args --nope
echo "rc=$?"'
expect_line "rc=2"

if [ "$failures" -gt 0 ]; then
  echo "test-ci-runner: ${failures} assertion(s) failed"
  exit 1
fi
echo "test-ci-runner: all assertions passed"
