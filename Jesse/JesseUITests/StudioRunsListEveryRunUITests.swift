import Foundation
import XCTest

/// The Studio runs list shows EVERY run the bridge lists, and a run's report opens from it.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_PD1_BRIDGE=host,port,token` and
/// `TEST_RUNNER_PD1_RUNS=<id>,<id>,…` (every run id `GET /jesse/runs` answers) on the
/// `xcodebuild test` command line, so the gate never runs it. Point it at a scratch bridge
/// serving a COPY of a run store, never at the live one: it presses nothing that posts.
///
/// - `PD1_REPORT_RUN`: a run with a report, opened at the end.
/// - `PD1_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
final class StudioRunsListEveryRunUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["PD1_BRIDGE"] != nil else {
            throw XCTSkip("taken only with PD1_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["PD1_SHOTS_DIR"] {
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    private func row(_ app: XCUIApplication, _ runID: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", "\(runID),")).firstMatch
    }

    func testEveryRunIsListedAndAReportOpens() throws {
        let ids = try XCTUnwrap(env["PD1_RUNS"]).split(separator: ",").map(String.init)
        let reportRun = try XCTUnwrap(env["PD1_REPORT_RUN"])

        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["PD1_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands"]
        app.launch()

        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        let runsButton = app.navigationBars.buttons["Studio runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")
        runsButton.tap()
        XCTAssertTrue(row(app, ids[0]).waitForExistence(timeout: 30), "the first row")
        sleep(1)

        // Walk the list top to bottom: every id must turn up as a row.
        var seen = Set<String>()
        var page = 1
        shot("pd1-ios-runs-list-\(page)")
        for _ in 0..<12 {
            for id in ids where !seen.contains(id) && row(app, id).exists { seen.insert(id) }
            if seen.count == ids.count { break }
            app.swipeUp(velocity: .slow)
            sleep(1)
            page += 1
            shot("pd1-ios-runs-list-\(page)")
        }
        let missing = ids.filter { !seen.contains($0) }
        XCTAssertTrue(missing.isEmpty, "rows missing from the Runs list: \(missing)")

        // One run's report, opened from its detail.
        var tries = 0
        while !(row(app, reportRun).exists && row(app, reportRun).isHittable) && tries < 12 {
            app.swipeDown(velocity: .slow)
            tries += 1
        }
        row(app, reportRun).tap()
        let report = app.buttons.matching(NSPredicate(format: "label CONTAINS %@",
                                                      "run-\(reportRun)")).firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 30), "the Report link of \(reportRun)")
        sleep(1)
        shot("pd1-ios-run-detail")
        report.tap()
        sleep(4)
        shot("pd1-ios-report-open")
    }
}
