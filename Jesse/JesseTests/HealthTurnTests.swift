import XCTest
import SwiftData
@testable import Jesse
import JesseCore
import JesseNetworking
import JesseDietDisplay
import JesseTodayDisplay

/// **Start-new-day through the real `HealthTurn`**, from the button and from the weigh-in.
///
/// Both record the diet day in `HealthNewDay.lastFiredDayKey`, and only the automatic
/// weigh-in is stopped by it. A tap is deliberate and always runs the refresh, online or held
/// offline; the day it records is what keeps a later weigh-in from firing a second one. The
/// route is `HealthTurnRoute`'s test in JesseCore. What only this file can show is which
/// caller turns the day guard on.
@MainActor
final class HealthTurnTests: XCTestCase {

    private static let day = "2026-08-22"

    // MARK: - Fixtures

    private static func makeContext() throws -> ModelContext {
        let container = try ModelContainer(
            for: JesseThread.self, Turn.self, OutboxItem.self, OutboxAttachment.self,
            configurations: ModelConfiguration(isStoredInMemoryOnly: true))
        return ModelContext(container)
    }

    private static func coordinator(_ client: PromptCountingClient) -> RunCoordinator {
        RunCoordinator(config: { JesseConfig(host: "studio", port: 8765, token: "tok") },
                       makeClient: { _ in client })
    }

    /// A loaded dashboard, so `captureDay` is known, then marked offline or not.
    private static func model(offline: Bool,
                              pending: HeldRuns? = nil) async -> HealthDashboardModel {
        let model = HealthDashboardModel(makeClient: { OneDietDay(day: day) }, pending: pending)
        await model.load()
        model.isNetworkUnreachable = offline
        return model
    }

    /// A defaults domain of this test's own, so the real last-fired day is never touched.
    private func freshDefaults() -> UserDefaults {
        let suite = "HealthTurnTests.\(UUID().uuidString)"
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        return UserDefaults(suiteName: suite)!
    }

    /// Let the coordinator's detached send task run.
    private static func settle() async {
        try? await Task.sleep(nanoseconds: 120_000_000)
    }

    // MARK: - The button

