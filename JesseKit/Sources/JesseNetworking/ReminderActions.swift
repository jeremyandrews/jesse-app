import Foundation

// The app's half of the bridge's TIMED REMINDERS: the two buttons on a reminder's
// notification. The bridge owns the reminder (when it fires, what it says, what Done and
// Snooze do to it); the app only ever presses a button on one, by the id the push carried.

/// What the bridge said to a reminder action, for the answers that are FINAL.
///
/// The answers a retry could change are not cases here: a transport failure, a `5xx`, an
/// auth refusal and a rate limit all THROW, and the caller holds the action on the offline
/// queue. Everything that returns is a verdict, and none of them is worth sending again.
public enum ReminderActionResult: Equatable, Sendable {
    /// `2xx`: the bridge took it.
    case applied
    /// `404`: the bridge has no reminder by that id (already done, deleted, or a bridge
    /// that has never heard of it). Terminal by contract — the action is DROPPED.
    case unknownReminder
    /// Any other `4xx`: the bridge understood the request and refused it, in its own words.
    /// Terminal as well: sending the same bytes again earns the same answer.
    case refused(status: Int, message: String)

    /// The per-request deadline. A notification action runs in the background inside a
    /// window of a few seconds; a request that has not answered by then is better queued
    /// than waited on.
    public static let requestTimeout: TimeInterval = 15

    /// Map a status code, throwing for the ones that mean "try again later".
    ///
    /// `401` and `403` throw rather than refuse: they say the PAIRING is wrong, which a
    /// re-pair fixes, and a Done pressed before that is still a Done. `408` and `429` are
    /// a bridge asking to be asked again.
    public static func classify(status: Int, body: String) throws -> ReminderActionResult {
        switch status {
        case 200..<300:
            return .applied
        case 404:
            return .unknownReminder
        case 401, 403, 408, 429:
            throw JesseError.badResponse(status, body)
        case 400..<500:
            return .refused(status: status, message: body)
        default:
            throw JesseError.badResponse(status, body)
        }
    }
}

/// The two reminder calls. A seam so the notification handler and the offline queue can
/// be driven by a fake in a test; `JesseBridgeClient` is the one real conformer.
public protocol ReminderActing: Sendable {
    func reminderDone(id: String) async throws -> ReminderActionResult
    func reminderSnooze(id: String, minutes: Int) async throws -> ReminderActionResult
}

extension JesseBridgeClient: ReminderActing {}

/// `POST /jesse/reminders/{id}/snooze`'s body.
struct ReminderSnoozeBody: Encodable, Sendable {
    let minutes: Int
}
