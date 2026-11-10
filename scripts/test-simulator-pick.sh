#!/usr/bin/env bash
#
# test-simulator-pick.sh — exercise scripts/simulator-pick.jq, the rule that decides
# which simulator scripts/local-ci-macos.sh and ios-ci.yml test the app on.
#
# The rule fails silently: a wrong pick still builds and still runs, it just runs on a
# model nobody meant (an iPhone Duo from a single-device runtime, an iPhone 11 because
# it was listed last), and the gate goes red, or green, for a reason that has nothing to
# do with the change being tested. Each case below is a runtime list in the shape
# `xcrun simctl list runtimes --json` prints, and the device it must resolve to.
#
# No simulator, no Xcode, no macOS: CI runs this on Linux on every push. Needs jq.
#
# Usage: bash scripts/test-simulator-pick.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PICK="$ROOT/scripts/simulator-pick.jq"

PASSED=0
FAILED=0

# A device type entry. $1 family, $2 name; the identifier is derived the way
# CoreSimulator spells them closely enough for matching.
dt() {
  id="com.apple.CoreSimulator.SimDeviceType.$(printf '%s' "$2" | tr ' ()' '-')"
  printf '{"productFamily":"%s","name":"%s","identifier":"%s"}' "$1" "$2" "$id"
}

# A runtime entry. $1 platform, $2 version, $3 identifier suffix, $4 isAvailable,
# rest: device type entries.
rt() {
  platform="$1"; ver="$2"; suffix="$3"; avail="$4"; shift 4
  types="$(IFS=,; printf '%s' "$*")"
  printf '{"platform":"%s","version":"%s","identifier":"com.apple.CoreSimulator.SimRuntime.%s","isAvailable":%s,"supportedDeviceTypes":[%s]}' \
    "$platform" "$ver" "$suffix" "$avail" "$types"
}

runtimes() { printf '{"runtimes":[%s]}' "$(IFS=,; printf '%s' "$*")"; }

# check <description> <expected "runtime-suffix|device name" or "null"> <platform>
#       <family> <override> <json>
check() {
  desc="$1"; want="$2"; platform="$3"; family="$4"; override="$5"; json="$6"
  out="$(printf '%s' "$json" | jq -r --arg platform "$platform" --arg family "$family" \
    --arg override "$override" -f "$PICK")"
  if [ "$out" = "null" ]; then
    got="null"
  else
    read -r rid _ver _dtid name <<<"$out"
    got="${rid#com.apple.CoreSimulator.SimRuntime.}|${name}"
  fi
  if [ "$got" = "$want" ]; then
    PASSED=$((PASSED + 1))
    echo "ok   - $desc"
  else
    FAILED=$((FAILED + 1))
    echo "FAIL - $desc: expected '$want', got '$got'"
  fi
}

P=iPhone
W="Apple Watch"

# The runtime list Xcode 27 installed on the Mac Studio, trimmed: the general iOS 27.2
# runtime, then a second 27.2 runtime that supports only iPhone Duo. The old rule took
# the Duo runtime (same version, listed later) and ran every UI test on it.
general_27="$(rt iOS 27.2 iOS-27-2 true \
  "$(dt $P 'iPhone 18 Pro')" "$(dt $P 'iPhone 18 Pro Max')" "$(dt $P 'iPhone 17 Pro')" \
  "$(dt $P 'iPhone 17e')" "$(dt $P 'iPhone Air')" "$(dt $P 'iPhone 17')" \
  "$(dt $P 'iPhone 16 Plus')" "$(dt $P 'iPhone SE (3rd generation)')" \
  "$(dt $P 'iPhone 13 mini')" "$(dt $P 'iPhone 11')")"
duo_27="$(rt iOS 27.2 iOS-27-2-iPhone19-4 true "$(dt $P 'iPhone Duo')")"
older_26="$(rt iOS 26.3.1 iOS-26-3 true "$(dt $P 'iPhone 17 Pro')" "$(dt $P 'iPhone 11')")"

check "a single-device runtime at the same version loses to the general one" \
  "iOS-27-2|iPhone 18 Pro" iOS $P "" "$(runtimes "$older_26" "$general_27" "$duo_27")"
check "listed in the other order, the same pick" \
  "iOS-27-2|iPhone 18 Pro" iOS $P "" "$(runtimes "$duo_27" "$general_27" "$older_26")"