    /// Today's refresh already ran from this device, and a tap runs it again.
    func testATapSendsEvenWhenTodaysRefreshAlreadyRan() async throws {
        let defaults = freshDefaults()
        defaults.set(Self.day, forKey: HealthNewDay.lastFiredDayKey)
        let context = try Self.makeContext()
        let client = PromptCountingClient()
        let coordinator = Self.coordinator(client)
        let model = await Self.model(offline: false)

        let outcome = HealthTurn.startNewDay(model: model, coordinator: coordinator,
                                             context: context, origin: .phone,
                                             dietDay: Self.day, oncePerDay: false,
                                             defaults: defaults)

        XCTAssertEqual(outcome, .sent)
        await Self.settle()
        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 1)
        XCTAssertEqual(try context.fetch(FetchDescriptor<JesseThread>()).count, 1)
    }

    /// Offline, the same tap is held rather than refused as a repeat.
    func testATapOfflineOnADayThatAlreadyRanIsHeld() async throws {
        let defaults = freshDefaults()
        defaults.set(Self.day, forKey: HealthNewDay.lastFiredDayKey)
        let store = HeldRuns()
        let client = PromptCountingClient()
        let coordinator = Self.coordinator(client)
        let model = await Self.model(offline: true, pending: store)

        let outcome = HealthTurn.startNewDay(model: model, coordinator: coordinator,
                                             context: try Self.makeContext(), origin: .phone,
                                             dietDay: Self.day, oncePerDay: false,
                                             defaults: defaults)

        XCTAssertEqual(outcome, .queued)
        XCTAssertEqual(store.all().map(\.kind), [.startNewDay])
        await Self.settle()
        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 0, "held, not sent")
    }

    // MARK: - The weigh-in

    /// A tap records the day, so the first weigh-in after it fires nothing: one refresh.
    func testAWeighInAfterATapFiresNothing() async throws {
        let defaults = freshDefaults()
        let context = try Self.makeContext()
        let client = PromptCountingClient()
        let coordinator = Self.coordinator(client)
        let model = await Self.model(offline: false)

        XCTAssertEqual(HealthTurn.startNewDay(model: model, coordinator: coordinator,
                                              context: context, origin: .phone,
                                              dietDay: Self.day, oncePerDay: false,
                                              defaults: defaults), .sent)
        XCTAssertEqual(defaults.string(forKey: HealthNewDay.lastFiredDayKey), Self.day)

        XCTAssertEqual(HealthTurn.startNewDay(model: model, coordinator: coordinator,
                                              context: context, origin: .automatic,
                                              dietDay: Self.day, oncePerDay: true,
                                              capturedDay: Self.day,
                                              defaults: defaults), .alreadyRan)

        await Self.settle()
        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 1)
        XCTAssertEqual(try context.fetch(FetchDescriptor<JesseThread>()).count, 1)
    }

    // MARK: - The automatic workout log on a locked phone

    /// A trigger over this test's defaults, clock and store, paired, with the lock state
    /// read from `locked` so a test can unlock the phone part way through. The trigger
    /// holds its coordinator weakly, as the app's does, so the test keeps it alive.
    private func trigger(defaults: UserDefaults, context: ModelContext, now: Date,
                         locked: @escaping @MainActor () -> Bool,
                         client: PromptCountingClient)
        async -> (HealthAutoTrigger, RunCoordinator) {
        let trigger = HealthAutoTrigger(defaults: defaults, now: { now },
                                        protectedDataAvailable: { !locked() },
                                        isPaired: { true }, context: { context })
        let coordinator = Self.coordinator(client)
        trigger.attach(coordinator: coordinator, model: await Self.model(offline: false))
        return (trigger, coordinator)
    }

    /// One workout noticed three minutes ago: past the settle, so it is due now.
    private static func seedPendingWorkout(_ defaults: UserDefaults, now: Date) throws {
        let noticed = now.addingTimeInterval(-3 * 60)
        let ledger = WorkoutLedger(pending: ["walk-1343": noticed], burstStartedAt: noticed)
        defaults.set(try JSONEncoder().encode(ledger), forKey: HealthAutoTrigger.ledgerKey)
    }

    private static func ledger(_ defaults: UserDefaults) throws -> WorkoutLedger {
        try JSONDecoder().decode(WorkoutLedger.self,
                                 from: defaults.data(forKey: HealthAutoTrigger.ledgerKey)!)
    }

    /// The 2026-09-30 walk: it settled with the phone locked. Nothing is sent, because the
    /// turn would carry no workouts, and the walk stays pending rather than marked logged.
    func testALockedPhoneSendsNoWorkoutLogAndKeepsTheWorkoutPending() async throws {
        let defaults = freshDefaults()
        let now = Date()
        try Self.seedPendingWorkout(defaults, now: now)
        let client = PromptCountingClient()
        let (trigger, coordinator) = await trigger(defaults: defaults,
                                                   context: try Self.makeContext(),
                                                   now: now, locked: { true }, client: client)

        await trigger.settleWorkouts()
        await Self.settle()

        XCTAssertEqual(client.count(of: HealthWorkoutLog.prompt), 0, "no send while locked")
        let ledger = try Self.ledger(defaults)
        XCTAssertNotNil(ledger.pending["walk-1343"], "still pending")
        XCTAssertNil(ledger.fired["walk-1343"], "not marked logged")
        withExtendedLifetime(coordinator) {}
    }

    /// Unlocked, the next settle sends one workout log and only then marks the walk fired.
    func testUnlockingThenSettlingSendsOneWorkoutLogAndMarksItFired() async throws {
        let defaults = freshDefaults()
        let now = Date()
        try Self.seedPendingWorkout(defaults, now: now)
        let client = PromptCountingClient()
        var locked = true
        let (trigger, coordinator) = await trigger(defaults: defaults,
                                                   context: try Self.makeContext(),
                                                   now: now, locked: { locked }, client: client)

        await trigger.settleWorkouts()
        await Self.settle()
        XCTAssertEqual(client.count(of: HealthWorkoutLog.prompt), 0, "held while locked")
        XCTAssertNotNil(try Self.ledger(defaults).pending["walk-1343"])

        locked = false
        await trigger.settleWorkouts()
        await Self.settle()

        XCTAssertEqual(client.count(of: HealthWorkoutLog.prompt), 1)
        let ledger = try Self.ledger(defaults)
        XCTAssertNil(ledger.pending["walk-1343"])
        XCTAssertNotNil(ledger.fired["walk-1343"], "marked fired once sent")
        withExtendedLifetime(coordinator) {}
    }

    // MARK: - The automatic morning refresh on a locked phone

    /// A first weigh-in of today's diet day, as the body-mass observer hands it over.
    private static func weighIn(at date: Date) -> HealthArrivals<Date> {
        HealthArrivals(items: [date], isBaseline: false, anchor: nil, deletedCount: 0)
    }

    /// A weigh-in that lands on a locked phone sends nothing, because the turn would carry no
    /// weight, and leaves the day unrecorded so a later fire is still allowed.
    func testALockedPhoneSendsNoMorningRefreshAndLeavesTheDayUnrecorded() async throws {
        let defaults = freshDefaults()
        let now = Date()
        let client = PromptCountingClient()
        let (trigger, coordinator) = await trigger(defaults: defaults,
                                                   context: try Self.makeContext(),
                                                   now: now, locked: { true }, client: client)

        trigger.receiveBodyMass(Self.weighIn(at: now))
        await Self.settle()

        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 0, "no send while locked")
        XCTAssertNil(defaults.string(forKey: HealthNewDay.lastFiredDayKey), "day not recorded")
        withExtendedLifetime(coordinator) {}
    }

    /// Unlocked, the next settle sends the held refresh exactly once and records the day.
    func testUnlockingThenSettlingSendsOneMorningRefreshAndRecordsTheDay() async throws {
        let defaults = freshDefaults()
        let now = Date()
        let client = PromptCountingClient()
        var locked = true
        let (trigger, coordinator) = await trigger(defaults: defaults,
                                                   context: try Self.makeContext(),
                                                   now: now, locked: { locked }, client: client)

        trigger.receiveBodyMass(Self.weighIn(at: now))
        await Self.settle()
        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 0, "held while locked")

        locked = false
        await trigger.settle()
        await trigger.settle()
        await Self.settle()

        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 1, "exactly one refresh")
        XCTAssertEqual(defaults.string(forKey: HealthNewDay.lastFiredDayKey),
                       DietDay.stamp(for: now))
        XCTAssertNil(defaults.string(forKey: HealthAutoTrigger.heldMorningRefreshKey),
                     "the hold is cleared once sent")
        withExtendedLifetime(coordinator) {}
    }

    /// A refresh held for a morning that has since rolled is dropped, never sent: it would
    /// audit the wrong day.
    func testARefreshHeldFromYesterdayIsClearedAndSendsNothing() async throws {
        let defaults = freshDefaults()
        let now = Date()
        defaults.set(DietDay.stamp(for: now.addingTimeInterval(-24 * 3600)),
                     forKey: HealthAutoTrigger.heldMorningRefreshKey)
        let client = PromptCountingClient()
        let (trigger, coordinator) = await trigger(defaults: defaults,
                                                   context: try Self.makeContext(),
                                                   now: now, locked: { false }, client: client)

        await trigger.settle()
        await Self.settle()

        XCTAssertEqual(client.count(of: HealthNewDay.prompt), 0)
        XCTAssertNil(defaults.string(forKey: HealthAutoTrigger.heldMorningRefreshKey))
        XCTAssertNil(defaults.string(forKey: HealthNewDay.lastFiredDayKey))
        withExtendedLifetime(coordinator) {}
    }

    /// A morning refresh held in the capture queue and replayed while the phone is locked
    /// goes through the real client, which refuses it before it leaves: the replay defers
    /// rather than refusing, and the outbox row says why. A later replay after the day rolled
    /// is still refused for that reason, so a stale morning never runs.
    func testALockedReplayOfTheMorningRefreshDefersAndARolledDayStillRefuses() async throws {
        let context = try Self.makeContext()
        let config = JesseConfig(host: "127.0.0.1", port: 1, token: "tok")
        let client = JesseClient(config: config,
                                 healthProvider: LockedHealth(),
                                 isHealthContextEnabled: { true },
                                 healthClassifier: HealthKeywordClassifier(),
                                 isLocationContextEnabled: { false },
                                 mealCorrectionsAck: { nil },
                                 readableSnapshotCache: nil)
        let coordinator = RunCoordinator(config: { config }, makeClient: { _ in client })
        let sender = CoordinatorTellSender(coordinator: coordinator,
                                           context: FixedContext(context))
        let store = HeldRuns()
        store.append(PendingIntentRecord(
            kind: .startNewDay, dayDate: Self.day,
            payload: PendingIntentPayload(origin: ThreadOrigin.automatic.rawValue)))
        let dietDay = LiveDietDay(Self.day)
        let replayer = IntentReplayer(store: store,
                                      day: TodayDashboardModel(makeClient: { NoToday() }),
                                      makeClient: { NoToday() }, tell: sender,
                                      dietDay: { dietDay.value })

        let locked = await replayer.replayAll()
        XCTAssertEqual(locked, [.deferred])
        XCTAssertEqual(store.all().first?.state, .queued, "still held for the next replay")
        let row = try XCTUnwrap(context.fetch(FetchDescriptor<OutboxItem>()).first)
        XCTAssertEqual(row.lastError, HealthContextLockedError().errorDescription,
                       "held for the lock, not failed on the network")

        dietDay.value = "2026-08-23"
        let rolled = await replayer.replayAll()
        XCTAssertEqual(rolled.count, 1)
        guard case .refused = rolled.first else {
            return XCTFail("a rolled day refuses the held morning, got \(rolled)")
        }
        XCTAssertEqual(try context.fetch(FetchDescriptor<OutboxItem>()).count, 1,
                       "the refused replay staged nothing")
        withExtendedLifetime(coordinator) {}
    }
}

