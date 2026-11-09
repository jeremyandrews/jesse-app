import Foundation
import XCTest

/// Screenshots of the Studio run screens against a running bridge, in the simulator.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_HO1B_BRIDGE=host,port,token` (and the
/// rest below) on the `xcodebuild test` command line, so the gate never runs it. Point it at a
/// scratch bridge serving a COPY of a run store, never at the live one: the test opens the
/// Cancel confirmation and backs out of it, and presses nothing that posts.
///
/// - `HO1B_STRAND`: the title of a strand with active runs and a runnable Drafts line.
/// - `HO1B_DRAFT`: that line's id, which has no live run.
/// - `HO1B_RUN_REPLY`: a `needs_you` or `interrupted` run.
/// - `HO1B_RUN_CANCEL`: a `queued`, `running` or `paused` run.
/// - `HO1B_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
final class StudioRunsScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["HO1B_BRIDGE"] != nil else {
            throw XCTSkip("screenshots are taken only with HO1B_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["HO1B_SHOTS_DIR"] {
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    private func element(_ app: XCUIApplication, containing text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    private func element(_ app: XCUIApplication, beginningWith text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", text)).firstMatch
    }

    func testStudioRunScreens() throws {
        let strand = try XCTUnwrap(env["HO1B_STRAND"])
        let draft = try XCTUnwrap(env["HO1B_DRAFT"])
        let replyRun = try XCTUnwrap(env["HO1B_RUN_REPLY"])
        let cancelRun = try XCTUnwrap(env["HO1B_RUN_CANCEL"])

        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["HO1B_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands"]
        app.launch()

        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        let runsButton = app.navigationBars.buttons["Studio runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")

        // The board, with the strand's badge.
        let row = element(app, containing: ", \(strand), ")
        XCTAssertTrue(row.waitForExistence(timeout: 30), "the strand row")
        // A row whose spoken label carries a badge (`2 Studio runs, 2 need you`), not the
        // toolbar's own `Studio runs` button.
        let badged = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label MATCHES %@", ".*, [0-9]+ Studio runs?.*"))
            .firstMatch
        shot("1-strands-segment")
        // A List draws only the rows on screen, so the badged one may not exist yet.
        var scrolls = 0
        while !(badged.exists && badged.isHittable && badged.frame.maxY < app.frame.maxY * 0.8)
                && scrolls < 15 {
            app.swipeUp(velocity: .slow)
            scrolls += 1
        }
        XCTAssertTrue(badged.exists, "a run badge on the board")
        shot("2-board-badge")
        // Back to the top, where the strand with the Drafts line is.
        while !row.isHittable && scrolls > 0 {
            app.swipeDown(velocity: .slow)
            scrolls -= 1
        }
        if !row.isHittable { app.swipeDown() }

        // The strand's Drafts line, long pressed.
        row.tap()
        let line = element(app, beginningWith: "\(draft) ")
        XCTAssertTrue(line.waitForExistence(timeout: 30), "the Drafts line \(draft)")
        line.press(forDuration: 1.2)
        XCTAssertTrue(app.buttons["Run on Studio"].waitForExistence(timeout: 10))
        shot("3-drafts-run-on-studio")
        // Back out of the menu without starting anything, then close the note.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)).tap()
        sleep(1)
        if app.buttons["Run on Studio"].exists {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.2)).tap()
            sleep(1)
        }
        let done = app.buttons["Done"].firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 10))
        done.tap()

        // The Runs list.
        XCTAssertTrue(runsButton.waitForExistence(timeout: 10))
        runsButton.tap()
        let replyRow = element(app, beginningWith: "\(replyRun),")
        XCTAssertTrue(replyRow.waitForExistence(timeout: 30), "the row of \(replyRun)")
        sleep(1)
        shot("4-runs-list")

        // A run's detail with its log tail, then its Reply composer.
        replyRow.tap()
        XCTAssertTrue(element(app, beginningWith: "Branch").waitForExistence(timeout: 30),
                      "the detail of \(replyRun)")
        sleep(2)
        shot("5-run-detail-log")
        let composer = app.textViews["Reply to \(replyRun)"].exists
            ? app.textViews["Reply to \(replyRun)"] : app.textFields["Reply to \(replyRun)"]
        var tries = 0
        while !composer.isHittable && tries < 6 { app.swipeUp(); tries += 1 }
        sleep(1)
        shot("6-reply-composer")
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // The Cancel confirmation, backed out of.
        let cancelRow = element(app, beginningWith: "\(cancelRun),")
        XCTAssertTrue(cancelRow.waitForExistence(timeout: 30), "the row of \(cancelRun)")
        cancelRow.tap()
        let cancel = app.buttons["Cancel run"]
        tries = 0
        while !(cancel.exists && cancel.isHittable) && tries < 6 { app.swipeUp(); tries += 1 }
        XCTAssertTrue(cancel.waitForExistence(timeout: 30))
        cancel.tap()
        let confirm = app.buttons["Cancel \(cancelRun)"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 10))
        shot("7-cancel-confirmation")
        // Backed out of, never confirmed. Where the dialog shows no cancel button (a popover
        // on iOS 27), a tap outside it is the way out.
        let keep = app.buttons["Keep running"]
        if keep.exists && keep.isHittable {
            keep.tap()
        } else {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.12)).tap()
        }
        XCTAssertTrue(confirm.waitForNonExistence(timeout: 10))
    }
}
