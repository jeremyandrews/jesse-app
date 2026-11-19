#!/usr/bin/env bash
#
# local-ci-macos.sh — run, on this Mac, the checks that .github/workflows/ios-ci.yml
# runs on a hosted macOS runner. Same checks, same order, same flags.
#
# THIS IS THE GATE FOR THE APP. The macOS job no longer runs on pull requests
# (hosted macOS minutes bill at 10x Linux, and that job was essentially the whole
# Actions bill), so nothing on GitHub will tell you the app is broken before your
# PR merges. This script is what tells you. The nightly run of ios-ci.yml is a
# backstop that reports up to a day late, not a gate.
#
# Usage:
#     scripts/local-ci-macos.sh [--fail-fast]
#
# The iPhone tests run on the newest standard "iPhone NN Pro" (scripts/simulator-pick.jq).
# To test on another model, name its device type (from `xcrun simctl list devicetypes`):
#     JESSE_CI_IPHONE_TYPE=com.apple.CoreSimulator.SimDeviceType.iPhone-17 scripts/local-ci-macos.sh
#
# It is also wired as a pre-push hook — install once per clone with:
#     scripts/install-hooks.sh
# after which every `git push` that touches Jesse/ or JesseKit/ runs it and is
# blocked if it fails.
#
# ESCAPE HATCHES (both leave you responsible for the nightly's verdict):
#     git push --no-verify        skip ALL pre-push checks, including the version
#                                 guard — the blunt instrument, avoid it
#     JESSE_SKIP_MAC_CI=1 git push  skip only this script, keep the version guard
#
# EVERY STEP RUNS, and one run reports every failing step, not only the first.
# A step that consumes another step's output (a simulator resolve, a build its
# test needs) runs only when that step passed, and is otherwise printed as
# "SKIP (needs <step>, which failed)": a dependency skip, not a host skip, and
# not counted in the SKIPPED total. After the last step the summary lists every
# FAIL, the final line is "local-ci-macos: N FAILED (<names>)" when anything
# failed, and the script exits 1; otherwise it prints ALL CHECKS PASSED and exits
# 0. The plumbing is scripts/lib/ci-runner.sh, which scripts/test-ci-runner.sh
# tests without Xcode.
#
# FAIL FAST, the old behaviour (stop at the first failing step), is an option:
#     scripts/local-ci-macos.sh --fail-fast
#     JESSE_CI_FAIL_FAST=1 scripts/local-ci-macos.sh   (or JESSE_CI_FAIL_FAST=1 git push,
#                                                    since the hook takes no arguments)
#
# AN OLDER HOST (macOS below 26) cannot run everything CI runs, and pretending
# otherwise made this gate fail on every push from such a machine, so pushes went
# out with --no-verify and the gate checked nothing at all. On such a host:
#
#   * JesseKit still BUILDS on the host, shipping code and tests, with warnings
#     as errors. Its tests cannot LOAD there (the bundle targets macOS 26 and
#     links FoundationModels), so they RUN on the iOS simulator instead, from a
#     synced copy of the package whose iOS minimum is lowered to fit the runtime.
#     The tracked Package.swift is never edited.
#   * The iOS app is BUILT for the generic simulator destination and its test
#     stage is skipped. No concrete simulator is eligible: JesseKit's iOS minimum
#     is above the newest runtime such a host's Xcode carries, and a
#     command-line IPHONEOS_DEPLOYMENT_TARGET does not change eligibility.
#   * The Mac test stage is skipped: xcodebuild will not run a macOS 26 test
#     host on an older macOS. The Mac BUILD still runs.
#   * The watch stages run as they do in CI.
#   * Tests that fail only on this path, for a reason written next to each in
#     OLD_HOST_KNOWN_FAILURES below, are skipped by name.
#
# Every one of these is printed as SKIP in the summary. The nightly ios-ci.yml
# run, on macOS 26, is what still covers them. On a macOS 26 host none of this
# applies and the checks are exactly CI's.
#
# Requires: Xcode (not just Command Line Tools), and `jq`.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

