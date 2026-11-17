import AppKit
import Foundation
import JesseNetworking
import UserNotifications

// TIMED REMINDERS on the Mac: the bridge pushes `JESSE_REMINDER` to every registered device,
// this Mac included (`MacPush.swift`). Until this file the Mac had no notification-center
// delegate and registered no categories, so a reminder arrived with no buttons. The category,
// the parse and the handler are the iPhone's own, from JesseKit
// (`JesseNetworking/ReminderNotification.swift`); what is the Mac's is here.
//
// * Done and Snooze 10 min are pressed on the bridge in the background; the app is not
//   brought forward.
// * A reminder's body opens the app and does nothing else.
// * Every other response — the Mac's own "Jesse replied" banners, any other push — does what
//   it did before there was a delegate: the app comes forward.
//
// The Mac has NO offline capture queue (no `PendingIntentStore`, no `IntentReplayer`), and a
// second queue system is not built for this. A press the bridge cannot take is retried once
// a few seconds later, then logged and dropped (`ReminderUndeliveredPolicy.retryOnce`). The
// reminder stays as it was on the bridge; the iPhone's notification can still act on it.

/// Every category the Mac defines. `setNotificationCategories` REPLACES the set rather than
/// adding to it, so this is the ONE place the Mac registers categories: a new one goes in
/// `all`, never in a second `setNotificationCategories` call somewhere else.
enum MacNotificationCategories {
    /// The Mac's own local "Jesse replied" banners carry no category, so today this is the
    /// reminder's alone.
    static var all: Set<UNNotificationCategory> { [ReminderNotification.category] }

    static func register(on center: any MacNotificationCentering = UNUserNotificationCenter.current()) {
        center.setNotificationCategories(all)
    }
}

/// The two things the Mac asks of the notification center at launch. A seam because an
/// unsigned build (the local gate's test host) cannot register categories with the system at
/// all, so a test checks what is ASKED of the center rather than what it reads back.
protocol MacNotificationCentering: AnyObject {
    var delegate: (any UNUserNotificationCenterDelegate)? { get set }
    func setNotificationCategories(_ categories: Set<UNNotificationCategory>)
}

extension UNUserNotificationCenter: MacNotificationCentering {}

/// What a notification response asks the Mac to do. Pure, so the routing is testable
/// without a `UNNotificationResponse` (which has no public initializer).
nonisolated enum MacNotificationRoute: Equatable, Sendable {
    /// A reminder button: press it on the bridge, in the background. Nothing comes forward.
    case reminder(ReminderAction)
    /// A reminder's body: the app comes forward, and nothing else happens.
    case openOnly
    /// Anything else (the Mac's own turn banners, any other push): what the Mac did before it
    /// had a delegate, which is to come forward. No thread routing, as before.
    case unchanged

    init(actionIdentifier: String, categoryIdentifier: String, userInfo: [AnyHashable: Any]) {
        switch ReminderResponse(actionIdentifier: actionIdentifier,
                                categoryIdentifier: categoryIdentifier, userInfo: userInfo) {
        case .press(let action): self = .reminder(action)
        case .body: self = .openOnly
        case nil: self = .unchanged
        }
    }

    /// How a notification arriving while the app is FRONTMOST is shown. A reminder is shown,
    /// buttons and all: it is time-critical and does not belong to anything on screen.
    /// Everything else is shown exactly as it was before the delegate existed, which is not
    /// at all — the Mac's own turn banners are only posted while it is in the background.
    static func presentation(categoryIdentifier: String,
                             userInfo: [AnyHashable: Any]) -> UNNotificationPresentationOptions {
        if categoryIdentifier == ReminderNotification.categoryIdentifier
            || ReminderNotification.reminderId(in: userInfo) != nil {
            return [.banner, .list, .sound]
        }
        return []
    }
}

/// The Mac's notification-center delegate. Installed at launch by `MacPushRegistrar`.
final class MacNotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    /// The center holds its delegate weakly; this is the strong reference.
    static let shared = MacNotificationDelegate(
        handle: { action in
            _ = await MacNotificationDelegate.handler.handle(action)
        },
        activate: { NSApplication.shared.activate() })

    /// The Mac's handler: the paired bridge, and one retry in place of a queue.
    static let handler = ReminderActionHandler(
        client: { JesseBridgeClient(config: MacPushRegistrar.config()) },
        undelivered: .retryOnce())

    private let handle: @MainActor (ReminderAction) async -> Void
    private let activate: @MainActor () -> Void

    init(handle: @escaping @MainActor (ReminderAction) async -> Void,
         activate: @escaping @MainActor () -> Void) {
        self.handle = handle
        self.activate = activate
    }

    /// Become the center's delegate and register the categories. Before launch finishes, so
    /// a click that launched the app is delivered here, and before any push can be shown: a
    /// category the center does not know is drawn with no buttons at all.
    func install(on center: any MacNotificationCentering = UNUserNotificationCenter.current()) {
        center.delegate = self
        MacNotificationCategories.register(on: center)
    }

    /// Carry out a route. `completion` is called once the work is done — for a press, only
    /// after it has landed, been refused, or been retried and let go.
    func perform(_ route: MacNotificationRoute, completion: @escaping @MainActor () -> Void) {
        switch route {
        case .reminder(let action):
            Task { @MainActor in
                await handle(action)
                completion()
            }
        case .openOnly, .unchanged:
            activate()
            completion()
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let content = notification.request.content
        completionHandler(MacNotificationRoute.presentation(
            categoryIdentifier: content.categoryIdentifier, userInfo: content.userInfo))
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        // Parsed here, at the synchronous entry, so only a `Sendable` route crosses actors.
        let content = response.notification.request.content
        let route = MacNotificationRoute(actionIdentifier: response.actionIdentifier,
                                         categoryIdentifier: content.categoryIdentifier,
                                         userInfo: content.userInfo)
        let done = MacUncheckedCompletion(completionHandler)
        Task { @MainActor in
            self.perform(route) { done.call() }
        }
    }
}

/// The system's completion handler, carried to the main actor and called exactly once.
nonisolated struct MacUncheckedCompletion: @unchecked Sendable {
    private let body: () -> Void
    init(_ body: @escaping () -> Void) { self.body = body }
    func call() { body() }
}
