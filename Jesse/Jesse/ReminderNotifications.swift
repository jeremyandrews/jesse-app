import Foundation
import UserNotifications
import JesseCore
import JesseNetworking

// TIMED REMINDERS: the bridge pushes `JESSE_REMINDER` at the moment a reminder falls due
// (`bridge/src/apns.rs`), and the notification carries two buttons — Done and Snooze 10
// min — that act on the reminder WITHOUT opening the app. Everything the app does with a
// reminder is in this file: the category, the parse, and the one handler that presses the
// button on the bridge or, when the bridge cannot be reached, holds the press on the
// offline capture queue for the next recovery to replay.
//
// Tapping the notification's body opens the app and does nothing else. A reminder push
// carries no `job_id` and no `conversation_id`, so there is no thread to land on, and
// `NotificationResponseRoute` says so explicitly rather than relying on that absence.

/// The reminder category and its two actions, as registered with the notification center.
nonisolated enum ReminderNotification {
    /// The `aps.category` the bridge sends on a reminder push.
    static let categoryIdentifier = "JESSE_REMINDER"
    static let doneActionIdentifier = "JESSE_REMINDER_DONE"
    static let snoozeActionIdentifier = "JESSE_REMINDER_SNOOZE"
    /// The payload key the bridge puts the reminder's id under.
    static let reminderIdKey = "reminder_id"
    /// What Snooze asks for. The bridge's default is the same, but the app says so out
    /// loud rather than leaning on it: the button's title promises ten minutes.
    static let snoozeMinutes = 10

    /// The category. NEITHER action is `.foreground`: pressing one runs in the background
    /// and the app never comes on screen, which is the point of a button on a notification.
    /// Neither is `.authenticationRequired` either — Done and Snooze act on a reminder the
    /// bridge itself scheduled, and asking for Face ID to snooze a pancake timer would make
    /// the button useless from the lock screen.
    static var category: UNNotificationCategory {
        UNNotificationCategory(
            identifier: categoryIdentifier,
            actions: [
                UNNotificationAction(identifier: doneActionIdentifier, title: "Done", options: []),
                UNNotificationAction(identifier: snoozeActionIdentifier,
                                     title: "Snooze \(snoozeMinutes) min", options: []),
            ],
            intentIdentifiers: [],
            options: [])
    }

    /// Register every category this app defines. `setNotificationCategories` REPLACES the
    /// set rather than adding to it, so this is the one place categories are registered.
    static func register(on center: UNUserNotificationCenter = .current()) {
        center.setNotificationCategories([category])
    }

    /// The reminder id a payload carries, trimmed; `nil` when there is none.
    static func reminderId(in userInfo: [AnyHashable: Any]) -> String? {
        guard let raw = (userInfo[reminderIdKey] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty else { return nil }
        return raw
    }
}

/// One press of a reminder button, parsed at the delegate's synchronous entry so the only
/// thing that crosses into the async work is a checked `Sendable` value.
nonisolated enum ReminderAction: Equatable, Sendable {
    case done(reminderId: String)
    case snooze(reminderId: String, minutes: Int)

    /// The press a notification response stands for, or `nil` when it is not a reminder
    /// button (or the payload carries no reminder id to act on).
    init?(actionIdentifier: String, userInfo: [AnyHashable: Any]) {
        guard let id = ReminderNotification.reminderId(in: userInfo) else { return nil }
        switch actionIdentifier {
        case ReminderNotification.doneActionIdentifier:
            self = .done(reminderId: id)
        case ReminderNotification.snoozeActionIdentifier:
            self = .snooze(reminderId: id, minutes: ReminderNotification.snoozeMinutes)
        default:
            return nil
        }
    }

    var reminderId: String {
        switch self {
        case .done(let id), .snooze(let id, _): return id
        }
    }

    /// The offline-queue row this press becomes when the bridge cannot be reached.
    func pendingRecord(now: Date) -> PendingIntentRecord {
        switch self {
        case .done(let id):
            return PendingIntentRecord(kind: .reminderDone, dayDate: "",
                                       payload: PendingIntentPayload(reminderId: id),
                                       createdAt: now)
        case .snooze(let id, let minutes):
            return PendingIntentRecord(kind: .reminderSnooze, dayDate: "",
                                       payload: PendingIntentPayload(reminderId: id,
                                                                     minutes: minutes),
                                       createdAt: now)
        }
    }
}

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
        if let action = ReminderAction(actionIdentifier: actionIdentifier, userInfo: userInfo) {
            self = .reminder(action)
            return
        }
        // A reminder by its category OR its id: either alone is enough to know this push
        // is not about a conversation, and the body tap must not go looking for one.
        if categoryIdentifier == ReminderNotification.categoryIdentifier
            || ReminderNotification.reminderId(in: userInfo) != nil {
            self = .openOnly
            return
        }
        if let tap = PushTap(userInfo: userInfo) {
            self = .thread(tap)
        } else {
            self = .openOnly
        }
    }
}

