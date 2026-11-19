import XCTest
import SwiftData
@testable import Jesse_Mac
import JesseCore
import JesseNetworking

/// The Mac's conversation-list poll: the pure cadence (`MacSessionPollRule`), what a tick does to
/// the store (`MacSessionPoller.tick` through the existing `refreshSessions`), and that a failed
/// tick does not leave the window red once a later one succeeds.
@MainActor
final class MacSessionPollerTests: XCTestCase {

    // MARK: - The timing rule

    func testFrontmostPollsEveryThirtySeconds() {
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable,
                                                    failures: 0), 30)
    }

    func testRunningButNotFrontmostPollsEveryFiveMinutes() {
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: false, reachability: .reachable,
                                                    failures: 0), 300)
    }

    /// `.unknown` is a cold launch before the first probe, not an outage: it polls.
    func testUnknownReachabilityStillPolls() {
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: true, reachability: .unknown,
                                                    failures: 0), 30)
    }

    func testAKnownUnreachableBridgeIsNotPolled() {
        XCTAssertNil(MacSessionPollRule.nextDelay(appActive: true, reachability: .unreachable,
                                                  failures: 0))
        XCTAssertNil(MacSessionPollRule.nextDelay(appActive: false, reachability: .unreachable,
                                                  failures: 3))
    }

    func testAnAsleepMacIsNotPolled() {
        XCTAssertNil(MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable,
                                                  failures: 0, asleep: true))
    }

    func testBackoffDoublesPerFailureAndCapsAtFiveMinutes() {
        let delays = (0...6).map {
            MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable, failures: $0)
        }
        XCTAssertEqual(delays, [30, 60, 120, 240, 300, 300, 300])
        // A counter far past the point the cap wins must not overflow into a short delay.
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable,
                                                    failures: 10_000), 300)
        // In the background the base already equals the cap.
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: false, reachability: .reachable,
                                                    failures: 4), 300)
    }

    /// The counter is reset by the coordinator on a completed round trip; the rule's half is that
    /// zero failures is the base cadence again.
    func testResetOnSuccessReturnsToTheBaseCadence() async throws {
        let context = try MacTestFixtures.context()
        let fake = ScriptedListClient([.failure, .failure, .failure, .notModified])
        let c = coordinator(fake)
        for _ in 0..<3 { await c.refreshSessions(context: context) }
        XCTAssertEqual(c.sessionRefreshFailures, 3)
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable,
                                                    failures: c.sessionRefreshFailures), 240)
        await c.refreshSessions(context: context)
        XCTAssertEqual(c.sessionRefreshFailures, 0, "a 304 is a completed round trip")
        XCTAssertEqual(MacSessionPollRule.nextDelay(appActive: true, reachability: .reachable,
                                                    failures: c.sessionRefreshFailures), 30)
    }

    func testRemainingIsMeasuredFromTheLastPull() {
        let t0 = Date(timeIntervalSince1970: 1_000)
        XCTAssertEqual(MacSessionPollRule.remaining(delay: 30, since: t0,
                                                    now: t0.addingTimeInterval(10)), 20)
        XCTAssertEqual(MacSessionPollRule.remaining(delay: 30, since: t0,
                                                    now: t0.addingTimeInterval(45)), 0)
        XCTAssertEqual(MacSessionPollRule.remaining(delay: 30, since: nil, now: t0), 30)
    }

    func testDebounceWindowIsTwoSeconds() {
        let t0 = Date(timeIntervalSince1970: 1_000)
        XCTAssertFalse(MacSessionPollRule.isDebounced(lastStart: nil, now: t0))
        XCTAssertTrue(MacSessionPollRule.isDebounced(lastStart: t0, now: t0.addingTimeInterval(1.9)))
        XCTAssertFalse(MacSessionPollRule.isDebounced(lastStart: t0, now: t0.addingTimeInterval(2)))
        XCTAssertFalse(MacSessionPollRule.isDebounced(lastStart: t0, now: t0.addingTimeInterval(-5)),
                       "a clock that went backwards is not recent")
    }

    // MARK: - Double firing

    /// Activation and the window's return to `.active` arrive together; the second automatic
    /// refresh is dropped rather than paying for another list call.
    func testTwoAutomaticRefreshesTogetherCostOneListCall() async throws {
        let context = try MacTestFixtures.context()
        let fake = ScriptedListClient([.notModified, .notModified])
        let c = coordinator(fake)
        await c.refreshSessionsUnlessRecent(context: context)
        await c.refreshSessionsUnlessRecent(context: context)
        XCTAssertEqual(fake.listCalls, 1)
        // A click is a request, not a signal: it is not debounced.
        await c.refreshSessions(context: context)
        XCTAssertEqual(fake.listCalls, 2)
    }

    // MARK: - What a tick does

    /// The common tick: nothing changed, the bridge answers `304`, and the store is not touched.
    func testATickAfterA304MakesNoStoreWrites() async throws {
        let context = try MacTestFixtures.context()
        let held = JesseThread(title: "Held", mode: .ask)
        held.conversationId = "held"
        context.insert(held)
        try context.save()
        let before = held.updatedAt

        let fake = ScriptedListClient([.notModified])
        let c = coordinator(fake)
        var saves = 0
        let token = NotificationCenter.default.addObserver(
            forName: ModelContext.didSave, object: context, queue: nil) { _ in saves += 1 }
        defer { NotificationCenter.default.removeObserver(token) }

        let poller = MacSessionPoller()
        await poller.tick(inputs(c, context))

        XCTAssertEqual(fake.listCalls, 1, "the tick went to the bridge")
        XCTAssertEqual(fake.etagsSent.first ?? nil, storedETag(),
                       "conditioned on the stored ETag")
        XCTAssertEqual(saves, 0, "a 304 saves nothing")
        XCTAssertFalse(context.hasChanges, "and leaves nothing pending")
        XCTAssertEqual(threads(context).count, 1)
        XCTAssertEqual(held.updatedAt, before)
    }

    /// The point of the feature: a conversation started on the phone is adopted by a tick, with
    /// no click.
    func testATickAfterAChangedListAdoptsANewRemoteConversation() async throws {
        let context = try MacTestFixtures.context()
        let cid = "fromPhone-\(UUID().uuidString)"
        let fake = ScriptedListClient([.list([summary(cid)], etag: "e-\(cid)")])
        let c = coordinator(fake)

        let poller = MacSessionPoller()
        await poller.tick(inputs(c, context))

        XCTAssertEqual(threads(context).map(\.conversationId), [cid])
        XCTAssertEqual(storedETag(), "e-\(cid)",
                       "the new ETag is stored once the adopt is done")
        XCTAssertEqual(poller.tickCount, 1)
    }

    /// One failed tick paints the caption; the next successful one takes it off.
    func testAFailedTickThenASuccessfulTickLeavesNoError() async throws {
        let context = try MacTestFixtures.context()
        let fake = ScriptedListClient([.failure, .notModified])
        let c = coordinator(fake)
        let poller = MacSessionPoller()

        await poller.tick(inputs(c, context))
        XCTAssertNotNil(c.lastError, "the failure is reported")
        XCTAssertEqual(c.sessionRefreshFailures, 1)

        await poller.tick(inputs(c, context))
        XCTAssertNil(c.lastError, "and cleared by the next completed round trip")
        XCTAssertEqual(c.sessionRefreshFailures, 0)
    }

    // MARK: - The loop

    /// The loop ticks on its cadence, measured from the last pull, and stops when its task is
    /// cancelled. Driven with a 50 ms "frontmost" interval through the injected clock: the rule
    /// says 30 s, so the clock is advanced 30 s per real tick instead.
    func testTheLoopTicksOnItsCadenceAndStopsWhenCancelled() async throws {
        let poller = MacSessionPoller()
        var clock = Date(timeIntervalSince1970: 1_000)
        var refreshes = 0
        let ticked = expectation(description: "three ticks")
        ticked.expectedFulfillmentCount = 3
        ticked.assertForOverFulfill = false
        let loop = Task { @MainActor in
            await poller.run(MacSessionPoller.Inputs(
                isAppActive: { true },
                reachability: { .reachable },
                failures: { 0 },
                lastRefreshStart: { nil },
                refresh: { refreshes += 1; ticked.fulfill() },
                reprobe: {},
                // Each read of the clock moves it 31 s, so every wait the loop computes is
                // already over by the time it is measured.
                now: { clock = clock.addingTimeInterval(31); return clock }))
        }
        await fulfillment(of: [ticked], timeout: 5)
        loop.cancel()
        await loop.value
        let settled = refreshes
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(refreshes, settled, "a cancelled loop does not tick again")
    }

    /// A known-unreachable bridge is not polled, and a nudge after it comes back resumes the
    /// cadence without waiting out the backoff.
    func testUnreachableStopsTheLoopAndANudgeResumesIt() async throws {
        let poller = MacSessionPoller()
        var state: BridgeReachability = .unreachable
        var refreshes = 0
        var clock = Date(timeIntervalSince1970: 1_000)
        let resumed = expectation(description: "ticks again once reachable")
        resumed.assertForOverFulfill = false
        let loop = Task { @MainActor in
            await poller.run(MacSessionPoller.Inputs(
                isAppActive: { true },
                reachability: { state },
                failures: { 0 },
                lastRefreshStart: { nil },
                refresh: { refreshes += 1; resumed.fulfill() },
                reprobe: {},
                now: { clock = clock.addingTimeInterval(31); return clock }))
        }
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(refreshes, 0, "no list pulls while the bridge is known unreachable")
        state = .reachable
        poller.nudge()
        await fulfillment(of: [resumed], timeout: 5)
        loop.cancel()
        await loop.value
    }

    /// Asleep, nothing; awake, the cadence resumes.
    func testSleepStopsTheLoopAndWakeResumesIt() async throws {
        let poller = MacSessionPoller()
        poller.willSleep()
        var refreshes = 0
        var clock = Date(timeIntervalSince1970: 1_000)
        let resumed = expectation(description: "ticks again after wake")
        resumed.assertForOverFulfill = false
        let loop = Task { @MainActor in
            await poller.run(MacSessionPoller.Inputs(
                isAppActive: { true },
                reachability: { .reachable },
                failures: { 0 },
                lastRefreshStart: { nil },
                refresh: { refreshes += 1; resumed.fulfill() },
                reprobe: {},
                now: { clock = clock.addingTimeInterval(31); return clock }))
        }
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(refreshes, 0, "no pulls while asleep")
        poller.didWake()
        await fulfillment(of: [resumed], timeout: 5)
        loop.cancel()
        await loop.value
    }

    // MARK: - Fixtures

    private func coordinator(_ fake: ScriptedListClient) -> MacCoordinator {
        UserDefaults.standard.removeObject(forKey: "sessions.etag")
        return MacCoordinator(
            configStore: MacConfigStore(config: JesseConfig(host: "studio", port: 8765, token: "tok")),
            makeClient: { _ in fake },
            sessionDeletionStore: MacTestFixtures.deletionStore())
    }

    private func inputs(_ c: MacCoordinator, _ context: ModelContext) -> MacSessionPoller.Inputs {
        MacSessionPoller.Inputs(
            isAppActive: { true },
            reachability: { .reachable },
            failures: { c.sessionRefreshFailures },
            lastRefreshStart: { c.lastSessionRefreshStartedAt },
            refresh: { await c.refreshSessions(context: context) },
            reprobe: {})
    }

    private func summary(_ id: String) -> ConversationSummary {
        ConversationSummary(conversationId: id, sessionId: "sess-\(id)",
                            sessionIds: ["sess-\(id)"], lastModified: 1_700_000_000,
                            firstMessage: "hi \(id)")
    }

    private func storedETag() -> String? { UserDefaults.standard.string(forKey: "sessions.etag") }

    private func threads(_ context: ModelContext) -> [JesseThread] {
        (try? context.fetch(FetchDescriptor<JesseThread>())) ?? []
    }
}

