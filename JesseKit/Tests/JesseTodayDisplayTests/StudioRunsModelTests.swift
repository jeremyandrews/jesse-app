import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// What each Studio run control offers in each status, the board's badge, the list's order,
/// and the bridge's refusals reaching an alert word for word.
@MainActor
final class StudioRunsModelTests: XCTestCase {

    private static let all: [StudioRunStatus] = [
        .queued, .running, .needsYou, .reported, .failed, .cancelled, .interrupted, .paused,
        .reviewing, .unknown("rebasing"),
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

    /// Resume is the empty continue: a `needs_you` run asked a question, which a blind continue
    /// would answer (the bridge refuses it with a 409), so only an interrupted run offers it.
    func testResumeIsOfferedOnInterruptedOnlyWithAnEmptyComposerOnly() {
        let offered = Self.all.filter { StudioRunSemantics.offersResume($0, composer: "") }
        XCTAssertEqual(offered, [.interrupted])
        XCTAssertFalse(StudioRunSemantics.offersResume(.needsYou, composer: ""))
        XCTAssertTrue(StudioRunSemantics.offersResume(.interrupted, composer: "  \n "))
        XCTAssertFalse(StudioRunSemantics.offersResume(.interrupted, composer: "use main"))
        XCTAssertFalse(StudioRunSemantics.offersResume(.reported, composer: ""))
        XCTAssertEqual(StudioRunSemantics.resumeText, "Continue where you left off.")
    }

    /// The bridge's cancel route accepts a queued, running, paused or reviewing run, and
    /// nothing else.
    func testCancelIsOfferedOnQueuedRunningPausedAndReviewingOnly() {
        let offered = Self.all.filter(StudioRunSemantics.canCancel)
        XCTAssertEqual(offered, [.queued, .running, .paused, .reviewing])
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
        XCTAssertEqual(polling, [.queued, .running, .needsYou, .interrupted, .paused, .reviewing])
    }

    func testTheDetailModelFollowsTheSemanticsAndTheComposer() {
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .needsYou),
                                         makeClient: { FakeRuns() })
        XCTAssertTrue(model.isComposerEnabled)
        XCTAssertFalse(model.showsResume, "a question is answered, never continued blind")
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

    // MARK: - Reviewing

