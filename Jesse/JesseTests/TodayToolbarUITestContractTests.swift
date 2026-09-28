import XCTest
@testable import Jesse
import JesseTodayDisplay

/// What `TodayToolbarUITests` (a UI-test target, which links no app code and so has to
/// hardcode them) depends on.
///
/// The UI test pins the tab's remembered view state with `-today.segment today` launch
/// arguments, because the segment is deliberately persisted per device: a run left on the
/// Strands board opened the next run there, where the day's sort menu and badge filter do
/// not exist, and all three toolbar tests failed on their anchor. Rename either key and the
/// argument lands in a domain nothing reads — the tests would go red again, with the same
/// misleading message, and nothing would say why.
@MainActor
final class TodayToolbarUITestContractTests: XCTestCase {

    func testTheSegmentKeyIsTheOneTheUITestPassesAsALaunchArgument() {
        XCTAssertEqual(TodayViewPreferences.segmentKey, "today.segment")
    }

    func testTheBadgeFilterKeyIsTheOneTheUITestPassesAsALaunchArgument() {
        XCTAssertEqual(TodayViewPreferences.badgeFilterKey, "today.badgeFilter")
    }

    /// And the values the arguments carry are the ones the store reads back: `today` is a
    /// `TodaySegment`, and the day is what an unset device opens on either way.
    func testTheArgumentValuesAreTheOnesTheStoreReads() {
        let defaults = UserDefaults(suiteName: "today-toolbar-contract")!
        defaults.removePersistentDomain(forName: "today-toolbar-contract")
        let preferences = TodayViewPreferences(defaults: defaults)
        XCTAssertEqual(preferences.segment, .today, "an unset device opens on the day")
        XCTAssertFalse(preferences.isBadgeFilterOn)

        defaults.set("today", forKey: TodayViewPreferences.segmentKey)
        defaults.set("NO", forKey: TodayViewPreferences.badgeFilterKey)
        XCTAssertEqual(preferences.segment, .today)
        XCTAssertFalse(preferences.isBadgeFilterOn, "`NO` reads as off, as a defaults bool")

        defaults.set(TodaySegment.strands.rawValue, forKey: TodayViewPreferences.segmentKey)
        XCTAssertEqual(preferences.segment, .strands,
                       "and the board is a real stored value, which is why it has to be pinned")
        defaults.removePersistentDomain(forName: "today-toolbar-contract")
    }
}