check "a newer runtime that offers only a Duo loses to an older one with a Pro" \
  "iOS-26-3|iPhone 17 Pro" iOS $P "" \
  "$(runtimes "$older_26" "$(rt iOS 28.0 iOS-28-0-Duo true "$(dt $P 'iPhone Duo')")")"
check "the highest number wins, not the first or last listed" \
  "iOS-27-2|iPhone 18 Pro" iOS $P "" \
  "$(runtimes "$(rt iOS 27.2 iOS-27-2 true "$(dt $P 'iPhone 9 Pro')" "$(dt $P 'iPhone 18 Pro')" "$(dt $P 'iPhone 17 Pro')")")"
check "Pro Max, Plus, mini, e, SE and Air are never the automatic pick" \
  "iOS-27-2|iPhone 15" iOS $P "" \
  "$(runtimes "$(rt iOS 27.2 iOS-27-2 true "$(dt $P 'iPhone 18 Pro Max')" "$(dt $P 'iPhone 17e')" \
    "$(dt $P 'iPhone Air')" "$(dt $P 'iPhone 16 Plus')" "$(dt $P 'iPhone 13 mini')" \
    "$(dt $P 'iPhone SE (3rd generation)')" "$(dt $P 'iPhone 15')" "$(dt $P 'iPhone Fold')")")"
check "no standard model at all falls back to the last one listed" \
  "iOS-27-2|iPhone Fold" iOS $P "" \
  "$(runtimes "$(rt iOS 27.2 iOS-27-2 true "$(dt $P 'iPhone Air')" "$(dt $P 'iPhone Fold')")")"
check "a newer runtime wins within the same tier" \
  "iOS-27-2|iPhone 18 Pro" iOS $P "" "$(runtimes "$general_27" "$older_26")"
check "versions compare as numbers, not strings (26.10 is newer than 26.9)" \
  "iOS-26-10|iPhone 17 Pro" iOS $P "" \
  "$(runtimes "$(rt iOS 26.10 iOS-26-10 true "$(dt $P 'iPhone 17 Pro')")" \
    "$(rt iOS 26.9 iOS-26-9 true "$(dt $P 'iPhone 17 Pro')")")"
check "an unavailable runtime is never picked" \
  "iOS-26-3|iPhone 17 Pro" iOS $P "" \
  "$(runtimes "$older_26" "$(rt iOS 27.2 iOS-27-2 false "$(dt $P 'iPhone 18 Pro')")")"
check "the override picks exactly that device type, Duo included" \
  "iOS-27-2-iPhone19-4|iPhone Duo" iOS $P "com.apple.CoreSimulator.SimDeviceType.iPhone-Duo" \
  "$(runtimes "$general_27" "$duo_27")"
check "the override may name a model the automatic rule excludes" \
  "iOS-27-2|iPhone 17e" iOS $P "com.apple.CoreSimulator.SimDeviceType.iPhone-17e" \
  "$(runtimes "$older_26" "$general_27")"
check "an override no runtime supports picks nothing rather than something else" \
  "null" iOS $P "com.apple.CoreSimulator.SimDeviceType.iPhone-99" \
  "$(runtimes "$general_27" "$duo_27")"
check "no runtime for the platform picks nothing" \
  "null" iOS $P "" "$(runtimes "$(rt watchOS 27.2 watchOS-27-2 true "$(dt "$W" 'Apple Watch Ultra 4 (49mm)')")")"

# The watch keeps the old rule: newest runtime, last Apple Watch it lists. The override
# is for iPhones only and does not reach it.
watch_26="$(rt watchOS 26.2 watchOS-26-2 true "$(dt "$W" 'Apple Watch Series 11 (46mm)')" \
  "$(dt "$W" 'Apple Watch Series 6 (40mm)')")"
watch_27="$(rt watchOS 27.2 watchOS-27-2 true "$(dt "$W" 'Apple Watch Series 12 (46mm)')" \
  "$(dt "$W" 'Apple Watch SE 3 (40mm)')")"
check "the watch is the newest runtime's last Apple Watch, as before" \
  "watchOS-27-2|Apple Watch SE 3 (40mm)" watchOS "$W" "" "$(runtimes "$watch_26" "$watch_27")"
check "the watch ignores iPhone device types in a mixed list" \
  "watchOS-27-2|Apple Watch SE 3 (40mm)" watchOS "$W" "" \
  "$(runtimes "$watch_27" "$general_27")"

echo ""
echo "simulator-pick: ${PASSED} passed, ${FAILED} failed"
[ "$FAILED" -eq 0 ]
