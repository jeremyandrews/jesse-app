import Foundation
import XCTest

/// Screenshots of the Today tab re-tap against a bridge serving a day and a strand board.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_TT1_BRIDGE=host,port,token` on the
/// `xcodebuild test` command line, so the gate never runs it. Point it at a stub serving
/// synthetic data (the TT1 shots used a loopback stub serving `today-full.json`, the
/// bridge's own serialization of its synthetic test day, and an invented board). It ticks
/// and posts nothing.
///
/// - `TT1_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
/// - `TT1_DAY_ITEM`: the lead of an item on the day, used to know the day has drawn.
/// - `TT1_STRAND`: the title of a strand on the board, used to know the board has drawn.
final class TodayRetapScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["TT1_BRIDGE"] != nil else {
            throw XCTSkip("screenshots are taken only with TT1_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["TT1_SHOTS_DIR"] {
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    private func element(_ app: XCUIApplication, containing text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    private func waitSelected(_ element: XCUIElement, _ message: String) {
        let wait = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isSelected == true"),
                                             object: element)
        XCTAssertEqual(XCTWaiter().wait(for: [wait], timeout: 10), .completed, message)
    }

    func testTheReTapReturnsFromTheBoardToTheDay() throws {
        let dayItem = try XCTUnwrap(env["TT1_DAY_ITEM"])
        let strand = try XCTUnwrap(env["TT1_STRAND"])

        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["TT1_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launch()

        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()

        // The system's own re-tap behaviour is kept: with the day showing, open an item,
        // then re-tap. The pushed detail goes and the day is still the segment showing.
        let todaySegment = app.segmentedControls.buttons["Today"]
        XCTAssertTrue(todaySegment.waitForExistence(timeout: 30))
        todaySegment.tap()
        let row = element(app, containing: dayItem)
        XCTAssertTrue(row.waitForExistence(timeout: 30), "the day has drawn")
        row.tap()
        XCTAssertTrue(app.navigationBars["Item"].waitForExistence(timeout: 10), "item pushed")
        today.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the re-tap popped back to the day")
        XCTAssertFalse(app.navigationBars["Item"].exists, "the detail is gone")
        waitSelected(todaySegment, "the day is still showing")

        // The board, then away to Chats and back: the first tap keeps the board.
        let strandsSegment = app.segmentedControls.buttons["Strands"]
        strandsSegment.tap()
        XCTAssertTrue(element(app, containing: strand).waitForExistence(timeout: 30),
                      "the board has drawn")
        app.tabBars.buttons["Chats"].tap()
        today.tap()
        waitSelected(strandsSegment, "the first tap keeps the board")
        XCTAssertTrue(element(app, containing: strand).waitForExistence(timeout: 30))
        shot("1-strands-before-retap")

        // The re-tap.
        today.tap()
        waitSelected(todaySegment, "the re-tap returns to the day")
        XCTAssertTrue(element(app, containing: dayItem).waitForExistence(timeout: 30))
        shot("2-today-after-retap")
    }
}
