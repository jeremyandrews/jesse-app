import Foundation
import UserNotifications
import JesseCore
import JesseNetworking

// TIMED REMINDERS on the iPhone. The category, the parse of a pressed button, what a response
// to a reminder asks for and the handler that presses it on the bridge are shared with the Mac
// and live in JesseKit (`JesseNetworking/ReminderNotification.swift`). What is the iPhone's own
// is here: how a response maps onto a route that includes a "Jesse finished" thread tap, and
// where a press the bridge cannot be reached for waits — the offline capture queue, for the
// next recovery to replay.
//
// Tapping the notification's body opens the app and does nothing else. A reminder push carries
// no `job_id` and no `conversation_id`, so there is no thread to land on, and
// `NotificationResponseRoute` says so explicitly rather than relying on that absence.

/// What a notification response asks the app to do. Pure, so the routing is testable
/// without a `UNNotificationResponse` (which has no public initializer).
nonisolated enum NotificationResponseRoute: Equatable, Sendable {
    /// A reminder button: press it on the bridge, in the background.
    case reminder(ReminderAction)
    /// A reminder's body was tapped: the app opens, and nothing else happens. No thread is
    /// routed to, because a reminder belongs to no conversation.
    case openOnly
    /// An ordinary "Jesse finished" tap: land on its thread.
    case thread(PushTap)

    init(actionIdentifier: String, categoryIdentifier: String, userInfo: [AnyHashable: Any]) {
        switch ReminderResponse(actionIdentifier: actionIdentifier,
                                categoryIdentifier: categoryIdentifier, userInfo: userInfo) {
        case .press(let action):
            self = .reminder(action)
        case .body:
            self = .openOnly
        case nil:
            if let tap = PushTap(userInfo: userInfo) {
                self = .thread(tap)
            } else {
                self = .openOnly
            }
        }
    }
}

/// The iPhone's handler. The queue is the app's EXISTING offline capture queue
/// (`PendingIntentStore`), the same one an offline check-off or quick log waits on, and the
/// replay is its existing replayer (`IntentReplayer`) — so a held Done is retried on exactly
/// the triggers a held check-off is: the network path coming back, the app becoming active
/// with a path, a fetch that proves the bridge is back, and the background refresh task. No
/// second queue and no second retry loop.
extension ReminderActionHandler {
    static let shared = ReminderActionHandler(
        client: { JesseBridgeClient(config: ConfigStore.load()) },
        queue: { RootTabView.pendingStore })
}
