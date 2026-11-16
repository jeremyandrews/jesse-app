import Foundation
import UIKit
import UserNotifications

/// The UI-test seam for the reminder notification's screenshots, and nothing else.
///
/// The app asks for notification permission only after a turn succeeds against a paired
/// bridge, and the simulator has no `simctl privacy` service for notifications — so a UI
/// test that wants to SEE a delivered reminder, with its Done and Snooze buttons, has no
/// way to get the permission granted. This asks at launch instead, so the test can accept
/// the system prompt, and registers for remote notifications as the grant path does.
///
/// `JESSE_UITEST_NOTIFICATIONS=1` arms it. ABSENT, which is every ordinary launch, it does
/// nothing. Compiled out of Release, and a launch environment is set only by a debugger,
/// an XCTest runner or `simctl launch`: the same terms as `ThreadLandingUITestSeam`.
@MainActor
enum ReminderNotificationUITestSeam {

    static let isArmed: Bool = {
        #if DEBUG
        return ProcessInfo.processInfo.environment["JESSE_UITEST_NOTIFICATIONS"] != nil
        #else
        return false
        #endif
    }()

    /// Ask for permission now, if armed.
    static func arm() {
        guard isArmed else { return }
        UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
                guard granted else { return }
                Task { @MainActor in UIApplication.shared.registerForRemoteNotifications() }
            }
    }
}