# The step plumbing (run_step, skip_step, note_known_skip, record, summary,
# ci_finish): see the file for what PASS, FAIL and the two kinds of SKIP mean.
# shellcheck source=lib/ci-runner.sh
. "$ROOT/scripts/lib/ci-runner.sh"
ci_parse_args "$@" || exit 2

# --- Toolchain -------------------------------------------------------------
#
# `xcode-select -p` commonly points at CommandLineTools on a dev Mac, where bare
# `xcodebuild` fails and `xcrun simctl` has no simulators. Point at the real
# Xcode for the duration of this script rather than asking for a sudo
# `xcode-select --switch` that changes the whole machine's state.
if [ -z "${DEVELOPER_DIR:-}" ]; then
  case "$(xcode-select -p 2>/dev/null || true)" in
    *Xcode*.app*) : ;;
    *)
      if [ -d /Applications/Xcode.app ]; then
        export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
      fi
      ;;
  esac
fi

if ! xcodebuild -version >/dev/null 2>&1; then
  echo "local-ci-macos: xcodebuild is not usable." >&2
  echo "  Install Xcode (Command Line Tools alone are not enough), or set" >&2
  echo "  DEVELOPER_DIR=/path/to/Xcode.app/Contents/Developer." >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "local-ci-macos: jq not found (used to resolve simulators and read test" >&2
  echo "  results, exactly as CI does). Install it: brew install jq" >&2
  exit 1
fi

echo "local-ci-macos: $(xcodebuild -version | head -1), DEVELOPER_DIR=$(xcode-select -p)"

HOST_OS="$(sw_vers -productVersion)"
if [ "${HOST_OS%%.*}" -lt 26 ]; then
  OLD_HOST=yes
  echo "local-ci-macos: host is macOS ${HOST_OS}, older than 26 — some checks run on a"
  echo "                simulator or are skipped; each is marked SKIP in the summary."
else
  OLD_HOST=no
fi

# Tests skipped on an older host only, each with the reason it fails there and not
# in CI. Anything added here must fail on untouched main on such a host first.
#
#   ArtifactFileTypeTests...EveryAcceptedMime — `text/markdown` has no declared
#     UTType on the iOS simulator, so the type is dynamic there. CI runs JesseKit
#     on macOS, where it is declared.
KIT_SIM_SKIPS="JesseNetworkingTests/ArtifactFileTypeTests/testPreviewItemCarriesATypeAndATitleForEveryAcceptedMime"

CI_HOST_DESC="macOS ${HOST_OS}"

# --- Warnings the local compiler would not promote -------------------------
#
# A newer Swift than the runner's can DEMOTE a diagnostic the runner reports as an
# error, and a demoted diagnostic is not one `-warnings-as-errors` (or `-Werror
# <group>`, or SWIFT_TREAT_WARNINGS_AS_ERRORS) promotes back. PR 333 merged through
# exactly that gap: a pure string helper on a SwiftUI `View`, which is MainActor
# by its conformance, was called from a nonisolated function. Xcode 26.6 on the
# runner rejected it ("call to main actor-isolated static method in a synchronous
# nonisolated context"); this Studio's Swift 6.4 printed the same text as a
# warning, the build succeeded, and the gate passed.
#
# So every build below runs through this, which fails the step on ANY Swift
# warning that survived. Under warnings-as-errors a surviving warning is by
# construction one the toolchain declined to promote, and the runner's older
# compiler may well make it an error.
no_surviving_warnings() {
  log="$(mktemp "${TMPDIR:-/tmp}/local-ci-build.XXXXXX")"
  "$@" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  if [ "$rc" -ne 0 ]; then
    rm -f "$log"
    return "$rc"
  fi
  survived="$(sed $'s/\x1b\\[[0-9;]*m//g' "$log" \
    | grep -E '^/[^ ]*\.swift:[0-9]+:[0-9]+: warning:' | sort -u)"
  rm -f "$log"
  if [ -n "$survived" ]; then
    echo "" >&2
    echo "Swift warnings survived warnings-as-errors. This toolchain demoted them; the" >&2
    echo "runner's may not (see no_surviving_warnings in this script):" >&2
    printf '%s\n' "$survived" >&2
    return 1
  fi
}

