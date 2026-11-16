import XCTest
import UserNotifications
@testable import Jesse
import JesseCore
import JesseNetworking

/// A timed reminder's notification: the category and its two buttons, how a response is
/// routed, and what pressing a button does — on the bridge when it answers, on the offline
/// capture queue when it does not, and nothing at all when the bridge says the reminder is
/// gone.
@MainActor
final class ReminderNotificationTests: XCTestCase {

    /// The bridge's own payload, as `bridge/src/apns.rs` sends it.
    private let payload: [AnyHashable: Any] = [
        "aps": [
            "alert": ["title": "Reminder", "body": "start the pancakes"],
            "sound": "default",
            "interruption-level": "time-sensitive",
            "category": "JESSE_REMINDER",
            "badge": 3,
        ] as [String: Any],
        "reminder_id": "r7k2m9",
    ]

    // MARK: - The category

    func testTheCategoryHasDoneAndSnoozeAndNeitherOpensTheApp() {
        let category = ReminderNotification.category
        XCTAssertEqual(category.identifier, "JESSE_REMINDER")
        XCTAssertEqual(category.actions.map(\.identifier),
                       ["JESSE_REMINDER_DONE", "JESSE_REMINDER_SNOOZE"])
        XCTAssertEqual(category.actions.map(\.title), ["Done", "Snooze 10 min"])
        for action in category.actions {
            XCTAssertFalse(action.options.contains(.foreground),
                           "\(action.identifier) must run in the background")
            XCTAssertFalse(action.options.contains(.destructive))
            XCTAssertFalse(action.options.contains(.authenticationRequired))
        }
    }

    /// The app registers it at launch — the test host is the app, so its delegate's
    /// `didFinishLaunching` has already run by the time this does.
    func testTheCategoryIsRegisteredAtLaunch() async {
        let registered = await UNUserNotificationCenter.current().notificationCategories()
        let reminder = registered.first { $0.identifier == ReminderNotification.categoryIdentifier }
        XCTAssertNotNil(reminder, "JESSE_REMINDER is registered with the notification center")
        XCTAssertEqual(reminder?.actions.map(\.identifier),
                       [ReminderNotification.doneActionIdentifier,
                        ReminderNotification.snoozeActionIdentifier])
    }

    // MARK: - Routing a response

    func testTheButtonsParseIntoPresses() {
        XCTAssertEqual(ReminderAction(actionIdentifier: "JESSE_REMINDER_DONE", userInfo: payload),
                       .done(reminderId: "r7k2m9"))
        XCTAssertEqual(ReminderAction(actionIdentifier: "JESSE_REMINDER_SNOOZE", userInfo: payload),
                       .snooze(reminderId: "r7k2m9", minutes: 10))
        // The body tap is not a press, and a press with no id has nothing to act on.
        XCTAssertNil(ReminderAction(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                    userInfo: payload))
        XCTAssertNil(ReminderAction(actionIdentifier: "JESSE_REMINDER_DONE",
                                    userInfo: ["reminder_id": "  "]))
    }

    func testButtonResponsesRouteToTheHandler() {
        let route = NotificationResponseRoute(actionIdentifier: "JESSE_REMINDER_SNOOZE",
                                              categoryIdentifier: "JESSE_REMINDER",
                                              userInfo: payload)
        XCTAssertEqual(route, .reminder(.snooze(reminderId: "r7k2m9", minutes: 10)))
    }

