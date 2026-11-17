import Foundation
import OSLog
import UserNotifications
import JesseCore

// TIMED REMINDERS, the half both apps share: the category with its Done and Snooze 10 min
// buttons, the parse of a pressed button, what a response to a reminder asks for, and the one
// handler that presses the button on the bridge. The iPhone and the Mac each own only their
// notification-center delegate and what they do when the bridge cannot be reached.
//
// The bridge pushes `JESSE_REMINDER` at the moment a reminder falls due (`bridge/src/apns.rs`).
// Tapping the notification's body opens the app and does nothing else: a reminder push carries
// no `job_id` and no `conversation_id`, so there is no thread to land on.

/// The reminder category and its two actions, as registered with the notification center.
public nonisolated enum ReminderNotification {
    /// The `aps.category` the bridge sends on a reminder push.
    public static let categoryIdentifier = "JESSE_REMINDER"
    public static let doneActionIdentifier = "JESSE_REMINDER_DONE"
    public static let snoozeActionIdentifier = "JESSE_REMINDER_SNOOZE"
    /// The payload key the bridge puts the reminder's id under.
    public static let reminderIdKey = "reminder_id"
    /// What Snooze asks for. The bridge's default is the same, but the app says so out
    /// loud rather than leaning on it: the button's title promises ten minutes.
    public static let snoozeMinutes = 10

    /// The category. NEITHER action is `.foreground`: pressing one runs in the background
    /// and the app never comes on screen, which is the point of a button on a notification.
    /// Neither is `.authenticationRequired` either — Done and Snooze act on a reminder the
    /// bridge itself scheduled, and asking for Face ID to snooze a pancake timer would make
    /// the button useless from the lock screen.
    public static var category: UNNotificationCategory {
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

    /// Register every category the iPhone app defines. `setNotificationCategories` REPLACES
    /// the set rather than adding to it, so this is the one place the iPhone registers
    /// categories. (The Mac has its own one place: `MacNotificationCategories`.)
    public static func register(on center: UNUserNotificationCenter = .current()) {
        center.setNotificationCategories([category])
    }

    /// The reminder id a payload carries, trimmed; `nil` when there is none.
    public static func reminderId(in userInfo: [AnyHashable: Any]) -> String? {
        guard let raw = (userInfo[reminderIdKey] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty else { return nil }
        return raw
    }
}

/// One press of a reminder button, parsed at the delegate's synchronous entry so the only
/// thing that crosses into the async work is a checked `Sendable` value.
public nonisolated enum ReminderAction: Equatable, Sendable {
    case done(reminderId: String)
    case snooze(reminderId: String, minutes: Int)

    /// The press a notification response stands for, or `nil` when it is not a reminder
    /// button (or the payload carries no reminder id to act on).
    public init?(actionIdentifier: String, userInfo: [AnyHashable: Any]) {
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

    public var reminderId: String {
        switch self {
        case .done(let id), .snooze(let id, _): return id
        }
    }

    /// The offline-queue row this press becomes when the bridge cannot be reached.
    public func pendingRecord(now: Date) -> PendingIntentRecord {
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

/// What a notification response asks for, as far as reminders go. Pure, so the routing is
/// testable without a `UNNotificationResponse` (which has no public initializer). Each app
/// maps it onto its own route; `nil` means "not a reminder", and the app keeps whatever it
/// does with every other response.
public nonisolated enum ReminderResponse: Equatable, Sendable {
    /// A reminder button: press it on the bridge, in the background.
    case press(ReminderAction)
    /// A reminder's body was tapped: the app opens, and nothing else happens.
    case body

    public init?(actionIdentifier: String, categoryIdentifier: String,
                 userInfo: [AnyHashable: Any]) {
        if let action = ReminderAction(actionIdentifier: actionIdentifier, userInfo: userInfo) {
            self = .press(action)
            return
        }
        // A reminder by its category OR its id: either alone is enough to know this push
        // is not about a conversation, and the body tap must not go looking for one.
        if categoryIdentifier == ReminderNotification.categoryIdentifier
            || ReminderNotification.reminderId(in: userInfo) != nil {
            self = .body
            return
        }
        return nil
    }
}

/// What became of one press.
public nonisolated enum ReminderActionOutcome: Equatable, Sendable {
    /// The bridge took it.
    case applied
    /// The bridge answered with a final no (`404`: no such reminder; another `4xx`: a
    /// refusal). Nothing is queued — sending it again would earn the same answer.
    case dropped
    /// The bridge could not be reached (or answered `5xx`, or the pairing was refused):
    /// the press is on the offline capture queue, and the next recovery replays it.
    case queued
    /// The bridge could not be reached, on an app with no offline capture queue (the Mac):
    /// the press was retried once after a short delay, failed again, and was logged and
    /// let go. The reminder is untouched on the bridge and fires again on its own terms.
    case lost
}

/// What the handler does with a press the bridge could not be reached for.
public enum ReminderUndeliveredPolicy {
    /// Hold it on the app's EXISTING offline capture queue (`PendingIntentStore`), for its
    /// existing replayer (`IntentReplayer`) to send. The iPhone.
    case queue(@MainActor () -> any PendingIntentStoring)
    /// Wait `delay`, try once more, and if that fails too, log and drop. For an app with
    /// no offline queue (the Mac), which must not grow a second queue system for this.
    case retryOnce(after: Duration, sleep: @Sendable (Duration) async -> Void)

    /// The Mac's: one retry five seconds on, long enough for a Wi-Fi blip or a bridge
    /// restart, short enough that nobody is left wondering whether the press took.
    public static func retryOnce(after delay: Duration = .seconds(5)) -> ReminderUndeliveredPolicy {
        .retryOnce(after: delay, sleep: { try? await Task.sleep(for: $0) })
    }
}

private let reminderLog = Logger(subsystem: "com.tag1.jesse", category: "push")

/// Presses a reminder button on the bridge, and on failure does what its
/// `ReminderUndeliveredPolicy` says.
@MainActor
public final class ReminderActionHandler {
    nonisolated deinit {}

    private let client: @MainActor () -> any ReminderActing
    private let undelivered: ReminderUndeliveredPolicy
    private let now: @MainActor () -> Date

    public init(client: @escaping @MainActor () -> any ReminderActing,
                undelivered: ReminderUndeliveredPolicy,
                now: @escaping @MainActor () -> Date = { Date() }) {
        self.client = client
        self.undelivered = undelivered
        self.now = now
    }

    /// The iPhone's: a press the bridge cannot take waits on the offline capture queue.
    public convenience init(client: @escaping @MainActor () -> any ReminderActing,
                            queue: @escaping @MainActor () -> any PendingIntentStoring,
                            now: @escaping @MainActor () -> Date = { Date() }) {
        self.init(client: client, undelivered: .queue(queue), now: now)
    }

    /// Press `action` on the bridge. Returns once the press has either landed, been
    /// refused for good, been QUEUED, or (with no queue) been retried and let go — which is
    /// the moment the notification's completion handler may be called. Each attempt is
    /// bounded by the request's own deadline (`ReminderActionResult.requestTimeout`).
    @discardableResult
    public func handle(_ action: ReminderAction) async -> ReminderActionOutcome {
        let first: ReminderActionResult
        do {
            first = try await press(action)
        } catch {
            switch undelivered {
            case .queue(let queue):
                reminderLog.error("reminder \(action.reminderId, privacy: .public) action not delivered (\(error.localizedDescription, privacy: .public)) — queued for the next recovery")
                queue().append(action.pendingRecord(now: now()))
                return .queued
            case .retryOnce(let delay, let sleep):
                reminderLog.notice("reminder \(action.reminderId, privacy: .public) action not delivered (\(error.localizedDescription, privacy: .public)) — retrying once")
                await sleep(delay)
                do {
                    return verdict(try await press(action), for: action)
                } catch {
                    reminderLog.error("reminder \(action.reminderId, privacy: .public) action not delivered on retry (\(error.localizedDescription, privacy: .public)) — dropped")
                    return .lost
                }
            }
        }
        return verdict(first, for: action)
    }

    private func press(_ action: ReminderAction) async throws -> ReminderActionResult {
        let bridge = client()
        switch action {
        case .done(let id):
            return try await bridge.reminderDone(id: id)
        case .snooze(let id, let minutes):
            return try await bridge.reminderSnooze(id: id, minutes: minutes)
        }
    }

    private func verdict(_ result: ReminderActionResult,
                         for action: ReminderAction) -> ReminderActionOutcome {
        switch result {
        case .applied:
            return .applied
        case .unknownReminder:
            reminderLog.notice("reminder \(action.reminderId, privacy: .public) is unknown to the bridge — dropped")
            return .dropped
        case .refused(let status, _):
            reminderLog.error("reminder \(action.reminderId, privacy: .public) action refused (\(status)) — dropped")
            return .dropped
        }
    }
}
