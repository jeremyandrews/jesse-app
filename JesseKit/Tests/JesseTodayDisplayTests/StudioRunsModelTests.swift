import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// What each Studio run control offers in each status, the board's badge, the list's order,
/// and the bridge's refusals reaching an alert word for word.
@MainActor
final class StudioRunsModelTests: XCTestCase {

    private static let all: [StudioRunStatus] = [
        .queued, .running, .needsYou, .reported, .failed, .cancelled, .interrupted, .paused,
        .unknown("rebasing"),
    ]

    private let now = Date(timeIntervalSince1970: 1_794_243_600) // 2026-11-09T17:00:00Z

    private func run(_ id: String, _ status: StudioRunStatus, strand: String? = "Gd",
                     item: String? = nil, created: String = "2026-11-09T10:00:00.000Z",
                     resumeAt: String? = nil) -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: item ?? id, status: status,
                  createdAt: created, resumeAt: resumeAt)
    }

    // MARK: - Enablement per status

    func testReplyIsEnabledForNeedsYouInterruptedAndReportedOnly() {
        let enabled = Self.all.filter(StudioRunSemantics.canReply)
        XCTAssertEqual(enabled, [.needsYou, .reported, .interrupted])
    }

    func testResumeIsOfferedOnInterruptedAndNeedsYouWithAnEmptyComposerOnly() {
        let offered = Self.all.filter { StudioRunSemantics.offersResume($0, composer: "") }
        XCTAssertEqual(offered, [.needsYou, .interrupted])
        XCTAssertTrue(StudioRunSemantics.offersResume(.interrupted, composer: "  \n "))
        XCTAssertFalse(StudioRunSemantics.offersResume(.interrupted, composer: "use main"))
        XCTAssertFalse(StudioRunSemantics.offersResume(.reported, composer: ""))
        XCTAssertEqual(StudioRunSemantics.resumeText, "Continue where you left off.")
    }

    func testCancelIsOfferedOnQueuedRunningAndPausedOnly() {
        let offered = Self.all.filter(StudioRunSemantics.canCancel)
        XCTAssertEqual(offered, [.queued, .running, .paused])
    }

    func testEveryDisabledComposerSaysWhyInOneLineAndAnEnabledOneSaysNothing() {
        for status in Self.all {
            let reason = StudioRunSemantics.replyDisabledReason(run("X", status), now: now)
            if StudioRunSemantics.canReply(status) {
                XCTAssertNil(reason, "\(status)")
            } else {
                let line = try? XCTUnwrap(reason, "\(status)")
                XCTAssertFalse(line?.isEmpty ?? true)
                XCTAssertFalse(line?.contains("\n") ?? true, "\(status)")
            }
        }
        let paused = run("X", .paused, resumeAt: "2026-11-09T19:30:00.000Z")
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        XCTAssertEqual(StudioRunSemantics.clock(paused.resumeAt, now: now, calendar: utc), "19:30")
        XCTAssertTrue(StudioRunSemantics.replyDisabledReason(paused, now: now)?
            .hasPrefix("Paused for the usage limit. It continues by itself at ") ?? false)
    }

    func testPollingStopsOnlyOnceTheRunIsTerminal() {
        let polling = Self.all.filter(StudioRunSemantics.keepsPolling)
        XCTAssertEqual(polling, [.queued, .running, .needsYou, .interrupted, .paused])
    }

    func testTheDetailModelFollowsTheSemanticsAndTheComposer() {
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .needsYou),
                                         makeClient: { FakeRuns() })
        XCTAssertTrue(model.isComposerEnabled)
        XCTAssertTrue(model.showsResume)
        XCTAssertFalse(model.canSend, "nothing typed")
        XCTAssertFalse(model.canCancel)
        model.composer = "main"
        XCTAssertTrue(model.canSend)
        XCTAssertFalse(model.showsResume)

        let running = StudioRunDetailModel(runID: "R", run: run("R", .running),
                                           makeClient: { FakeRuns() })
        running.composer = "hello"
        XCTAssertFalse(running.isComposerEnabled)
        XCTAssertFalse(running.canSend)
        XCTAssertTrue(running.canCancel)
        XCTAssertNotNil(running.replyDisabledReason)
    }

    // MARK: - The badge

    func testTheBadgeCountsActiveRunsAndSaysHowManyNeedYou() {
        XCTAssertNil(StudioRunSemantics.badge(active: nil, needsYou: nil), "an older bridge")
        XCTAssertNil(StudioRunSemantics.badge(active: 0, needsYou: 0))
        let two = StudioRunSemantics.badge(active: 2, needsYou: 1)
        XCTAssertEqual(two, StudioRunBadge(text: "2", needsYou: true,
                                           accessibilityLabel: "2 Studio runs, 1 needs you"))
        let one = StudioRunSemantics.badge(active: 1, needsYou: 0)
        XCTAssertEqual(one, StudioRunBadge(text: "1", needsYou: false,
                                           accessibilityLabel: "1 Studio run"))
        XCTAssertEqual(StudioRunSemantics.badge(active: 3, needsYou: nil)?.needsYou, false)
        XCTAssertEqual(StudioRunSemantics.badge(active: 2, needsYou: 2)?.accessibilityLabel,
                       "2 Studio runs, 2 need you")
        let strand = Strand(slug: "Gd", runsActive: 2, runsNeedsYou: 1)
        XCTAssertEqual(StudioRunSemantics.badge(for: strand)?.accessibilityLabel,
                       "2 Studio runs, 1 needs you")
    }

    // MARK: - Order, age, and the live run of a line

    func testLiveRunsComeFirstThenTheRestNewestFirst() {
        let runs = [
            run("old-done", .reported, created: "2026-11-01T00:00:00.000Z"),
            run("new-done", .failed, created: "2026-11-09T12:00:00.000Z"),
            run("old-live", .paused, created: "2026-11-02T00:00:00.000Z"),
            run("new-live", .running, created: "2026-11-08T00:00:00.000Z"),
            run("waiting", .needsYou, created: "2026-11-05T00:00:00.000Z"),
        ]
        XCTAssertEqual(StudioRunSemantics.sorted(runs).map(\.runID),
                       ["new-live", "waiting", "old-live", "new-done", "old-done"])
    }

    func testAgeReadsInTheLargestWholeUnit() {
        XCTAssertEqual(StudioRunSemantics.age("2026-11-09T16:59:30.000Z", now: now), "just now")
        XCTAssertEqual(StudioRunSemantics.age("2026-11-09T16:48:00.000Z", now: now), "12m")
        XCTAssertEqual(StudioRunSemantics.age("2026-11-09T14:00:00Z", now: now), "3h")
        XCTAssertEqual(StudioRunSemantics.age("2026-11-07T17:00:00.000Z", now: now), "2d")
        XCTAssertNil(StudioRunSemantics.age("not a date", now: now))
    }

    func testOnlyALiveRunOfTheSameStrandAndLineReplacesRunOnStudio() {
        let runs = [
            run("GD5", .reported, item: "GD5"),
            run("GD5b", .needsYou, item: "GD5"),
            run("X1", .running, strand: "Other", item: "GD7"),
        ]
        XCTAssertEqual(StudioRunSemantics.liveRun(strand: "Gd", itemID: "GD5", in: runs)?.runID,
                       "GD5b")
        XCTAssertNil(StudioRunSemantics.liveRun(strand: "Gd", itemID: "GD7", in: runs))
        XCTAssertNil(StudioRunSemantics.liveRun(strand: "Gd", itemID: "GD9", in: runs))
    }

    // MARK: - The calls

    func testAStartRefusalReachesTheAlertVerbatimAndASuccessReloadsTheList() async {
        let fake = FakeRuns()
        let model = StudioRunsModel(makeClient: { fake })
        fake.startResult = .failure(StudioRunError.refused(
            status: 409, message: "Gd GD5 already has a live run: GD5 (needs_you)"))
        let refused = await model.start(strand: "Gd", itemID: "GD5")
        XCTAssertNil(refused)
        XCTAssertEqual(model.alert, "Gd GD5 already has a live run: GD5 (needs_you)")

        model.alert = nil
        fake.startResult = .success(StudioRunStarted(runID: "GD5b", status: .queued))
        fake.list = StudioRunList(runs: [run("GD5b", .queued, item: "GD5")])
        let started = await model.start(strand: "Gd", itemID: "GD5")
        XCTAssertEqual(started?.runID, "GD5b")
        XCTAssertNil(model.alert)
        XCTAssertEqual(fake.started.last?.0, "Gd")
        XCTAssertEqual(fake.started.last?.1, "GD5")
        XCTAssertEqual(model.liveRun(strand: "Gd", itemID: "GD5")?.runID, "GD5b")
    }

    func testResumeSendsTheContinueTextAndACancelRefusalIsShown() async {
        let fake = FakeRuns()
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .interrupted),
                                         makeClient: { fake })
        fake.replyResult = .success(run("GD5", .queued))
        await model.resume()
        XCTAssertEqual(fake.replies.last?.1, "Continue where you left off.")
        XCTAssertEqual(model.status, .queued)

        fake.cancelResult = .failure(StudioRunError.refused(
            status: 409, message: "GD5 is reported; only a queued, running or paused run can be cancelled"))
        await model.cancel()
        XCTAssertEqual(model.alert,
                       "GD5 is reported; only a queued, running or paused run can be cancelled")
    }

    func testASentReplyClearsTheComposerAndAFailedOneKeepsIt() async {
        let fake = FakeRuns()
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .needsYou),
                                         makeClient: { fake })
        model.composer = "  use main  "
        fake.replyResult = .failure(StudioRunError.refused(status: 400, message: "the reply is empty"))
        await model.send()
        XCTAssertEqual(model.composer, "  use main  ")
        XCTAssertEqual(model.alert, "the reply is empty")
        fake.replyResult = .success(run("GD5", .queued))
        await model.send()
        XCTAssertEqual(fake.replies.last?.1, "use main")
        XCTAssertEqual(model.composer, "")
    }

    func testAFailedListLoadIsShownAndClearedByTheNextGoodOne() async {
        let fake = FakeRuns()
        let model = StudioRunsModel(makeClient: { fake })
        fake.listError = StudioRunError.refused(status: 503, message: "[hostrun] is not enabled on this bridge")
        await model.load()
        XCTAssertEqual(model.loadError, "[hostrun] is not enabled on this bridge")
        fake.listError = nil
        fake.list = StudioRunList(runs: [run("A", .reported), run("B", .running)])
        await model.load()
        XCTAssertNil(model.loadError)
        XCTAssertEqual(model.runs.map(\.runID), ["B", "A"])
        XCTAssertEqual(model.runs(strand: "Gd").count, 2)
        XCTAssertEqual(model.runs(strand: "Other").count, 0)
    }
}