    /// Tapping a reminder's body opens the app and does NOTHING else: there is no thread to
    /// land on, so nothing may be handed to the router.
    func testTappingAReminderBodyOpensTheAppAndRoutesNowhere() {
        let route = NotificationResponseRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                              categoryIdentifier: "JESSE_REMINDER",
                                              userInfo: payload)
        XCTAssertEqual(route, .openOnly)
        XCTAssertNil(PushTap(userInfo: payload), "the payload carries no routing keys")
        // Even a reminder that somehow carried a routing key stays a reminder.
        var routed = payload
        routed["conversation_id"] = "CID-1"
        XCTAssertEqual(NotificationResponseRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                                 categoryIdentifier: "JESSE_REMINDER",
                                                 userInfo: routed), .openOnly)
    }

    /// The finished-turn tap is unchanged.
    func testAFinishedTurnTapStillRoutesToItsThread() {
        let route = NotificationResponseRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                              categoryIdentifier: "",
                                              userInfo: ["job_id": "job-1",
                                                         "conversation_id": "CID-1"])
        XCTAssertEqual(route, .thread(PushTap(jobId: "job-1", conversationId: "CID-1")))
        XCTAssertEqual(NotificationResponseRoute(actionIdentifier: UNNotificationDefaultActionIdentifier,
                                                 categoryIdentifier: "", userInfo: [:]),
                       .openOnly)
    }

    // MARK: - Pressing a button

    final class FakeReminders: ReminderActing, @unchecked Sendable {
        enum Answer { case result(ReminderActionResult), status(Int), unreachable }
        var answer: Answer = .result(.applied)
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
            switch answer {
            case .result(let r): return r
            case .status(let code): return try ReminderActionResult.classify(status: code, body: "")
            case .unreachable: throw JesseError.cannotConnect("laptop")
            }
        }
    }

    final class FakeQueue: PendingIntentStoring {
        nonisolated deinit {}
        private(set) var records: [PendingIntentRecord] = []
        func all() -> [PendingIntentRecord] { records }
        func append(_ record: PendingIntentRecord) { records.append(record) }
        func update(_ record: PendingIntentRecord) {
            guard let i = records.firstIndex(where: { $0.id == record.id }) else { return }
            records[i] = record
        }
        func delete(id: UUID) { records.removeAll { $0.id == id } }
    }

    private let pressedAt = Date(timeIntervalSince1970: 1_793_000_000)

    private func handler(_ fake: FakeReminders, _ queue: FakeQueue) -> ReminderActionHandler {
        ReminderActionHandler(client: { fake }, queue: { queue }, now: { self.pressedAt })
    }

    func testDoneCallsTheDoneRoute() async {
        let fake = FakeReminders(), queue = FakeQueue()
        let outcome = await handler(fake, queue).handle(.done(reminderId: "r7k2m9"))
        XCTAssertEqual(outcome, .applied)
        XCTAssertEqual(fake.done, ["r7k2m9"])
        XCTAssertTrue(fake.snoozed.isEmpty)
        XCTAssertTrue(queue.records.isEmpty)
    }

    func testSnoozeCallsTheSnoozeRouteWithTen() async {
        let fake = FakeReminders(), queue = FakeQueue()
        let action = try! XCTUnwrap(ReminderAction(actionIdentifier: "JESSE_REMINDER_SNOOZE",
                                                   userInfo: payload))
        let outcome = await handler(fake, queue).handle(action)
        XCTAssertEqual(outcome, .applied)
        XCTAssertEqual(fake.snoozed.map(\.id), ["r7k2m9"])
        XCTAssertEqual(fake.snoozed.map(\.minutes), [10])
        XCTAssertTrue(fake.done.isEmpty)
        XCTAssertTrue(queue.records.isEmpty)
    }

    /// The bridge cannot be reached: the press goes on the offline capture queue, as a row
    /// the existing replayer knows how to send.
    func testAnUnreachableBridgeQueuesThePress() async {
        let fake = FakeReminders(), queue = FakeQueue()
        fake.answer = .unreachable

        let done = await handler(fake, queue).handle(.done(reminderId: "r7k2m9"))
        let snooze = await handler(fake, queue).handle(.snooze(reminderId: "r7k2m9", minutes: 10))

        XCTAssertEqual(done, .queued)
        XCTAssertEqual(snooze, .queued)
        XCTAssertEqual(queue.records.map(\.kind), [.reminderDone, .reminderSnooze])
        XCTAssertEqual(queue.records.map(\.payload.reminderId), ["r7k2m9", "r7k2m9"])
        XCTAssertEqual(queue.records.map(\.payload.minutes), [nil, 10])
        XCTAssertEqual(queue.records.map(\.state), [.queued, .queued])
        XCTAssertEqual(queue.records.map(\.createdAt), [pressedAt, pressedAt])
    }

    /// A `5xx` is the bridge failing, not answering: queued too.
    func testAServerErrorQueuesThePress() async {
        let fake = FakeReminders(), queue = FakeQueue()
        fake.answer = .status(503)
        let outcome = await handler(fake, queue).handle(.done(reminderId: "r1"))
        XCTAssertEqual(outcome, .queued)
        XCTAssertEqual(queue.records.count, 1)
    }

    /// THE CONTRACT. A `404` is final: nothing is queued and nothing will be retried.
    func testA404IsDroppedNotQueued() async {
        let fake = FakeReminders(), queue = FakeQueue()
        fake.answer = .status(404)
        let done = await handler(fake, queue).handle(.done(reminderId: "gone"))
        let snooze = await handler(fake, queue).handle(.snooze(reminderId: "gone", minutes: 10))
        XCTAssertEqual(done, .dropped)
        XCTAssertEqual(snooze, .dropped)
        XCTAssertTrue(queue.records.isEmpty)
    }

    /// A queued press is exactly the row the replayer sends: same kind, same id, same length.
    func testAQueuedRowRoundTripsThroughTheStoredPayload() {
        let row = ReminderAction.snooze(reminderId: "r7k2m9", minutes: 10).pendingRecord(now: pressedAt)
        let stored = PendingIntentPayload.decode(row.payload.json)
        XCTAssertEqual(stored.reminderId, "r7k2m9")
        XCTAssertEqual(stored.minutes, 10)
        XCTAssertFalse(row.kind.isDayFileWrite, "not shown on the Today pending list")
    }
}