    private func reviewing(_ id: String = "XR", label: String? = "GLM 5.3",
                           reviews: [StudioRunReview] = [], lastLine: String? = "Reading diff",
                           strand: String? = "Gd") -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: id, status: .reviewing,
                  createdAt: "2026-11-09T10:00:00.000Z", lastLine: lastLine,
                  reviewer: "glm-5.3", reviewerLabel: label, reviews: reviews)
    }

    func testReviewingIsDrawnAsItsOwnStatusInTheRunningTint() {
        XCTAssertEqual(StudioRunSemantics.label(.reviewing), "Reviewing")
        XCTAssertEqual(StudioRunSemantics.symbol(.reviewing), "magnifyingglass.circle.fill")
        XCTAssertEqual(StudioRunSemantics.tint(.reviewing), StudioRunSemantics.tint(.running))
    }

    /// No composer, no Resume, one line where the composer would be; Cancel, as the bridge's
    /// cancel route takes a reviewing run; no Archive, as the bridge refuses one.
    func testAReviewingRunOffersNoReplyNoResumeButCancel() {
        XCTAssertFalse(StudioRunSemantics.canReply(.reviewing))
        XCTAssertFalse(StudioRunSemantics.showsComposer(.reviewing))
        XCTAssertTrue(Self.all.filter { $0 != .reviewing }.allSatisfy(StudioRunSemantics.showsComposer))
        XCTAssertFalse(StudioRunSemantics.offersResume(.reviewing, composer: ""))
        XCTAssertTrue(StudioRunSemantics.canCancel(.reviewing))
        XCTAssertTrue(StudioRunSemantics.keepsPolling(.reviewing))
        XCTAssertFalse(StudioRunSemantics.canArchive(reviewing()))
        XCTAssertEqual(StudioRunSemantics.replyDisabledReason(reviewing(), now: now),
                       "Being reviewed. The findings go to the run itself; nothing to reply to yet.")
        XCTAssertTrue(StudioRunSemantics.cancelMessage(.reviewing).hasPrefix("The review is stopped"))

        let model = StudioRunDetailModel(runID: "XR", run: reviewing(), makeClient: { FakeRuns() })
        model.composer = "hello"
        XCTAssertFalse(model.showsComposer)
        XCTAssertFalse(model.isComposerEnabled)
        XCTAssertFalse(model.canSend)
        XCTAssertFalse(model.showsResume)
        XCTAssertTrue(model.canCancel)
        XCTAssertFalse(model.canArchive)
        XCTAssertTrue(model.keepsPolling)
    }

    /// While reviewing, the row's second line names the reviewer instead of the log's last
    /// line: the newest review's label, else the run's, else its reviewer id.
    func testTheRowsSecondLineNamesTheReviewer() {
        XCTAssertEqual(StudioRunSemantics.movingLine(reviewing()), "Reviewing with GLM 5.3")
        let requested = reviewing(reviews: [StudioRunReview(n: 1, reviewer: "codex-write",
                                                            reviewerLabel: "Codex")])
        XCTAssertEqual(StudioRunSemantics.movingLine(requested), "Reviewing with Codex")
        XCTAssertEqual(StudioRunSemantics.movingLine(reviewing(label: nil)),
                       "Reviewing with glm-5.3")
        var running = reviewing()
        running.status = .running
        XCTAssertEqual(StudioRunSemantics.movingLine(running), "Reading diff")
        running.status = .reported
        XCTAssertNil(StudioRunSemantics.movingLine(running))
    }

    func testAReviewingRunIsLiveSortsFirstAndReplacesRunOnStudio() {
        let runs = [run("done", .reported, created: "2026-11-09T12:00:00.000Z"), reviewing()]
        XCTAssertEqual(StudioRunSemantics.sorted(runs).map(\.runID), ["XR", "done"])
        XCTAssertEqual(StudioRunSemantics.liveRun(strand: "Gd", itemID: "XR", in: runs)?.runID,
                       "XR")
    }

    /// The Moving filter shows a reviewing run, Finished and Needs me do not, and a project's
    /// counts include it under Moving.
    func testTheMovingFilterAndItsCountIncludeAReviewingRun() async {
        let fake = FakeRuns()
        fake.list = StudioRunList(
            runs: [reviewing(), run("R", .running), run("D", .reported), run("Q", .needsYou)],
            counts: StudioRunCounts(running: 1, needsYou: 1, needsMe: 1, moving: 2, finished: 1,
                                    archived: 0))
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        XCTAssertEqual(model.runs(filter: .moving, strand: "Gd").map(\.runID), ["XR", "R"])
        XCTAssertFalse(model.runs(filter: .finished, strand: "Gd").contains { $0.runID == "XR" })
        XCTAssertFalse(model.runs(filter: .needsMe, strand: "Gd").contains { $0.runID == "XR" })
        XCTAssertEqual(model.count(filter: .moving, strand: "Gd"), 2)
        XCTAssertEqual(model.count(filter: .finished, strand: "Gd"), 1)
        XCTAssertEqual(model.count(filter: .all, strand: "Gd"), 4)
        XCTAssertEqual(model.count(filter: .moving, strand: nil), 2, "the bridge's count")
        XCTAssertEqual(WorkSemantics.moving("Gd", in: model.runs).map(\.runID), ["XR", "R"])
    }

    func testTheReviewSectionLinesNameTheReviewerTheStatusAndTheFinishTime() {
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        let answered = StudioRunReview(n: 1, reviewer: "codex-write", reviewerLabel: "Codex",
                                       status: .answered, finishedAt: "2026-11-09T16:02:00.000Z",
                                       path: "Inbox/archive/2026-11-09-run-XR-review-1.md")
        XCTAssertEqual(StudioRunSemantics.reviewLine(answered, now: now, calendar: utc),
                       "Codex \u{00B7} Answered \u{00B7} 16:02")
        let running = StudioRunReview(n: 2, reviewer: "glm-5.3", status: .running)
        XCTAssertEqual(StudioRunSemantics.reviewLine(running, now: now, calendar: utc),
                       "glm-5.3 \u{00B7} Reviewing")
        XCTAssertEqual(StudioRunSemantics.reviewLabel(.unknown("re_reading")), "re_reading")
        XCTAssertEqual(StudioRunSemantics.pendingReviewLine(reviewing()),
                       "GLM 5.3 reviews the work when this round reports.")
        XCTAssertNil(StudioRunSemantics.pendingReviewLine(reviewing(reviews: [running])))
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

    func testTheListShowsEveryRunInEveryStatusAndEachOpensWithItsReport() async throws {
        let fake = FakeRuns()
        let statuses = Self.all
        fake.list = StudioRunList(runs: statuses.enumerated().map { i, status in
            StudioRun(runID: "R\(i)", strand: i % 2 == 0 ? "Gd" : nil, status: status,
                      createdAt: "2026-11-09T1\(i):00:00.000Z",
                      reportPaths: ["Inbox/2026-11-09-run-R\(i).md"])
        })
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        XCTAssertEqual(model.runs(strand: nil).count, statuses.count, "no run is dropped")
        XCTAssertEqual(Set(model.runs(strand: nil).map(\.status)), Set(statuses))
        for i in statuses.indices {
            let detail = model.detailModel(for: "R\(i)")
            XCTAssertEqual(detail.run?.latestReportPath, "Inbox/2026-11-09-run-R\(i).md")
            await detail.refresh()
            XCTAssertNil(detail.loadError)
            XCTAssertEqual(detail.run?.latestReportPath, "Inbox/2026-11-09-run-R\(i).md")
        }
    }
}

