import XCTest
@testable import JesseTodayDisplay
import JesseCore
import JesseNetworking

// A timed reminder's Done / Snooze, pressed while the bridge was out of reach and held on
// the offline capture queue, replayed by the same `IntentReplayer` every other held change
// goes through. The rules are simpler than a check-off's — the reminder is the bridge's
// own object and its id is its whole identity — and the one that matters is the contract's:
// a `404` is final and the row is DROPPED, while an unreachable bridge leaves it queued.

@MainActor
final class IntentReplayerReminderTests: XCTestCase {

    typealias FakeStore = IntentReplayerTests.FakeStore
    typealias ReplayClient = IntentReplayerTests.ReplayClient

    /// The two reminder routes, scripted, recording what each call carried.
    final class FakeReminders: ReminderActing, @unchecked Sendable {
        enum Answer { case result(ReminderActionResult), unreachable }
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
            case .unreachable: throw JesseError.cannotConnect("laptop")
            }
        }
    }

    nonisolated static let capturedAt = Date(timeIntervalSince1970: 1_772_521_500)
    nonisolated static let replayedAt = Date(timeIntervalSince1970: 1_772_560_000)

    private func makeReplayer(_ store: FakeStore,
                              reminders: FakeReminders?) -> IntentReplayer {
        let client = ReplayClient()
        var routes: (@MainActor () -> any ReminderActing)?
        if let reminders { routes = { reminders } }
        return IntentReplayer(
            store: store,
            day: TodayDashboardModel(makeClient: { client }, now: { Self.replayedAt }),
            makeClient: { client },
            reminders: routes,
            now: { Self.replayedAt })
    }

    private func held(_ kind: PendingIntentKind, id: String?,
                      minutes: Int? = nil) -> PendingIntentRecord {
        PendingIntentRecord(kind: kind, dayDate: "",
                            payload: PendingIntentPayload(reminderId: id, minutes: minutes),
                            createdAt: Self.capturedAt)
    }

    func testAHeldDoneReplaysToTheDoneRoute() async {
        let fake = FakeReminders()
        let row = held(.reminderDone, id: "r7k2m9")
        let store = FakeStore([row])

        let outcomes = await makeReplayer(store, reminders: fake).replayAll()

        XCTAssertEqual(outcomes, [.applied])
        XCTAssertEqual(fake.done, ["r7k2m9"])
        XCTAssertTrue(fake.snoozed.isEmpty)
        XCTAssertEqual(store.record(row.id)?.state, .applied)
    }

    func testAHeldSnoozeReplaysWithItsMinutes() async {
        let fake = FakeReminders()
        let store = FakeStore([held(.reminderSnooze, id: "r7k2m9", minutes: 10)])

        let outcomes = await makeReplayer(store, reminders: fake).replayAll()

        XCTAssertEqual(outcomes, [.applied])
        XCTAssertEqual(fake.snoozed.map(\.id), ["r7k2m9"])
        XCTAssertEqual(fake.snoozed.map(\.minutes), [10])
        XCTAssertTrue(fake.done.isEmpty)
    }

    /// A snooze stored without a length asks for the bridge's default, ten minutes.
    func testASnoozeWithNoStoredMinutesAsksForTen() async {
        let fake = FakeReminders()
        let store = FakeStore([held(.reminderSnooze, id: "r1")])
        _ = await makeReplayer(store, reminders: fake).replayAll()
        XCTAssertEqual(fake.snoozed.map(\.minutes), [IntentReplayer.defaultSnoozeMinutes])
        XCTAssertEqual(IntentReplayer.defaultSnoozeMinutes, 10)
    }

    /// Still unreachable: the press stays queued for the next recovery, and the run stops
    /// there, as it does for every other kind.
    func testAnUnreachableBridgeLeavesItQueued() async {
        let fake = FakeReminders()
        fake.answer = .unreachable
        let first = held(.reminderDone, id: "r1")
        var second = held(.reminderSnooze, id: "r2", minutes: 10)
        second.createdAt = Self.capturedAt.addingTimeInterval(1)
        let store = FakeStore([first, second])

        let outcomes = await makeReplayer(store, reminders: fake).replayAll()

        XCTAssertEqual(outcomes, [.deferred], "the run stops at the first deferral")
        XCTAssertEqual(store.record(first.id)?.state, .queued)
        XCTAssertEqual(store.record(second.id)?.state, .queued)
        XCTAssertTrue(fake.snoozed.isEmpty)
    }

    /// THE CONTRACT. A `404` is final: the bridge has no such reminder, and the row is
    /// deleted rather than kept as a refusal nobody can see.
    func testAnUnknownReminderIsDropped() async {
        let fake = FakeReminders()
        fake.answer = .result(.unknownReminder)
        let row = held(.reminderDone, id: "gone")
        let store = FakeStore([row])

        let outcomes = await makeReplayer(store, reminders: fake).replayAll()

        XCTAssertEqual(outcomes, [.dropped])
        XCTAssertNil(store.record(row.id), "dropped, not held and not refused")
    }

    /// Any other final refusal is dropped the same way.
    func testARefusalIsDropped() async {
        let fake = FakeReminders()
        fake.answer = .result(.refused(status: 400, message: "minutes out of range"))
        let row = held(.reminderSnooze, id: "r1", minutes: 10)
        let store = FakeStore([row])

        let outcomes = await makeReplayer(store, reminders: fake).replayAll()

        XCTAssertEqual(outcomes, [.dropped])
        XCTAssertNil(store.record(row.id))
    }

    /// A replayer built without the reminder routes holds the press rather than drop it.
    func testNoReminderRoutesDefers() async {
        let row = held(.reminderDone, id: "r1")
        let store = FakeStore([row])
        let outcomes = await makeReplayer(store, reminders: nil).replayAll()
        XCTAssertEqual(outcomes, [.deferred])
        XCTAssertEqual(store.record(row.id)?.state, .queued)
    }

    /// A row with no id has nothing to address and can never be delivered.
    func testARowWithNoIdIsDropped() async {
        let fake = FakeReminders()
        let row = held(.reminderDone, id: "  ")
        let store = FakeStore([row])
        let outcomes = await makeReplayer(store, reminders: fake).replayAll()
        XCTAssertEqual(outcomes, [.dropped])
        XCTAssertTrue(fake.done.isEmpty)
        XCTAssertNil(store.record(row.id))
    }

    /// Neither pending list shows a reminder action — the Today list holds day-file writes
    /// and the Health list its three kinds.
    func testReminderKindsAreNotDayFileWrites() {
        XCTAssertFalse(PendingIntentKind.reminderDone.isDayFileWrite)
        XCTAssertFalse(PendingIntentKind.reminderSnooze.isDayFileWrite)
        XCTAssertTrue(PendingIntentKind.reminderDone.isReminderAction)
        XCTAssertTrue(PendingIntentKind.reminderSnooze.isReminderAction)
        XCTAssertFalse(PendingIntentKind.check.isReminderAction)
        XCTAssertNil(PendingIntentKind.reminderDone.asserts)
    }
}