# --- The checks, in ios-ci.yml's order -------------------------------------

# 1. JesseKit local package: JesseCore + JesseNetworking + JesseConversations.
#    First because it is by far the fastest feedback (seconds, no simulator) and
#    it is where most of the shared logic lives. Warnings-as-errors, symmetric
#    with the app build below.
#
#    It builds from an EMPTY scratch directory every time, as the runner does. An
#    incremental build does not recompile, and so does not re-print, a file that
#    did not change, and a warning in it would slip past no_surviving_warnings.
jessekit() {
  kit_scratch="$(mktemp -d "${TMPDIR:-/tmp}/local-ci-jessekit.XXXXXX")"
  if [ "$OLD_HOST" = no ]; then
    ( cd "$ROOT/JesseKit" \
      && no_surviving_warnings swift build --scratch-path "$kit_scratch" -Xswiftc -warnings-as-errors \
      && no_surviving_warnings swift test --scratch-path "$kit_scratch" -Xswiftc -warnings-as-errors )
    rc=$?
    rm -rf "$kit_scratch"
    return $rc
  fi
  # `--build-tests` is what `swift test` would have compiled; a plain build skips
  # the test targets and misses a test file that no longer compiles.
  ( cd "$ROOT/JesseKit" \
    && no_surviving_warnings swift build --scratch-path "$kit_scratch" -Xswiftc -warnings-as-errors \
    && no_surviving_warnings swift build --scratch-path "$kit_scratch" --build-tests -Xswiftc -warnings-as-errors )
  rc=$?
  rm -rf "$kit_scratch"
  [ "$rc" -eq 0 ] || return 1
  jessekit_on_simulator
}

# The package's tests on the iOS simulator, for a host that cannot load them. The
# copy is synced (mtimes kept, so the build stays incremental) and only the copy's
# iOS minimum is lowered: xcodebuild decides destination eligibility from
# Package.swift, and a command-line IPHONEOS_DEPLOYMENT_TARGET does not change it.
jessekit_on_simulator() {
  resolve_sim "iOS" "iPhone" "jesse-local-ci-iphone" || return 1
  kit_copy="$ROOT/Jesse/build/JesseKitSimulator"
  mkdir -p "$kit_copy"
  rsync -a --delete --exclude .build --exclude .swiftpm "$ROOT/JesseKit/" "$kit_copy/" || return 1
  pkg_ios="$(sed -n 's/.*\.iOS("\([0-9.]*\)").*/\1/p' "$kit_copy/Package.swift" | head -1)"
  if [ -z "$pkg_ios" ]; then
    echo "No .iOS(\"…\") platform in JesseKit/Package.swift." >&2
    return 1
  fi
  kit_ios="$(printf '%s\n%s\n' "$pkg_ios" "$SIM_VER" | sort -V | head -1)"
  sed -i '' "s/\.iOS(\"${pkg_ios}\")/.iOS(\"${kit_ios}\")/" "$kit_copy/Package.swift"
  echo "JesseKit on the simulator: package ships iOS ${pkg_ios}, simulator runs ${SIM_VER} -> testing at ${kit_ios}"

  skip_args=""
  for t in $KIT_SIM_SKIPS; do
    skip_args="${skip_args} -skip-testing:${t}"
    note_known_skip "$t"
  done
  fresh_bundle ResultKit.xcresult
  # shellcheck disable=SC2086 # skip_args is a list of words by construction
  ( cd "$kit_copy" && xcodebuild test \
      -scheme JesseKit-Package \
      -destination "$SIM_DEST" \
      -derivedDataPath "$ROOT/Jesse/build/DerivedDataKit" \
      -resultBundlePath "$ROOT/Jesse/build/ResultKit.xcresult" \
      $skip_args \
      CODE_SIGNING_ALLOWED=NO )
}

