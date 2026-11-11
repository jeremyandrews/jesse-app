import Foundation
import XCTest

/// Screenshots of the Work segment against a running bridge, in the simulator.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_HO2_BRIDGE=host,port,token` (and the
/// rest below) on the `xcodebuild test` command line, so the gate never runs it. Point it at a
/// scratch bridge serving a COPY of a run store and of the vault's Strands notes, never at the
/// live one. Nothing here posts but the last look the board itself posts: no Send, no Start,
/// no Resume, no Cancel is pressed.
///
/// - `HO2_PROJECT`: the title of a project whose view has Your turn, Moving and Next.
/// - `HO2_RUN`: the id of a `needs_you` run.
/// - `HO2_BOARD_ONLY`: take only the board (a second pass over a different copy).
/// - `HO2_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
final class WorkScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["HO2_BRIDGE"] != nil else {
            throw XCTSkip("screenshots are taken only with HO2_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["HO2_SHOTS_DIR"] {
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

    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["HO2_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands"]
        app.launch()
        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        XCTAssertTrue(app.navigationBars.buttons["Runs"].waitForExistence(timeout: 30),
                      "the Runs toolbar button")
        return app
    }

    func testWorkScreens() throws {
        let prefix = env["HO2_PREFIX"] ?? "ios"
        let app = launch()

        // The whole Work segment as it opens, then with Since your last look expanded.
        let since = element(app, containing: "finished since your last look")
        XCTAssertTrue(since.waitForExistence(timeout: 30), "the Since your last look line")
        sleep(2)
        shot("\(prefix)-6-work-segment")
        since.tap()
        sleep(1)
        shot("\(prefix)-1-work-board-since-expanded")
        // The rows under the expanded line.
        app.swipeUp()
        app.swipeUp()
        sleep(1)
        shot("\(prefix)-1b-work-board-rows")
        if env["HO2_BOARD_ONLY"] != nil { return }
        app.swipeDown()
        app.swipeDown()
        app.swipeDown()

        // A project's view: Your turn, Moving and Next.
        let project = try XCTUnwrap(env["HO2_PROJECT"])
        since.tap()
        let row = element(app, beginningWith: "\(project), ")
        var tries = 0
        while !(row.exists && row.isHittable) && tries < 10 { app.swipeUp(); tries += 1 }
        XCTAssertTrue(row.waitForExistence(timeout: 30), "the row of \(project)")
        row.tap()
        XCTAssertTrue(app.staticTexts["Moving"].waitForExistence(timeout: 30)
                      || element(app, containing: "MOVING").waitForExistence(timeout: 5),
                      "the Moving section")
        sleep(2)
        shot("\(prefix)-2-project-view")
        app.swipeUp()
        sleep(1)
        shot("\(prefix)-2b-project-view-next")
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // The runs list of every run, from the Runs button, then a question's detail.
        let runsButton = app.navigationBars.buttons["Runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 10))
        runsButton.tap()
        let runID = try XCTUnwrap(env["HO2_RUN"])
        let runRow = element(app, containing: ", \(runID),")
        XCTAssertTrue(runRow.waitForExistence(timeout: 30), "the row of \(runID)")
        sleep(2)
        shot("\(prefix)-5-runs-list")
        runRow.tap()
        XCTAssertTrue(app.navigationBars["Run"].waitForExistence(timeout: 30),
                      "the detail of \(runID)")
        sleep(2)
        shot("\(prefix)-3-run-detail-question")
        let details = element(app, beginningWith: "Details")
        tries = 0
        while !(details.exists && details.isHittable) && tries < 8 { app.swipeUp(); tries += 1 }
        details.tap()
        sleep(1)
        app.swipeUp()
        sleep(1)
        shot("\(prefix)-4-run-detail-details")
    }
}