/// What became of one press.
nonisolated enum ReminderActionOutcome: Equatable, Sendable {
    /// The bridge took it.
    case applied
    /// The bridge answered with a final no (`404`: no such reminder; another `4xx`: a
    /// refusal). Nothing is queued — sending it again would earn the same answer.
    case dropped
    /// The bridge could not be reached (or answered `5xx`, or the pairing was refused):
    /// the press is on the offline capture queue, and the next recovery replays it.
    case queued
}

/// Presses a reminder button on the bridge, or holds the press for later.
///
/// The queue is the app's EXISTING offline capture queue (`PendingIntentStore`), the same
/// one an offline check-off or quick log waits on, and the replay is its existing replayer
/// (`IntentReplayer`) — so a held Done is retried on exactly the triggers a held check-off
/// is: the network path coming back, the app becoming active with a path, a fetch that
/// proves the bridge is back, and the background refresh task. No second queue and no
/// second retry loop.
@MainActor
final class ReminderActionHandler {
    nonisolated deinit {}

    static let shared = ReminderActionHandler(
        client: { JesseBridgeClient(config: ConfigStore.load()) },
        queue: { RootTabView.pendingStore })

    private let client: @MainActor () -> any ReminderActing
    private let queue: @MainActor () -> any PendingIntentStoring
    private let now: @MainActor () -> Date

    init(client: @escaping @MainActor () -> any ReminderActing,
         queue: @escaping @MainActor () -> any PendingIntentStoring,
         now: @escaping @MainActor () -> Date = { Date() }) {
        self.client = client
        self.queue = queue
        self.now = now
    }

    /// Press `action` on the bridge. Returns once the press has either landed, been
    /// refused for good, or been QUEUED — which is the moment the notification's
    /// completion handler may be called. Bounded by the request's own deadline
    /// (`ReminderActionResult.requestTimeout`), well inside the background window.
    @discardableResult
    func handle(_ action: ReminderAction) async -> ReminderActionOutcome {
        let bridge = client()
        let result: ReminderActionResult
        do {
            switch action {
            case .done(let id):
                result = try await bridge.reminderDone(id: id)
            case .snooze(let id, let minutes):
                result = try await bridge.reminderSnooze(id: id, minutes: minutes)
            }
        } catch {
            Log.push.error("reminder \(action.reminderId) action not delivered " +
                           "(\(error.localizedDescription)) — queued for the next recovery")
            queue().append(action.pendingRecord(now: now()))
            return .queued
        }
        switch result {
        case .applied:
            return .applied
        case .unknownReminder:
            Log.push.notice("reminder \(action.reminderId) is unknown to the bridge — dropped")
            return .dropped
        case .refused(let status, _):
            Log.push.error("reminder \(action.reminderId) action refused (\(status)) — dropped")
            return .dropped
        }
    }
}