# JesseKit's tests, compiled for iOS. The package declares iOS as well as macOS, and
# `swift test` on this host compiles them for macOS only, so a test that cannot
# compile for iOS (an AppKit type with no fence) passes here and fails nowhere a
# gate looks. PR 242 merged exactly that: StrandBoardRenderTests read its render
# through NSBitmapImageRep, and `main` stopped building its iOS tests. A generic
# destination needs no simulator, so this runs on any macOS 26 host. An older host
# skips it because jessekit_on_simulator already compiles and runs them for iOS.
jessekit_ios_tests_build() {
  ( cd "$ROOT/JesseKit" && xcodebuild build-for-testing \
      -scheme JesseKit-Package \
      -destination "generic/platform=iOS Simulator" \
      -derivedDataPath "$ROOT/Jesse/build/DerivedDataKitIOS" \
      CODE_SIGNING_ALLOWED=NO )
}

# Simulator resolution is scripts/simulator-pick.jq, the same file ios-ci.yml reads, so
# the gate and the nightly pick the same runtime and model. For an iPhone that is the
# newest standard "iPhone NN Pro" (JESSE_CI_IPHONE_TYPE, a device type identifier,
# overrides it); the watch keeps the newest runtime's last Apple Watch. The file says why.
# The one deliberate difference from CI is REUSE — CI creates a throwaway device per
# run because its runner is throwaway; creating a new simulator on every push here
# would leak devices and re-pay first-boot cost each time.
resolve_sim() {
  platform="$1"; family="$2"; sim_name="$3"
  override=""
  if [ "$family" = "iPhone" ]; then override="${JESSE_CI_IPHONE_TYPE:-}"; fi

  read -r RUNTIME_ID RUNTIME_VER DEVTYPE DEV_NAME < <(
    xcrun simctl list runtimes --json | jq -r \
      --arg platform "$platform" --arg family "$family" --arg override "$override" \
      -f "$ROOT/scripts/simulator-pick.jq"
  )
  if [ -z "${RUNTIME_ID:-}" ] || [ "$RUNTIME_ID" = "null" ] || [ -z "${DEVTYPE:-}" ]; then
    if [ -n "$override" ]; then
      echo "No available ${platform} runtime supports JESSE_CI_IPHONE_TYPE=${override}." >&2
      echo "  It must be a device type identifier: xcrun simctl list devicetypes" >&2
    else
      echo "No available ${platform} runtime with a ${family} device type." >&2
      echo "  Install one via Xcode > Settings > Components." >&2
    fi
    return 1
  fi
  echo "Device type: ${DEV_NAME} (${DEVTYPE})${override:+, from JESSE_CI_IPHONE_TYPE}"
  echo "Runtime:     ${platform} ${RUNTIME_VER} (${RUNTIME_ID})"

  # Reuse is by name AND device type. A simulator of this name made by an older rule
  # (an iPhone Duo, an iPhone 11) is deleted, not reused: reusing by name alone would
  # keep testing on the model the rule was changed to stop picking.
  stale="$(
    xcrun simctl list devices --json \
      | jq -r --arg rt "$RUNTIME_ID" --arg n "$sim_name" --arg dt "$DEVTYPE" \
        '(.devices[$rt] // []) | map(select(.name == $n and .deviceTypeIdentifier != $dt)) | .[].udid'
  )"
  for old in $stale; do
    echo "Replacing ${sim_name} ${old}: wrong device type for this rule."
    xcrun simctl shutdown "$old" >/dev/null 2>&1 || true
    xcrun simctl delete "$old" || return 1
  done

  UDID="$(
    xcrun simctl list devices --json \
      | jq -r --arg rt "$RUNTIME_ID" --arg n "$sim_name" --arg dt "$DEVTYPE" \
        '(.devices[$rt] // []) | map(select(.name == $n and .deviceTypeIdentifier == $dt and .isAvailable)) | last | .udid // empty'
  )"
  if [ -z "$UDID" ]; then
    UDID="$(xcrun simctl create "$sim_name" "$DEVTYPE" "$RUNTIME_ID")" || return 1
  fi
  xcrun simctl boot "$UDID" >/dev/null 2>&1 || true
  xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true

  SIM_DEST="platform=${platform} Simulator,id=${UDID}"
  SIM_VER="$RUNTIME_VER"
  echo "Resolved: ${DEV_NAME} (${platform} ${RUNTIME_VER}) -> ${SIM_DEST}"
}