// MARK: - Doubles

/// Records the text of every send without touching the network.
@MainActor
private final class PromptCountingClient: JesseClientProtocol {
    private(set) var sent: [String] = []

    func count(of text: String) -> Int { sent.filter { $0 == text }.count }

    func send(mode: JesseMode, text: String, sessionId: String?,
              conversationId: String, voice: Bool,
              instructions: String?, floorOverride: String?,
              attachments: [JesseAttachment], requestId: UUID,
              model: String?, effort: String?, sentFor: String?) async throws -> JesseSendResult {
        sent.append(text)
        return .reply(JesseReply(text: "ok", sessionId: "s-1"), jobId: nil, conversationId: nil)
    }

    func result(jobId: String) async throws -> JesseResultState { .running }
    func cancelJob(jobId: String) async throws {}
    func stream(jobId: String) -> AsyncThrowingStream<JesseStreamEvent, Error> {
        AsyncThrowingStream { $0.finish() }
    }
}

/// One diet day with nothing in it, so the model has a `captureDay` to hold a run against.
private struct OneDietDay: DietSnapshotProviding {
    let day: String

    func fetchDietSnapshot(date: String?) async throws -> DietSnapshot {
        try DietSnapshot.decode(from: Data("""
        {"asOf":"\(day)T07:00:00Z","dietDay":"\(day)",
         "today":{"date":"\(day)","exercise":[],"meals":[],"targets":{"calories":2100}},
         "errors":[]}
        """.utf8))
    }
}

