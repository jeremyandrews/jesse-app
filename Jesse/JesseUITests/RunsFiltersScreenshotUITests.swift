import Foundation
import XCTest

/// Screenshots of the runs list's filter bar and archiving against a running bridge, in the
/// simulator.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_HO4_BRIDGE=host,port,token` (and the
/// rest below) on the `xcodebuild test` command line, so the gate never runs it. Point it at a
/// scratch bridge serving a COPY of a run store and of the vault's Strands notes and Inbox,
/// never at the live one: it archives one run and takes it back with Undo.
///
/// - `HO4_SWIPE_RUN`: the id of a finished run, swiped and archived.
/// - `HO4_PROJECT`: the title of a project whose view offers All runs.
/// - `HO4_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
final class RunsFiltersScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["HO4_BRIDGE"] != nil else {
            throw XCTSkip("screenshots are taken only with HO4_BRIDGE set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["HO4_SHOTS_DIR"] {
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

    private func segment(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", title)).firstMatch
    }

    private func pick(_ app: XCUIApplication, _ title: String) {
        let s = segment(app, title)
        XCTAssertTrue(s.waitForExistence(timeout: 10), "the \(title) segment")
        s.tap()
        sleep(2)
    }

    func testRunsFilterScreens() throws {
        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["HO4_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands"]
        app.launch()
        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        let runsButton = app.navigationBars.buttons["Runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")
        runsButton.tap()

        // Every filter, with real rows and counts.
        let swipeID = try XCTUnwrap(env["HO4_SWIPE_RUN"])
        XCTAssertTrue(segment(app, "All").waitForExistence(timeout: 30))
        pick(app, "All")
        sleep(1)
        shot("1-all")
        shot("0-runs-list-full-screen")
        pick(app, "Needs me")
        shot("2-needs-me")
        pick(app, "Moving")
        shot("3-moving")
        pick(app, "Finished")
        shot("4-finished")
        let archived = app.buttons["studio-runs-archived"].firstMatch
        XCTAssertTrue(archived.waitForExistence(timeout: 10))
        archived.tap()
        sleep(3)
        shot("5-archived")
        archived.tap()
        sleep(2)

        // Finished: Archive all finished asks first, naming the count. Nothing is archived.
        pick(app, "Finished")
        let all = app.buttons["Archive all finished"].firstMatch
        XCTAssertTrue(all.waitForExistence(timeout: 10))
        all.tap()
        sleep(1)
        shot("8-archive-all-finished-confirmation")
        // The cancel button is the tap outside the popover on an iPhone; nothing is archived.
        let keep = app.buttons["Keep them"].firstMatch
        if keep.exists && keep.isHittable {
            keep.tap()
        } else {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.75)).tap()
        }
        sleep(1)

        // A row mid swipe, its Archive showing.
        let row = element(app, containing: ", \(swipeID),")
        var tries = 0
        while !(row.exists && row.isHittable) && tries < 15 { app.swipeUp(); tries += 1 }
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the row of \(swipeID)")
        let from = row.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5))
        let to = row.coordinate(withNormalizedOffset: CGVector(dx: 0.55, dy: 0.5))
        from.press(forDuration: 0.1, thenDragTo: to, withVelocity: .slow,
                   thenHoldForDuration: 0.1)
        sleep(1)
        shot("6-row-mid-swipe-archive")

        // Archive: the row leaves at once, Undo shows; Undo puts it back.
        // A full swipe runs the row's first action, Archive.
        XCTAssertTrue(app.buttons["Archive"].firstMatch.exists, "the swipe's Archive")
        row.swipeLeft(velocity: .fast)
        let undo = app.buttons["studio-runs-undo"].firstMatch
        if !undo.waitForExistence(timeout: 5) { shot("debug-after-archive") }
        XCTAssertTrue(undo.exists, "the Undo banner")
        shot("7-undo-banner")
        undo.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 20), "back after Undo")
        sleep(2)

        // A finished run's detail, Archive in its toolbar.
        row.tap()
        XCTAssertTrue(app.buttons["studio-run-archive"].waitForExistence(timeout: 30),
                      "the detail's Archive")
        sleep(2)
        shot("9-run-detail-archive")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        sleep(1)
        app.navigationBars.buttons["Done"].tap()
        sleep(1)

        // A project's list, from its view's All runs, with the same bar.
        let project = try XCTUnwrap(env["HO4_PROJECT"])
        let projectRow = element(app, beginningWith: "\(project), ")
        tries = 0
        while !(projectRow.exists && projectRow.isHittable) && tries < 10 {
            app.swipeUp(); tries += 1
        }
        XCTAssertTrue(projectRow.waitForExistence(timeout: 30), "the row of \(project)")
        projectRow.tap()
        let allRuns = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'All runs'"))
            .firstMatch
        tries = 0
        while !(allRuns.exists && allRuns.isHittable) && tries < 10 { app.swipeUp(); tries += 1 }
        allRuns.tap()
        XCTAssertTrue(segment(app, "Needs me").waitForExistence(timeout: 30))
        pick(app, "All")
        sleep(2)
        shot("10-project-runs-list")
    }
}
