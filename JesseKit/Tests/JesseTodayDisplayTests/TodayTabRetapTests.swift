import XCTest
@testable import JesseTodayDisplay

// A TAP ON THE TODAY TAB BAR ITEM WHILE THE TAB IS UP RETURNS TO THE DAY.
//
// The bug this pins: the iPhone shell had no notion of a re-tap, so with the Strands
// segment showing, tapping Today in the tab bar did nothing and the only way back to the
// day was the segmented picker. The rule is three cases, one test each, over a stand-in
// for the shell's tab type (the package does not know the shell's tabs).
final class TodayTabRetapTests: XCTestCase {

    private enum Tab: CaseIterable { case chats, today, health, vault }

    private func segment(from selected: Tab, tapping tapped: Tab,
                         showing: TodaySegment) -> TodaySegment? {
        TodayTabRetap.segment(selected: selected, tapped: tapped, todayTab: Tab.today,
                              showing: showing)
    }

    /// Behaviour 1: the first tap on Today, from any other tab, keeps the remembered
    /// segment, whichever one it is.
    func testSelectingTheTodayTabKeepsTheRememberedSegment() {
        for other in Tab.allCases where other != .today {
            for showing in TodaySegment.allCases {
                XCTAssertNil(segment(from: other, tapping: .today, showing: showing),
                             "from \(other) with \(showing) remembered")
            }
        }
    }

    /// Behaviour 2: a re-tap with Strands showing puts the day up. With behaviour 1, two
    /// taps from another tab land on the day.
    func testReTappingTodayWithStrandsShowingReturnsToTheDay() {
        XCTAssertEqual(segment(from: .today, tapping: .today, showing: .strands), .today)

        let first = segment(from: .chats, tapping: .today, showing: .strands)
        XCTAssertNil(first, "the first tap only selects the tab")
        let second = segment(from: .today, tapping: .today, showing: first ?? .strands)
        XCTAssertEqual(second, .today, "the second tap lands on the day")
    }

    /// Behaviour 3: a re-tap with the day already showing changes no segment.
    func testReTappingTodayWithTheDayShowingChangesNothing() {
        XCTAssertNil(segment(from: .today, tapping: .today, showing: .today))
    }

    /// Taps on the other tabs never touch the Today tab's segment, re-taps included.
    func testOtherTabsNeverChangeTheSegment() {
        for selected in Tab.allCases {
            for tapped in Tab.allCases where tapped != .today {
                for showing in TodaySegment.allCases {
                    XCTAssertNil(segment(from: selected, tapping: tapped, showing: showing),
                                 "\(tapped) tapped from \(selected) with \(showing) showing")
                }
            }
        }
    }
}
