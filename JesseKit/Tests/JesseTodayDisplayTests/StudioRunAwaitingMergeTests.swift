import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// `awaiting_merge` (DJ12) on screen: its label, symbol and tint, its place beside the runs that
/// need their owner (the Needs me filter, the list's order, Your turn), what its detail offers
/// (as the bridge's routes accept: reply and archive yes, cancel no, an empty continue not
/// offered), and the pull request link on its row and detail.
@MainActor
final class StudioRunAwaitingMergeTests: XCTestCase {

    private let now = Date(timeIntervalSince1970: 1_795_511_400) // 2026-11-24T09:30:00Z

    private static let wait = StudioRunMergeWait(
        url: "https://github.com/jeremyandrews/jesse-app/pull/362", owner: "jeremyandrews",
        repo: "jesse-app", number: 362,
        title: "Bridge 0.217.1: read a quoted devjob-priority followed by a comment",
        since: "2026-11-24T09:12:00Z", lastCheckAt: "2026-11-24T09:17:00Z",
        lastPushAt: "2026-11-24T09:12:00Z")

    private func run(_ id: String, _ status: StudioRunStatus, strand: String = "Gd",
                     created: String = "2026-11-09T10:00:00.000Z",
                     mergeWait: StudioRunMergeWait? = nil,
                     archivedAt: String? = nil) -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: id, status: status, createdAt: created,
                  title: "Run \(id)", archivedAt: archivedAt, mergeWait: mergeWait)
    }

    // MARK: - The look

    func testItReadsAwaitingYourMergeInTheNeedsYouColour() {
        XCTAssertEqual(StudioRunSemantics.label(.awaitingMerge), "Awaiting your merge")
        XCTAssertEqual(StudioRunSemantics.symbol(.awaitingMerge), "arrow.triangle.merge")
        XCTAssertEqual(StudioRunSemantics.tint(.awaitingMerge),
                       StudioRunSemantics.tint(.needsYou))
        XCTAssertEqual(StudioRunSemantics.statusTint(run("M", .awaitingMerge)), .orange)
    }

    // MARK: - What it offers

    /// A reply continues it (an empty `Continue where you left off.` is accepted by the bridge,
    /// but the app keeps it like a reported run: typed text sends, no Resume); Archive closes it
    /// first, as a needs_you run; Cancel is refused, as for needs_you.
    func testItOffersReplyAndArchiveButNotCancelOrResume() {
        XCTAssertTrue(StudioRunSemantics.canReply(.awaitingMerge))
        XCTAssertTrue(StudioRunSemantics.showsComposer(.awaitingMerge))
        XCTAssertFalse(StudioRunSemantics.offersResume(.awaitingMerge, composer: ""))
        XCTAssertEqual(StudioRunSemantics.offersResume(.awaitingMerge, composer: ""),
                       StudioRunSemantics.offersResume(.reported, composer: ""))
        XCTAssertFalse(StudioRunSemantics.canCancel(.awaitingMerge))
        XCTAssertTrue(StudioRunSemantics.keepsPolling(.awaitingMerge),
                      "the bridge resumes it by itself when the pull request merges")
        XCTAssertNil(StudioRunSemantics.replyDisabledReason(run("M", .awaitingMerge), now: now))
        XCTAssertTrue(StudioRunSemantics.canArchive(run("M", .awaitingMerge)))
        XCTAssertFalse(StudioRunSemantics.canUnarchive(run("M", .awaitingMerge)))
        XCTAssertFalse(run("M", .awaitingMerge, archivedAt: "2026-11-24T09:00:00Z").isArchived,
                       "waiting on its owner, it is in the list even with archived_at set")
        XCTAssertTrue(WorkSemantics.countsAsFinished(.awaitingMerge))

        let model = StudioRunDetailModel(runID: "M", run: run("M", .awaitingMerge, mergeWait: Self.wait),
                                         makeClient: { FakeRuns() })
        XCTAssertTrue(model.showsComposer)
        XCTAssertTrue(model.isComposerEnabled)
        XCTAssertFalse(model.showsResume)
        XCTAssertFalse(model.canSend, "nothing typed")
        XCTAssertFalse(model.canCancel)
        XCTAssertTrue(model.canArchive)
        XCTAssertTrue(model.keepsPolling)
        model.composer = "Merged, carry on."
        XCTAssertTrue(model.canSend)
    }

    func testAReplyToAnAwaitingMergeRunIsSentAndItsAnswerAdopted() async {
        let fake = FakeRuns()
        fake.replyResult = .success(run("M", .queued))
        let model = StudioRunDetailModel(runID: "M", run: run("M", .awaitingMerge, mergeWait: Self.wait),
                                         makeClient: { fake })
        model.composer = "I merged it; continue."
        let sent = await model.send()
        XCTAssertTrue(sent)
        XCTAssertEqual(fake.replies.last?.1, "I merged it; continue.")
        XCTAssertEqual(model.status, .queued)
        XCTAssertEqual(model.composer, "")
    }

    // MARK: - Filter and order

    func testItIsUnderNeedsMeAndNotMovingOrFinished() async {
        let fake = FakeRuns()
        fake.list = StudioRunList(runs: [
            run("Q", .needsYou), run("M", .awaitingMerge, mergeWait: Self.wait),
            run("R", .running), run("D", .reported), run("I", .interrupted),
        ])
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        let ids: (StudioRunFilter) -> Set<String> = {
            Set(model.runs(filter: $0, strand: "Gd").map(\.runID))
        }
        XCTAssertEqual(ids(.needsMe), ["Q", "M", "I"])
        XCTAssertFalse(ids(.moving).contains("M"))
        XCTAssertFalse(ids(.finished).contains("M"))
        XCTAssertTrue(ids(.all).contains("M"))
        XCTAssertFalse(ids(.archived).contains("M"))
    }

    /// Live first: an awaiting-merge run sorts with the live runs (needs you among them),
    /// ahead of every settled one, newest first within.
    func testItSortsWithTheLiveRuns() {
        let runs = [
            run("Old-done", .reported, created: "2026-11-24T09:00:00.000Z"),
            run("Merge", .awaitingMerge, created: "2026-11-01T09:00:00.000Z",
                mergeWait: Self.wait),
            run("Ask", .needsYou, created: "2026-11-02T09:00:00.000Z"),
            run("Failed", .failed, created: "2026-11-23T09:00:00.000Z"),
        ]
        XCTAssertEqual(StudioRunSemantics.sorted(runs).map(\.runID),
                       ["Ask", "Merge", "Old-done", "Failed"])
        XCTAssertEqual(StudioRunSemantics.liveRun(strand: "Gd", itemID: "Merge", in: runs)?.runID,
                       "Merge", "a second start of its draft is refused while it waits")
    }

    /// Your turn: questions first, then pull requests to merge, then cut off runs, then
    /// failures; the row's line names the pull request.
    func testYourTurnListsItAfterQuestionsAndTheRowSaysMergeIt() {
        let runs = [
            run("F", .failed, created: "2026-11-24T09:00:00.000Z"),
            run("I", .interrupted, created: "2026-11-24T08:00:00.000Z"),
            run("M", .awaitingMerge, created: "2026-11-24T07:00:00.000Z", mergeWait: Self.wait),
            run("Q", .needsYou, created: "2026-11-24T06:00:00.000Z"),
        ]
        XCTAssertEqual(WorkSemantics.yourTurnRuns("Gd", in: runs).map(\.runID),
                       ["Q", "M", "I", "F"])
        let strand = Strand(slug: "Gd", title: "Gd", runsActive: 1, runsNeedsYou: 1,
                            drafts: [])
        XCTAssertEqual(WorkSemantics.action(strand, runs: [runs[2]]),
                       "For you: merge jeremyandrews/jesse-app#362")
        XCTAssertEqual(WorkSemantics.rowStatus(strand, runs: [runs[2]]), .yourTurn)
        XCTAssertEqual(WorkSemantics.action(strand, runs: [run("N", .awaitingMerge)]),
                       "For you: merge the pull request for Run N")
        XCTAssertNil(WorkSemantics.doing(strand, runs: [runs[2]], runsLoaded: true),
                     "waiting on its owner, it is not moving")
    }

    // MARK: - The pull request link

    func testTheRowAndDetailLinkThePullRequest() throws {
        let waiting = run("M", .awaitingMerge, mergeWait: Self.wait)
        let link = try XCTUnwrap(StudioRunSemantics.mergeLink(waiting))
        XCTAssertEqual(link.url, URL(string: "https://github.com/jeremyandrews/jesse-app/pull/362"))
        XCTAssertEqual(link.text, "jeremyandrews/jesse-app#362")
        XCTAssertEqual(link.title,
                       "Bridge 0.217.1: read a quoted devjob-priority followed by a comment")
        XCTAssertEqual(link.accessibilityLabel,
                       "Pull request, jeremyandrews/jesse-app#362, Bridge 0.217.1: read a quoted devjob-priority followed by a comment")
        XCTAssertEqual(StudioRunSemantics.rowMergeLink(waiting), link)
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        XCTAssertEqual(StudioRunSemantics.clock(Self.wait.lastCheckAt, now: now, calendar: utc),
                       "09:17")
        XCTAssertTrue(StudioRunSemantics.mergeLine(waiting, now: now)?
            .hasPrefix("Merge it and the run continues by itself. Last checked ") ?? false)
    }

    func testTheLinkFallsBackToTheTitleOrTheURLAndNeedsAWebURL() {
        let titled = run("T", .awaitingMerge, mergeWait: StudioRunMergeWait(
            url: "https://example.com/review/9", title: "  Give dev jobs a second cloud slot "))
        XCTAssertEqual(StudioRunSemantics.mergeLink(titled)?.text,
                       "Give dev jobs a second cloud slot")
        XCTAssertNil(StudioRunSemantics.mergeLink(titled)?.title)
        let bare = run("B", .awaitingMerge,
                       mergeWait: StudioRunMergeWait(url: "https://example.com/review/9"))
        XCTAssertEqual(StudioRunSemantics.mergeLink(bare)?.text, "https://example.com/review/9")
        let fromURL = run("U", .awaitingMerge, mergeWait: StudioRunMergeWait(
            url: "https://github.com/jeremyandrews/k3s/pull/412"))
        XCTAssertEqual(StudioRunSemantics.mergeLink(fromURL)?.text, "jeremyandrews/k3s#412")
        for bad in ["", "not a url", "javascript:alert(1)", "file:///etc/passwd"] {
            let r = run("X", .awaitingMerge, mergeWait: StudioRunMergeWait(url: bad))
            XCTAssertNil(StudioRunSemantics.mergeLink(r), bad)
        }
        XCTAssertNil(StudioRunSemantics.mergeLink(run("N", .awaitingMerge)), "no merge_wait")
        XCTAssertNil(StudioRunSemantics.mergeLink(run("Q", .needsYou)))
    }

    /// Once the wait is over the row drops the link; the detail keeps it, with how it ended.
    func testAnEndedWaitLeavesTheRowAndSaysHowItEndedOnTheDetail() {
        var ended = Self.wait
        ended.ended = "merged at 2026-11-24T10:00:00Z as abc123"
        ended.endedAt = "2026-11-24T10:00:00Z"
        let resumed = run("M", .queued, mergeWait: ended)
        XCTAssertNil(StudioRunSemantics.rowMergeLink(resumed))
        XCTAssertNotNil(StudioRunSemantics.mergeLink(resumed))
        XCTAssertEqual(StudioRunSemantics.mergeLine(resumed, now: now),
                       "Wait ended: merged at 2026-11-24T10:00:00Z as abc123")
        XCTAssertNil(StudioRunSemantics.rowMergeLink(run("R", .reported, mergeWait: Self.wait)))
    }
}