# The gate builds at the target the app SHIPS, lowered only far enough to fit an
# older simulator. Pinning to the simulator's own version instead (which this did
# until the iOS 27 runtime arrived) turns every API the newer SDK deprecates into
# a build error under warnings-as-errors, failing the gate on a diagnostic the
# shipping app never sees. The min, and the reason for it, live in
# scripts/ios-deployment-target.sh.
resolve_ios_sim() {
  resolve_sim "iOS" "iPhone" "jesse-local-ci-iphone" || return 1
  IOS_DEST="$SIM_DEST"
  PROJECT_TARGET="$("$ROOT/scripts/ios-deployment-target.sh" --project-target)" || return 1
  IOS_TARGET="$("$ROOT/scripts/ios-deployment-target.sh" "$SIM_VER")" || return 1
  echo "Deployment target: project ships ${PROJECT_TARGET}, simulator runs ${SIM_VER} -> building at ${IOS_TARGET}"
}

resolve_watch_sim() {
  resolve_sim "watchOS" "Apple Watch" "jesse-local-ci-watch" || return 1
  WATCH_DEST="$SIM_DEST"
}

# `xcodebuild test` REFUSES to write a -resultBundlePath that already exists
# ("error: Existing file at -resultBundlePath"), so the second local run of this
# script would fail before compiling a line. CI never meets this because its
# runner is empty every time; a developer Mac meets it every time but the first.
# Clearing the bundle (and only the bundle — DerivedData is kept, and reusing it
# is the whole reason a local run is fast) is what makes this re-runnable.
fresh_bundle() { rm -rf "$ROOT/Jesse/build/$1"; }

# `xcodebuild test` deliberately does NOT carry SWIFT_TREAT_WARNINGS_AS_ERRORS,
# and that asymmetry is CI's, reproduced here on purpose rather than tidied up:
# the test action also builds JesseUITests, whose XCUIApplication calls emit
# MainActor-isolation warnings in Swift 5 mode. Promoting those to errors fails
# the run on code you did not touch. Warnings are errors for SHIPPING code (the
# `build` action) only.
ios_build() {
  ( cd "$ROOT/Jesse" && no_surviving_warnings xcodebuild build \
      -scheme Jesse \
      -destination "$IOS_DEST" \
      -derivedDataPath build/DerivedData \
      IPHONEOS_DEPLOYMENT_TARGET="$IOS_TARGET" \
      SWIFT_TREAT_WARNINGS_AS_ERRORS=YES \
      SWIFT_SUPPRESS_WARNINGS=NO \
      CODE_SIGNING_ALLOWED=NO )
}

# CODE_SIGNING_ALLOWED=NO is not an optimization, it is fidelity: a bare local
# `xcodebuild test` builds SIGNED and diverges from CI. Code that reads its
# identity from entitlements (HealthKit's HKSource.default() is the one that bit
# this repo) passes signed and takes the test host down unsigned.
ios_test() {
  fresh_bundle Result.xcresult
  ( cd "$ROOT/Jesse" && xcodebuild test \
      -scheme Jesse \
      -destination "$IOS_DEST" \
      -derivedDataPath build/DerivedData \
      -resultBundlePath build/Result.xcresult \
      -enableCodeCoverage YES \
      IPHONEOS_DEPLOYMENT_TARGET="$IOS_TARGET" \
      CODE_SIGNING_ALLOWED=NO )
}

# An older host's compile check of the iOS app: every source, warnings as errors,
# at the target the app ships, with no simulator to be eligible for.
ios_build_generic() {
  ( cd "$ROOT/Jesse" && no_surviving_warnings xcodebuild build \
      -scheme Jesse \
      -destination "generic/platform=iOS Simulator" \
      -derivedDataPath build/DerivedData \
      SWIFT_TREAT_WARNINGS_AS_ERRORS=YES \
      SWIFT_SUPPRESS_WARNINGS=NO \
      CODE_SIGNING_ALLOWED=NO )
}

