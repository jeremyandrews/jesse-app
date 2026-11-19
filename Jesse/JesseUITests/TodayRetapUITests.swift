import XCTest

/// A tap on the Today tab bar item while the Today tab is up, driven through the real app.
///
/// The sibling of `TodayToolbarUITests`. The rule is `TodayTabRetap`'s and is unit tested in
/// the package; what only a UI test can show is that the TAB BAR delivers it: that the shell
/// sees a re-tap at all, that it does not mistake the first tap for one, and that the
/// segmented picker on screen follows.
///
/// The stored segment is set THROUGH THE PICKER rather than with a `-today.segment` launch
/// argument. An argument lands in `NSArgumentDomain`, which `UserDefaults` reads before
/// anything the app writes, so a launch pinned to `strands` could never show the day again
/// whatever the re-tap did. Nothing here needs a paired bridge: the picker and both
/// segments' toolbars draw whether or not a day or a board ever loads.
final class TodayRetapUITests: XCTestCase {

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    private func todayTabButton(_ app: XCUIApplication) -> XCUIElement {
        app.tabBars.buttons["Today"]
    }

    private func segment(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.segmentedControls.buttons[label]
    }

    /// The Today tab's picker shows `label`, and the toolbar is that segment's: the day's
    /// sort menu, or the board's Runs button.
    private func assertShowing(_ app: XCUIApplication, _ label: String,
                               _ message: String, line: UInt = #line) {
        let selected = segment(app, label)
        XCTAssertTrue(selected.waitForExistence(timeout: 30), "the \(label) segment button",
                      line: line)
        let settled = NSPredicate(format: "isSelected == true")
        let wait = XCTNSPredicateExpectation(predicate: settled, object: selected)
        XCTAssertEqual(XCTWaiter().wait(for: [wait], timeout: 10), .completed, message,
                       line: line)
        let anchor = label == "Today"
            ? app.navigationBars.buttons["Order every section: File order"]
            : app.navigationBars.buttons["Runs"]
        XCTAssertTrue(anchor.waitForExistence(timeout: 10),
                      "\(message): the \(label) segment's toolbar is up", line: line)
    }

    /// Launch (on Chats, the default tab), open Today and pick `label` in the segmented
    /// picker, which stores it exactly as a person picking it would.
    private func launchAndPick(_ label: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launch()
        let today = todayTabButton(app)
        XCTAssertTrue(today.waitForExistence(timeout: 30), "Today tab button")
        today.tap()
        let choice = segment(app, label)
        XCTAssertTrue(choice.waitForExistence(timeout: 30), "the \(label) segment button")
        choice.tap()
        assertShowing(app, label, "\(label) picked")
        return app
    }

    /// Behaviours 1 and 2: from Chats with Strands remembered, the first tap on Today keeps
    /// Strands showing and the second lands on the day.
    func testTwoTapsFromChatsWithStrandsRememberedLandOnTheDay() {
        let app = launchAndPick("Work")

        let chats = app.tabBars.buttons["Chats"]
        XCTAssertTrue(chats.waitForExistence(timeout: 10))
        chats.tap()
        XCTAssertTrue(chats.isSelected, "on Chats")

        todayTabButton(app).tap()
        assertShowing(app, "Work", "the first tap keeps the remembered segment")

        todayTabButton(app).tap()
        assertShowing(app, "Today", "the re-tap returns to the day")
    }

    /// Behaviour 3: already on Today with the day showing, a re-tap changes no segment.
    func testReTappingTodayWithTheDayShowingChangesNoSegment() {
        let app = launchAndPick("Today")

        todayTabButton(app).tap()
        assertShowing(app, "Today", "a re-tap on the day stays on the day")
        XCTAssertFalse(segment(app, "Work").isSelected, "Work was not selected")
    }
}
