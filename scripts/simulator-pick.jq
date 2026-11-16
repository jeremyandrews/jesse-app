# simulator-pick.jq — which simulator runtime and device type the app's gates test on.
#
# ONE program, read by both scripts/local-ci-macos.sh and .github/workflows/ios-ci.yml
# (`jq -r -f`), so the local gate and the nightly cannot pick different models.
# scripts/test-simulator-pick.sh pins its behaviour on Linux in ci.yml.
#
# Input: `xcrun simctl list runtimes --json`.
# Args:  --arg platform  iOS | watchOS
#        --arg family    iPhone | Apple Watch   (a device type's productFamily)
#        --arg override  a device type identifier, or "" (iPhone only)
# Output: "<runtime id> <runtime version> <device type id> <device type name>", or
#         "null" when nothing qualifies.
#
# WHY NOT "THE LAST DEVICE TYPE OF THE NEWEST RUNTIME". That was the rule, and it picks
# whatever Xcode happens to list last. Xcode 27 ships a second iOS 27.2 runtime,
# `iOS-27-2-iPhone19-4`, that supports one device type, iPhone Duo. Both runtimes are
# version 27.2, the sort is stable, and the Duo runtime is listed second, so the gate ran
# every UI test on an iPhone Duo. On that device iOS draws a TabView's tabs as a vertical
# rail beside the content, not as a bottom tab bar, so there is no TabBar element and 38
# UI tests timed out looking for one. Before that, the same rule picked iPhone 11.
#
# THE RULE NOW, for iPhone:
#   0. $override, when set: that device type exactly, or nothing (a typo should fail the
#      gate, not quietly test something else).
#   1. the highest numbered "iPhone NN Pro";
#   2. the highest numbered "iPhone NN";
#   3. the old rule, the last iPhone listed.
# Tiers 1 and 2 match the whole name, so Pro Max, Plus, mini, e, SE, Air, Duo and any
# fold are never picked automatically. The best tier any runtime offers wins, then the
# newest runtime within it. That is what makes a single-device runtime lose to the
# general one at the same version.
#
# The Apple Watch keeps the old rule (tier 3 everywhere): it resolves to an ordinary
# watch (SE 3, 40mm) and its tests pass there.

def model_number: capture("^iPhone (?<n>[0-9]+)").n | tonumber;

def tiered($rank): if . == null then null else {dt: ., rank: $rank} end;

def choose($family; $override):
  .supportedDeviceTypes | map(select(.productFamily == $family)) as $types
  | if $family != "iPhone" then
      ($types | last | tiered(3))
    elif $override != "" then
      ($types | map(select(.identifier == $override)) | first | tiered(0))
    else
      ($types | map(select(.name | test("^iPhone [0-9]+ Pro$")))
              | sort_by(.name | model_number) | last | tiered(1))
      // ($types | map(select(.name | test("^iPhone [0-9]+$")))
                 | sort_by(.name | model_number) | last | tiered(2))
      // ($types | last | tiered(3))
    end;

.runtimes
| map(select(.platform == $platform and .isAvailable))
| map(
    choose($family; $override) as $pick
    | select($pick != null)
    | {id: .identifier, ver: .version, dt: $pick.dt, rank: $pick.rank}
  )
# Ascending, so `last` is the lowest rank, then the newest version within it.
| sort_by([(0 - .rank), (.ver | split(".") | map(tonumber))])
| last
| if . == null then "null" else "\(.id) \(.ver) \(.dt.identifier) \(.dt.name)" end