/// The six calls, faked.
private final class FakeRuns: StudioRunsProviding, @unchecked Sendable {
    var list = StudioRunList()
    var listError: (any Error)?
    var startResult: Result<StudioRunStarted, any Error> = .failure(StudioRunError.other("unset"))
    var replyResult: Result<StudioRun, any Error> = .failure(StudioRunError.other("unset"))
    var cancelResult: Result<StudioRun, any Error> = .failure(StudioRunError.other("unset"))
    var started: [(String, String)] = []
    var replies: [(String, String)] = []

    func listRuns() async throws -> StudioRunList {
        if let listError { throw listError }
        return list
    }
    func startRun(strand: String, itemID: String) async throws -> StudioRunStarted {
        started.append((strand, itemID))
        return try startResult.get()
    }
    func getRun(id: String) async throws -> StudioRun {
        guard let r = list.runs.first(where: { $0.runID == id }) else {
            throw StudioRunError.refused(status: 404, message: "no run \"\(id)\"")
        }
        return r
    }
    func runLog(id: String, tail: Int?) async throws -> StudioRunLog {
        StudioRunLog(runID: id, lines: ["one", "two"])
    }
    func replyToRun(id: String, text: String) async throws -> StudioRun {
        replies.append((id, text))
        return try replyResult.get()
    }
    func cancelRun(id: String) async throws -> StudioRun {
        try cancelResult.get()
    }
}