/// An in-memory capture queue.
private final class HeldRuns: PendingIntentStoring {
    nonisolated deinit {}
    private(set) var records: [PendingIntentRecord] = []
    func all() -> [PendingIntentRecord] { records.sorted { $0.createdAt < $1.createdAt } }
    func append(_ record: PendingIntentRecord) { records.append(record) }
    func update(_ record: PendingIntentRecord) {
        guard let i = records.firstIndex(where: { $0.id == record.id }) else { return }
        records[i] = record
    }
    func delete(id: UUID) { records.removeAll { $0.id == id } }
}

/// The diet day the replayer reads, so a test can roll it between two replays.
@MainActor
private final class LiveDietDay {
    nonisolated deinit {}
    var value: String
    init(_ value: String) { self.value = value }
}

/// Apple Health on a locked phone: nothing readable, and it says so.
private struct LockedHealth: HealthContextProviding {
    func snapshot() async -> HealthSnapshot { .locked }
}

/// The replayer's one store, handed over as the app's provider would.
@MainActor
private final class FixedContext: ModelContextProviding {
    nonisolated deinit {}
    let modelContext: ModelContext?
    init(_ context: ModelContext) { modelContext = context }
}

/// A Today endpoint that is never reached: a Start-new-day replay does not read the day file.
private struct NoToday: TodayProviding {
    struct Unused: Error {}
    func getToday(ifNoneMatch: String?) async throws -> TodayFetchResult { throw Unused() }
    func checkItem(id: String, checked: Bool, evidence: String?, at: Date,
                   day: String?, ifMatch: String) async throws -> TodayMutationResult {
        throw Unused()
    }
    func moveItem(id: String, op: TodayMoveOp, at: Date,
                  day: String?, ifMatch: String) async throws -> TodayMutationResult {
        throw Unused()
    }
    func postpone(id: String, deferred: Bool, at: Date,
                  day: String?, ifMatch: String) async throws -> TodayMutationResult {
        throw Unused()
    }
    func glance(id: String, at: Date, ifMatch: String) async throws -> TodayMutationResult {
        throw Unused()
    }
}