/// A list endpoint that answers from a script, one entry per call, and records what it was
/// sent. Everything else is inert: the poll path calls nothing else.
private final class ScriptedListClient: BridgeClientProtocol, @unchecked Sendable {
    enum Answer {
        case notModified
        case list([ConversationSummary], etag: String)
        case failure
    }

    private let lock = NSLock()
    private var script: [Answer]
    private var _etagsSent: [String?] = []
    var listCalls: Int { lock.withLock { _etagsSent.count } }
    var etagsSent: [String?] { lock.withLock { _etagsSent } }

    init(_ script: [Answer]) { self.script = script }

    nonisolated var config: JesseConfig { JesseConfig(host: "studio", port: 8765, token: "tok") }

    nonisolated func listConversations(since: UInt64?, etag: String?) async throws -> ConversationsResult {
        let answer: Answer = lock.withLock {
            _etagsSent.append(etag)
            return script.isEmpty ? .notModified : script.removeFirst()
        }
        switch answer {
        case .notModified: return .notModified
        case let .list(list, etag): return .conversations(list, deleted: [], etag: etag)
        case .failure: throw URLError(.cannotConnectToHost)
        }
    }

    nonisolated func deleteConversation(_ conversationId: String) async throws {}
    nonisolated func sendPrepared(_ request: JesseRequest) async throws -> JesseSendResult { throw JesseError.notConfigured }
    nonisolated func send(mode: JesseMode, text: String, sessionId: String?,
                          conversationId: String, voice: Bool,
                          instructions: String?, floorOverride: String?,
                          attachments: [JesseRequest.Attachment], requestId: String,
                          model: String?, effort: String?, sentFor: String?) async throws -> JesseSendResult {
        throw JesseError.notConfigured
    }
    nonisolated func result(jobId: String) async throws -> JesseResultState { throw JesseError.notConfigured }
    nonisolated func stream(jobId: String) -> AsyncThrowingStream<JesseStreamEvent, Error> {
        AsyncThrowingStream { $0.finish() }
    }
    nonisolated func hydrate(conversationId: String, after cursor: String?) async throws
        -> (turns: [HydratedTurn], nextCursor: String) {
        ([], cursor ?? "")
    }
    nonisolated func title(text: String, conversationId: String?) async -> String? { nil }
    nonisolated func cancelJob(jobId: String) async throws {}
    nonisolated func health() async throws -> BridgeHealth { BridgeHealth(version: nil) }
    nonisolated func fetchDietSnapshot(date: String?) async throws -> DietSnapshot { throw DietFetchError.notConfigured }
    nonisolated func fetchPrompts() async throws -> PromptDefaults { throw JesseError.notConfigured }
}
