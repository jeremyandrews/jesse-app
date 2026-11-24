import Foundation
import XCTest

/// DJ12: a run awaiting its owner's merge, in the simulator: its row under Needs me with the
/// pull request link, its detail with the link, the project's Your turn, and the whole detail
/// with Details open.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_DJ12_BRIDGE=host,port,token` (and the
/// rest below) on the `xcodebuild test` command line, so the gate never runs it. Point it at a
/// scratch bridge (or a stand-in serving the wire fixtures) that lists an `awaiting_merge` run.
/// Nothing here posts but the last look the board itself posts, and the link is never tapped.
///
/// - `DJ12_RUN`: the awaiting-merge run's id.
/// - `DJ12_PROJECT`: the title of its project.
/// - `DJ12_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
final class MergeWaitScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["DJ12_BRIDGE"] != nil else {
            throw XCTSkip("screenshots are taken only with DJ12_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["DJ12_SHOTS_DIR"] {
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

    func testMergeWaitScreens() throws {
        let runID = try XCTUnwrap(env["DJ12_RUN"])
        let project = try XCTUnwrap(env["DJ12_PROJECT"])
        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["DJ12_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands",
                                "-studioRunsFilter", "needsMe"]
        app.launch()
        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()

        // The project's view: Your turn lists the run with its pull request link.
        let projectRow = element(app, beginningWith: "\(project), ")
        var tries = 0
        while !(projectRow.exists && projectRow.isHittable) && tries < 10 {
            app.swipeUp(); tries += 1
        }
        XCTAssertTrue(projectRow.waitForExistence(timeout: 30), "the row of \(project)")
        projectRow.tap()
        let projectLink = app.descendants(matching: .any)["studio-run-merge-link"].firstMatch
        tries = 0
        while !(projectLink.exists && projectLink.isHittable) && tries < 6 {
            app.swipeUp(); tries += 1
        }
        XCTAssertTrue(projectLink.waitForExistence(timeout: 30), "the Your turn link")
        sleep(2)
        shot("dj12-ios-1-project-your-turn")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        sleep(1)

        // The Runs list under Needs me: the awaiting-merge row and its link.
        let runsButton = app.navigationBars.buttons["Runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")
        runsButton.tap()
        let row = element(app, containing: ", \(runID),")
        XCTAssertTrue(row.waitForExistence(timeout: 30), "the row of \(runID)")
        XCTAssertTrue(row.label.contains("Awaiting your merge"), row.label)
        sleep(2)
        shot("dj12-ios-2-runs-needs-me")

        // The detail: tapped on the row's title, above the link, so the row opens rather than
        // the pull request.
        row.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.12)).tap()
        let detailLink = app.descendants(matching: .any)["studio-run-merge-link"].firstMatch
        XCTAssertTrue(detailLink.waitForExistence(timeout: 30), "the detail's link")
        XCTAssertTrue(detailLink.label.contains("#"), detailLink.label)
        sleep(2)
        shot("dj12-ios-3-run-detail")

        // The whole page: Details open, scrolled down through it.
        let details = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Details'"))
            .firstMatch
        tries = 0
        while !(details.exists && details.isHittable) && tries < 6 { app.swipeUp(); tries += 1 }
        XCTAssertTrue(details.exists, "the Details disclosure")
        details.tap()
        sleep(1)
        app.swipeUp()
        sleep(1)
        shot("dj12-ios-4-run-detail-details")
    }
}