watch_build() {
  ( cd "$ROOT/Jesse" && no_surviving_warnings xcodebuild build \
      -scheme "Jesse Watch App" \
      -destination "$WATCH_DEST" \
      -derivedDataPath build/DerivedDataWatch \
      SWIFT_TREAT_WARNINGS_AS_ERRORS=YES \
      SWIFT_SUPPRESS_WARNINGS=NO \
      CODE_SIGNING_ALLOWED=NO )
}

watch_test() {
  fresh_bundle ResultWatch.xcresult
  ( cd "$ROOT/Jesse" && xcodebuild test \
      -scheme "Jesse Watch App" \
      -destination "$WATCH_DEST" \
      -derivedDataPath build/DerivedDataWatch \
      -resultBundlePath build/ResultWatch.xcresult \
      CODE_SIGNING_ALLOWED=NO )
}

mac_build() {
  ( cd "$ROOT/Jesse" && no_surviving_warnings xcodebuild build \
      -scheme "Jesse Mac" \
      -destination "platform=macOS" \
      -derivedDataPath build/DerivedDataMac \
      SWIFT_TREAT_WARNINGS_AS_ERRORS=YES \
      SWIFT_SUPPRESS_WARNINGS=NO \
      CODE_SIGNING_ALLOWED=NO )
}

mac_test() {
  fresh_bundle ResultMac.xcresult
  ( cd "$ROOT/Jesse" && xcodebuild test \
      -scheme "Jesse Mac" \
      -destination "platform=macOS" \
      -derivedDataPath build/DerivedDataMac \
      -resultBundlePath build/ResultMac.xcresult \
      CODE_SIGNING_ALLOWED=NO )
}

# Step names, in one place, because a step's needs name other steps exactly.
S_RUNNER_TESTS="Runner tests (scripts/test-ci-runner.sh)"
S_SWIFT_GUARDS="Swift test guards (scripts/swift-test-guards.sh)"
S_KIT_OLD="JesseKit package (host build + tests, warnings-as-errors; tests on iOS sim)"
S_KIT="JesseKit package (build + test, warnings-as-errors)"
S_KIT_IOS="JesseKit tests compile for iOS"
S_IOS_GENERIC="iOS build (generic simulator, warnings-as-errors)"
S_IOS_SIM="Resolve iOS simulator"
S_IOS_BUILD="iOS build (warnings-as-errors)"
S_IOS_TEST="iOS test"
S_WATCH_SIM="Resolve watchOS simulator"
S_WATCH_BUILD="watch build (warnings-as-errors)"
S_WATCH_TEST="watch test"
S_MAC_BUILD="Mac build (warnings-as-errors)"
S_MAC_TEST="Mac test"
S_COUNTS="Test counts (every suite that ran, ran tests)"

