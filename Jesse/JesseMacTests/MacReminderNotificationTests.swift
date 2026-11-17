import XCTest
import UserNotifications
@testable import Jesse_Mac
import JesseCore
import JesseNetworking

/// A timed reminder on the Mac: the category the Mac registers, how a response is routed, and
/// what pressing a button does — applied, dropped for an unknown reminder, and with no offline
/// queue on the Mac, retried once and then let go when the bridge cannot be reached.
@MainActor
final class MacReminderNotificationTests: XCTestCase {

    /// The bridge's own payload, as `bridge/src/apns.rs` sends it.
    private let payload: [AnyHashable: Any] = [
        "aps": [
            "alert": ["title": "Reminder", "body": "start the pancakes"],
            "sound": "default",
            "interruption-level": "time-sensitive",
            "category": "JESSE_REMINDER",
        ] as [String: Any],
        "reminder_id": "r7k2m9",
    ]

    // MARK: - The categories

    func testTheMacRegistersTheReminderWithExactlyDoneAndSnooze() throws {
        let reminder = try XCTUnwrap(MacNotificationCategories.all.first {
            $0.identifier == "JESSE_REMINDER"
        })
        XCTAssertEqual(reminder.actions.map(\.identifier),
                       ["JESSE_REMINDER_DONE", "JESSE_REMINDER_SNOOZE"])
        XCTAssertEqual(reminder.actions.map(\.title), ["Done", "Snooze 10 min"])
        for action in reminder.actions {
            XCTAssertFalse(action.options.contains(.foreground),
                           "\(action.identifier) must not bring the app forward")
            XCTAssertFalse(action.options.contains(.authenticationRequired))
        }
    }

    /// `setNotificationCategories` replaces the whole set, so the one set the Mac registers
    /// must carry every category the Mac uses. The Mac's own turn banners use none (they are
    /// posted with the empty category), so the set is the reminder's, and nothing is lost.
    func testTheOneRegisteredSetKeepsEveryCategoryTheMacUses() {
        let identifiers = Set(MacNotificationCategories.all.map(\.identifier))
        XCTAssertEqual(identifiers, ["JESSE_REMINDER"])
        // The turn banner's content carries no category to lose.
        let banner = UNMutableNotificationContent()
        XCTAssertEqual(banner.categoryIdentifier, "")
    }

    /// The test host is the app, so `applicationWillFinishLaunching` has already made the
    /// Mac's delegate the center's by the time this runs.
    func testTheDelegateIsInstalledAtLaunch() {
        XCTAssertTrue(UNUserNotificationCenter.current().delegate === MacNotificationDelegate.shared)
    }

    final class FakeCenter: MacNotificationCentering {
        weak var delegate: (any UNUserNotificationCenterDelegate)?
        private(set) var registered: [Set<UNNotificationCategory>] = []
        func setNotificationCategories(_ categories: Set<UNNotificationCategory>) {
            registered.append(categories)
        }
    }

    /// Installing sets the delegate and registers the WHOLE set in one call — a second call
    /// would replace the first.
    func testInstallRegistersTheWholeSetOnceAndBecomesTheDelegate() throws {
        let center = FakeCenter()
        let delegate = MacNotificationDelegate(handle: { _ in }, activate: {})
        delegate.install(on: center)
        XCTAssertTrue(center.delegate === delegate)
        XCTAssertEqual(center.registered.count, 1)
        let set = try XCTUnwrap(center.registered.first)
        XCTAssertEqual(set, MacNotificationCategories.all)
        XCTAssertEqual(set.first { $0.identifier == "JESSE_REMINDER" }?.actions.map(\.identifier),
                       ["JESSE_REMINDER_DONE", "JESSE_REMINDER_SNOOZE"])
    }

    // MARK: - Routing a response