/// The run calls and the two last look calls, faked. Shared with `WorkSemanticsTests`.
final class FakeRuns: StudioRunsProviding, @unchecked Sendable {
    var list = StudioRunList()
    var listError: (any Error)?
    var startResult: Result<StudioRunStarted, any Error> = .failure(StudioRunError.other("unset"))
    var replyResult: Result<StudioRun, any Error> = .failure(StudioRunError.other("unset"))
    var cancelResult: Result<StudioRun, any Error> = .failure(StudioRunError.other("unset"))
    var started: [(String, String)] = []
    var replies: [(String, String)] = []

    /// What `?archived=only` answers; `list` is what the default answers.
    var archivedList = StudioRunList()
    var modes: [StudioRunArchivedMode] = []
    var archived: [String] = []
    var unarchived: [String] = []
    /// A refusal for an archive of these runs, in the bridge's words.
    var archiveRefusals: [String: String] = [:]

    func listRuns(archived mode: StudioRunArchivedMode) async throws -> StudioRunList {
        modes.append(mode)
        if let listError { throw listError }
        return mode == .only ? archivedList : list
    }
    func archive(id: String) async throws -> StudioRun {
        if let message = archiveRefusals[id] {
            throw StudioRunError.refused(status: 409, message: message)
        }
        archived.append(id)
        guard var r = list.runs.first(where: { $0.runID == id }) else {
            throw StudioRunError.refused(status: 404, message: "no run \"\(id)\"")
        }
        if r.status == .needsYou || r.status == .interrupted { r.status = .closed }
        r.archivedAt = "2026-11-09T17:00:00.000Z"
        r.archivedReason = "archived by Jeremy"
        list.runs.removeAll { $0.runID == id }
        archivedList.runs.insert(r, at: 0)
        return r
    }
    func unarchive(id: String) async throws -> StudioRun {
        unarchived.append(id)
        guard var r = archivedList.runs.first(where: { $0.runID == id })
                ?? list.runs.first(where: { $0.runID == id }) else {
            throw StudioRunError.refused(status: 404, message: "no run \"\(id)\"")
        }
        r.archivedAt = nil
        r.archivedReason = nil
        archivedList.runs.removeAll { $0.runID == id }
        if !list.runs.contains(where: { $0.runID == id }) { list.runs.append(r) }
        return r
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
    var lastLookValue: String?
    var lastLookError: (any Error)?
    var marks = 0
    func lastLook() async throws -> WorkLastLook {
        if let lastLookError { throw lastLookError }
        return WorkLastLook(lastLook: lastLookValue)
    }
    func markLastLook() async throws -> WorkLastLook {
        marks += 1
        lastLookValue = "2026-11-09T17:00:00.000Z"
        return WorkLastLook(lastLook: lastLookValue)
    }
}