# CI's "Report test count" steps are a real gate, not decoration: a suite that
# silently ran ZERO tests is the failure mode a green xcodebuild does not catch.
# Same assertion here.
#
# It reads ONLY the bundle of a test step that ran in this invocation. Every bundle
# is cleared before the first step (clear_result_bundles), so one left by an earlier
# run can never be counted as if it ran now; a suite whose step was skipped is
# printed as not run. A step that FAILED without writing a bundle (it failed to
# build, say) is already a FAIL above and is not failed twice here.
report_counts() {
  rc=0
  if [ "$OLD_HOST" = yes ]; then
    # The iOS and Mac suites did not run here; the JesseKit suite ran on the
    # simulator rather than through `swift test`, so it gets the zero-test check.
    suites="JesseKitTests|ResultKit.xcresult|${S_KIT_OLD}
JesseWatchTests|ResultWatch.xcresult|${S_WATCH_TEST}"
  else
    suites="JesseTests|Result.xcresult|${S_IOS_TEST}
JesseWatchTests|ResultWatch.xcresult|${S_WATCH_TEST}
JesseMacTests|ResultMac.xcresult|${S_MAC_TEST}"
  fi
  while IFS='|' read -r label file step; do
    bundle="$ROOT/Jesse/build/${file}"
    if ! ci_step_ran "$step"; then
      echo "${label}: not run ('${step}' did not run in this invocation)"
      continue
    fi
    if [ ! -d "$bundle" ]; then
      if [ "$(ci_step_status "$step")" = FAIL ]; then
        echo "${label}: no result bundle ('${step}' failed before writing one)"
      else
        echo "No result bundle at ${bundle}" >&2
        rc=1
      fi
      continue
    fi
    s="$(xcrun xcresulttool get test-results summary --path "$bundle")"
    total=$(printf '%s' "$s" | jq -r '.totalTestCount')
    passed=$(printf '%s' "$s" | jq -r '.passedTests')
    failed=$(printf '%s' "$s" | jq -r '.failedTests')
    skipped=$(printf '%s' "$s" | jq -r '.skippedTests')
    echo "${label}: total=${total} passed=${passed} failed=${failed} skipped=${skipped}"
    if [ "${total}" -le 0 ]; then
      echo "Expected ${label} to run at least one test, got ${total}" >&2
      rc=1
    fi
  done <<EOF2
$suites
EOF2
  return $rc
}

clear_result_bundles() {
  for b in Result.xcresult ResultWatch.xcresult ResultMac.xcresult ResultKit.xcresult; do
    fresh_bundle "$b"
  done
}

# --- The run ---------------------------------------------------------------
#
# Dependencies (a step runs only when every step it needs PASSED):
#   macOS 26 host:  Resolve iOS simulator -> iOS build -> iOS test
#   older host:     iOS build (generic simulator) stands alone; iOS test is a host SKIP
#   both:           Resolve watchOS simulator -> watch build -> watch test
#                   Mac build -> Mac test (a host SKIP on an older host)
#   independent:    the two guards, the JesseKit steps, and Test counts (always runs)

clear_result_bundles
if [ "$CI_FAIL_FAST" = 1 ]; then
  echo "local-ci-macos: fail fast: stopping at the first failing step."
fi

run_step "$S_RUNNER_TESTS"                                     bash "$ROOT/scripts/test-ci-runner.sh"
run_step "$S_SWIFT_GUARDS"                                     bash "$ROOT/scripts/swift-test-guards.sh" "$ROOT"
if [ "$OLD_HOST" = yes ]; then
  run_step "$S_KIT_OLD"                                        jessekit
else
  run_step "$S_KIT"                                            jessekit
  run_step "$S_KIT_IOS"                                        jessekit_ios_tests_build
fi
if [ "$OLD_HOST" = yes ]; then
  run_step "$S_IOS_GENERIC"                                    ios_build_generic
  skip_step "$S_IOS_TEST" "no iOS simulator on this host is eligible for JesseKit's iOS minimum"
else
  run_step "$S_IOS_SIM"                                        resolve_ios_sim
  run_step --needs "$S_IOS_SIM" "$S_IOS_BUILD"                 ios_build
  run_step --needs "$S_IOS_SIM" --needs "$S_IOS_BUILD" "$S_IOS_TEST" ios_test
fi
run_step "$S_WATCH_SIM"                                        resolve_watch_sim
run_step --needs "$S_WATCH_SIM" "$S_WATCH_BUILD"               watch_build
run_step --needs "$S_WATCH_SIM" --needs "$S_WATCH_BUILD" "$S_WATCH_TEST" watch_test
run_step "$S_MAC_BUILD"                                        mac_build
if [ "$OLD_HOST" = yes ]; then
  skip_step "$S_MAC_TEST" "xcodebuild cannot run a macOS 26 test host on macOS ${HOST_OS}"
else
  run_step --needs "$S_MAC_BUILD" "$S_MAC_TEST"                mac_test
fi
run_step "$S_COUNTS"                                           report_counts

ci_finish