    func testDoneAndSnoozeRouteToTheHandlerWithTheReminderId() {
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: "JESSE_REMINDER_DONE",
                                            categoryIdentifier: "JESSE_REMINDER",
                                            userInfo: payload),
                       .reminder(.done(reminderId: "r7k2m9")))
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: "JESSE_REMINDER_SNOOZE",
                                            categoryIdentifier: "JESSE_REMINDER",
                                            userInfo: payload),
                       .reminder(.snooze(reminderId: "r7k2m9", minutes: 10)))
    }

    func testAReminderBodyTapRoutesToOpenOnly() {
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                            categoryIdentifier: "JESSE_REMINDER",
                                            userInfo: payload), .openOnly)
        // Even one that somehow carried a routing key stays a reminder.
        var routed = payload
        routed["conversation_id"] = "CID-1"
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                            categoryIdentifier: "JESSE_REMINDER",
                                            userInfo: routed), .openOnly)
    }

    /// The Mac's own turn banner and any other push keep the route they had before there was
    /// a delegate: the app comes forward, nothing more.
    func testANonReminderResponseKeepsItsRoute() {
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                            categoryIdentifier: "", userInfo: [:]), .unchanged)
        XCTAssertEqual(MacNotificationRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                            categoryIdentifier: "",
                                            userInfo: ["job_id": "job-1",
                                                       "conversation_id": "CID-1"]),
                       .unchanged)
    }

    /// While the app is frontmost a reminder is shown; the Mac's own banners are shown exactly
    /// as before the delegate existed — not at all.
    func testFrontmostPresentationShowsRemindersOnly() {
        XCTAssertEqual(MacNotificationRoute.presentation(categoryIdentifier: "JESSE_REMINDER",
                                                         userInfo: payload),
                       [.banner, .list, .sound])
        XCTAssertEqual(MacNotificationRoute.presentation(categoryIdentifier: "", userInfo: [:]), [])
    }

    func testAPressIsHandedToTheHandlerAndNothingComesForward() async {
        var handled: [ReminderAction] = []
        var activations = 0
        let delegate = MacNotificationDelegate(handle: { handled.append($0) },
                                               activate: { activations += 1 })
        let finished = expectation(description: "completion")
        delegate.perform(.reminder(.done(reminderId: "r7k2m9"))) { finished.fulfill() }
        await fulfillment(of: [finished], timeout: 5)
        XCTAssertEqual(handled, [.done(reminderId: "r7k2m9")])
        XCTAssertEqual(activations, 0)
    }

    func testABodyTapComesForwardAndPressesNothing() {
        var handled: [ReminderAction] = []
        var activations = 0
        var completed = 0
        let delegate = MacNotificationDelegate(handle: { handled.append($0) },
                                               activate: { activations += 1 })
        delegate.perform(.openOnly) { completed += 1 }
        delegate.perform(.unchanged) { completed += 1 }
        XCTAssertTrue(handled.isEmpty)
        XCTAssertEqual(activations, 2)
        XCTAssertEqual(completed, 2)
    }

    // MARK: - Pressing a button, the Mac's way

    final class FakeReminders: ReminderActing, @unchecked Sendable {
        enum Answer { case result(ReminderActionResult), status(Int), unreachable }
        /// One answer per call, the last repeated.
        var answers: [Answer] = [.result(.applied)]
        private(set) var done: [String] = []
        private(set) var snoozed: [(id: String, minutes: Int)] = []

        func reminderDone(id: String) async throws -> ReminderActionResult {
            done.append(id)
            return try reply()
        }

        func reminderSnooze(id: String, minutes: Int) async throws -> ReminderActionResult {
            snoozed.append((id, minutes))
            return try reply()
        }

        private func reply() throws -> ReminderActionResult {
            let answer = answers.count > 1 ? answers.removeFirst() : answers[0]
            switch answer {
            case .result(let r): return r
            case .status(let code): return try ReminderActionResult.classify(status: code, body: "")
            case .unreachable: throw JesseError.cannotConnect("studio")
            }
        }
    }

    final class SleepLog: @unchecked Sendable {
        var slept: [Duration] = []
    }

    private func macHandler(_ fake: FakeReminders, _ sleeps: SleepLog) -> ReminderActionHandler {
        ReminderActionHandler(client: { fake },
                              undelivered: .retryOnce(after: .seconds(5),
                                                      sleep: { sleeps.slept.append($0) }))
    }

    func testAppliedOnTheFirstTry() async {
        let fake = FakeReminders(), sleeps = SleepLog()
        let done = await macHandler(fake, sleeps).handle(.done(reminderId: "r7k2m9"))
        let snooze = await macHandler(fake, sleeps).handle(.snooze(reminderId: "r7k2m9", minutes: 10))
        XCTAssertEqual(done, .applied)
        XCTAssertEqual(snooze, .applied)
        XCTAssertEqual(fake.done, ["r7k2m9"])
        XCTAssertEqual(fake.snoozed.map(\.minutes), [10])
        XCTAssertTrue(sleeps.slept.isEmpty, "no retry when the first try lands")
    }

    func testAnUnknownReminderIsDroppedWithoutARetry() async {
        let fake = FakeReminders(), sleeps = SleepLog()
        fake.answers = [.status(404)]
        let outcome = await macHandler(fake, sleeps).handle(.done(reminderId: "gone"))
        XCTAssertEqual(outcome, .dropped)
        XCTAssertEqual(fake.done, ["gone"])
        XCTAssertTrue(sleeps.slept.isEmpty)
    }

    func testAnUnreachableBridgeIsRetriedOnceThenLost() async {
        let fake = FakeReminders(), sleeps = SleepLog()
        fake.answers = [.unreachable]
        let outcome = await macHandler(fake, sleeps).handle(.snooze(reminderId: "r1", minutes: 10))
        XCTAssertEqual(outcome, .lost)
        XCTAssertEqual(fake.snoozed.map(\.id), ["r1", "r1"], "exactly one retry")
        XCTAssertEqual(sleeps.slept, [.seconds(5)])
    }

    func testAnUnreachableBridgeThatComesBackOnTheRetryIsApplied() async {
        let fake = FakeReminders(), sleeps = SleepLog()
        fake.answers = [.status(503), .result(.applied)]
        let outcome = await macHandler(fake, sleeps).handle(.done(reminderId: "r1"))
        XCTAssertEqual(outcome, .applied)
        XCTAssertEqual(fake.done, ["r1", "r1"])
        XCTAssertEqual(sleeps.slept, [.seconds(5)])
    }

    /// The Mac's real handler is the retry-once kind: it has no queue to hold a press on.
    func testTheMacsDefaultPolicyRetriesFiveSecondsOn() {
        guard case .retryOnce(let delay, _) = ReminderUndeliveredPolicy.retryOnce() else {
            return XCTFail("the Mac's policy is retry-once")
        }
        XCTAssertEqual(delay, .seconds(5))
    }
}
