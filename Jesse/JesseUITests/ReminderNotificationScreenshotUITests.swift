import Foundation
import XCTest

/// Screenshots of a delivered timed reminder, in the simulator: the notification as it
/// arrives, and long pressed to show its Done and Snooze 10 min buttons.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_RM1_SHOTS_DIR=<dir>` on the
/// `xcodebuild test` command line, so the gate never runs it. The PUSH comes from the host:
/// once the app has launched (registering the reminder category) and permission is
/// granted, the test writes `<dir>/ready` and waits; the host then runs
/// `xcrun simctl push <udid> com.tag1.Jesse <payload.json>` with the bridge's reminder
/// payload. Nothing here presses either button.
///
/// - `RM1_SHOTS_DIR`: where the PNGs (and the `ready` marker) are written. The PNGs are
///   attached to the result too.
final class ReminderNotificationScreenshotUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: URL? { env["RM1_SHOTS_DIR"].map { URL(fileURLWithPath: $0) } }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        guard env["RM1_SHOTS_DIR"] != nil else {
            throw XCTSkip("screenshots are taken only with RM1_SHOTS_DIR set")
        }
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir {
            try? png.write(to: dir.appendingPathComponent("\(name).png"))
        }
    }

    func testAReminderNotificationAndItsActions() throws {
        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_NOTIFICATIONS"] = "1"
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launch()

        // The permission prompt the seam raises belongs to springboard, not the app.
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allow = springboard.alerts.buttons["Allow"]
        if allow.waitForExistence(timeout: 20) { allow.tap() }
        sleep(2)

        // Home, so the reminder arrives as it would in a pocket: a banner over springboard.
        XCUIDevice.shared.press(.home)
        sleep(1)
        if app.state == .runningForeground { springboard.activate() }
        _ = app.wait(for: .runningBackground, timeout: 10)
        sleep(1)
        if let dir {
            try Data("ready".utf8).write(to: dir.appendingPathComponent("ready"))
        }

        let reminder = springboard.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", "start the pancakes")).firstMatch
        XCTAssertTrue(reminder.waitForExistence(timeout: 300), "the reminder banner")
        sleep(1)
        shot("1-reminder-collapsed")

        // Long press — the banner if it is still up, otherwise the same notification in
        // Notification Center.
        if !(reminder.exists && reminder.isHittable) {
            let top = springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.005))
            top.press(forDuration: 0.1,
                      thenDragTo: springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.7)))
            XCTAssertTrue(reminder.waitForExistence(timeout: 10), "the reminder in Notification Center")
        }
        reminder.press(forDuration: 1.5)
        let snooze = springboard.buttons["Snooze 10 min"]
        XCTAssertTrue(snooze.waitForExistence(timeout: 10), "the Snooze 10 min action")
        XCTAssertTrue(springboard.buttons["Done"].exists, "the Done action")
        sleep(1)
        shot("2-reminder-actions")

        // Out of the expanded view without pressing either button.
        XCUIDevice.shared.press(.home)
    }
}
