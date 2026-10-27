#!/usr/bin/env bash
#
# nav-cost.sh: measure, then report, what the app's everyday navigation costs, per scenario,
# from a simulator's log.
#
# The app writes one `nav` signpost interval per suspect piece of navigation work (see
# `NavTrace`) and one `render` notice per SwiftUI body evaluation when `JESSE_RENDER_PROBE=1`
# (see `RenderProbe`). `NavigationCostUITests` brackets each measured iteration with a
# `mark pf1-<scale>-<scenario>-<n>-begin/end` line. This reads all three back out of the log
# and prints, per scenario: the time inside each signposted interval and how many intervals
# ran, and how many body evaluations each view had, all as a mean per iteration.
#
# Usage:
#     scripts/nav-cost.sh run <simulator-udid> [ios-deployment-target]
#         runs NavigationCost1000UITests and NavigationCost5UITests on that simulator
#         (with TEST_RUNNER_PF1_MEASURE=1, which they skip without), then reports
#     scripts/nav-cost.sh report <simulator-udid> [minutes-back]
#         reports on whatever the log holds (default: the last 60 minutes)
#
# The XCTClockMetric medians are in the result bundle and in the `PF1 ... ms=` lines the
# test prints; this script is the per interval breakdown behind them.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-}"
UDID="${2:-}"
if [ -z "$MODE" ] || [ -z "$UDID" ]; then
  sed -n '3,22p' "$0" >&2
  exit 2
fi

report() {
  local minutes="$1"
  xcrun simctl spawn "$UDID" log show --last "${minutes}m" --signpost --style ndjson \
      --predicate 'subsystem == "com.tag1.jesse" AND (category == "nav" OR category == "render")' \
      2>/dev/null \
    | jq -r 'select(.subsystem == "com.tag1.jesse")
             | [.timestamp, (.signpostType // "log"), (.signpostName // ""),
                ((.signpostID // 0) | tostring), (.eventMessage // "")] | join("|")' \
    | awk -F'|' '
      function secs(ts,   t, h) { split(substr(ts, 12, 15), t, ":"); return t[1]*3600 + t[2]*60 + t[3] }
      $2 == "log" && $5 ~ /^mark pf1-.*-begin$/ {
        w = $5; sub(/^mark /, "", w); sub(/-begin$/, "", w)
        scen = w; sub(/-[0-9]+$/, "", scen); window = scen
        if (!(scen in iters)) order[++n] = scen
        iters[scen]++; next
      }
      $2 == "log" && $5 ~ /^mark pf1-.*-end$/ { window = ""; next }
      $2 == "log" && $5 ~ /^body / {
        if (window == "") next
        v = $5; sub(/^body /, "", v); bodies[window "|" v]++; next
      }
      $2 == "begin" { open[$4] = secs($1); openwin[$4] = window; next }
      $2 == "end" && ($4 in open) {
        w = openwin[$4]; d = (secs($1) - open[$4]) * 1000
        delete open[$4]; delete openwin[$4]
        if (w == "") next
        ms[w "|" $3] += d; cnt[w "|" $3]++
        if (d > worst[w "|" $3]) worst[w "|" $3] = d
        next
      }
      END {
        for (i = 1; i <= n; i++) {
          s = order[i]; k = iters[s]
          printf "\n%s (%d iterations), per iteration:\n", s, k
          for (key in ms) {
            split(key, p, "|"); if (p[1] != s) continue
            printf "  %-22s %9.2f ms  %7.1f intervals  worst %7.2f ms\n", p[2], ms[key]/k, cnt[key]/k, worst[key]
          }
          for (key in bodies) {
            split(key, p, "|"); if (p[1] != s) continue
            printf "  body %-17s %7.1f evaluations\n", p[2], bodies[key]/k
          }
        }
      }'
}

case "$MODE" in
  run)
    # ONE TEST AT A TIME, each reported straight after it runs: the simulator keeps
    # signposts for minutes, not for the length of the whole measurement.
    TARGET="${3:-$("$ROOT/scripts/ios-deployment-target.sh" --project-target)}"
    ( cd "$ROOT/Jesse" && xcodebuild build-for-testing \
        -scheme Jesse \
        -destination "platform=iOS Simulator,id=${UDID}" \
        -derivedDataPath build/DerivedData \
        IPHONEOS_DEPLOYMENT_TARGET="$TARGET" \
        CODE_SIGNING_ALLOWED=NO ) >&2 || exit 1
    for scale in 1000 5; do
      for test in testANewChat testBBackOut testCTabCycle testDOpenLong; do
        ( cd "$ROOT/Jesse" && TEST_RUNNER_PF1_MEASURE=1 xcodebuild test-without-building \
            -scheme Jesse \
            -destination "platform=iOS Simulator,id=${UDID}" \
            -derivedDataPath build/DerivedData \
            -only-testing:"JesseUITests/NavigationCost${scale}UITests/${test}" \
            -collect-test-diagnostics never \
            CODE_SIGNING_ALLOWED=NO ) > >(tee -a /dev/stderr | grep '^PF1 ') 2>&1
        report 6
      done
    done
    ;;
  report)
    report "${3:-60}"
    ;;
  *)
    echo "nav-cost: unknown mode '$MODE' (run or report)" >&2
    exit 2
    ;;
esac
