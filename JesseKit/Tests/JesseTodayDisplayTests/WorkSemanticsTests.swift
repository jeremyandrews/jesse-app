import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// The Work view's decisions: the board row's one status line and its precedence, Send and
/// Resume, Start on the first Next line only, and the `Since your last look` count.
@MainActor
final class WorkSemanticsTests: XCTestCase {

    private func run(_ id: String, _ status: StudioRunStatus, strand: String? = "Gd",
                     item: String? = nil, created: String = "2026-11-09T10:00:00.000Z",
                     finished: String? = nil, title: String? = nil,
                     reportLine: String? = nil) -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: item ?? id, status: status,
                  createdAt: created, finishedAt: finished,
                  reportPaths: reportLine == nil ? [] : ["Inbox/2026-11-09-run-\(id).md"],
                  title: title, reportLine: reportLine)
    }

    private func draft(_ id: String, section: String = "queue", checked: Bool = false,
                       title: String? = nil, startable: Bool? = nil,
                       reason: String? = nil) -> StrandDraft {
        StrandDraft(id: id, text: "\(id) text.", section: section, checked: checked,
                    studioRunnable: startable ?? false, title: title ?? "\(id) title",
                    startable: startable, startableReason: reason)
    }

    private func strand(needsYou: Int? = 0, active: Int? = 0, waiting: String? = nil,
                        drafts: [StrandDraft]? = nil) -> Strand {
        Strand(slug: "Gd", title: "Gd", waiting: waiting.map { StrandWaiting(text: $0) },
               runsActive: active, runsNeedsYou: needsYou, drafts: drafts)
    }

    // MARK: - The row's status line

    func testYourTurnComesFirstForAQuestionACutOffRunAFailureOrAYouGate() {
        let next = [draft("A1")]
        let moving = [run("M", .running)]
        XCTAssertEqual(WorkSemantics.rowStatus(strand(needsYou: 1, active: 2, drafts: next),
                                               runs: moving), .yourTurn)
        XCTAssertEqual(WorkSemantics.rowStatus(strand(drafts: next),
                                               runs: moving + [run("I", .interrupted)]),
                       .yourTurn)
        XCTAssertEqual(WorkSemantics.rowStatus(strand(drafts: next),
                                               runs: moving + [run("F", .failed)]),
                       .yourTurn)
        XCTAssertEqual(WorkSemantics.rowStatus(strand(waiting: "you: create the repo",
                                                      drafts: next), runs: moving),
                       .yourTurn)
        XCTAssertEqual(WorkSemantics.rowStatus(strand(waiting: "You: say yes", drafts: next),
                                               runs: []), .yourTurn)
        XCTAssertEqual(WorkStatusText.of(.yourTurn), "Your turn")
        XCTAssertTrue(WorkRowStatus.yourTurn.isYourTurn)
    }

    func testAGateOnSomeoneElseIsNotYourTurn() {
        XCTAssertEqual(WorkSemantics.rowStatus(strand(waiting: "provider key",
                                                      drafts: [draft("A1")]), runs: []),
                       .next("A1 title"))
    }

    func testAFailureAlreadyRetriedIsNotYourTurn() {
        let runs = [
            run("GD5", .failed, item: "GD5", created: "2026-11-09T09:00:00.000Z"),
            run("GD5b", .reported, item: "GD5", created: "2026-11-09T11:00:00.000Z"),
        ]
        XCTAssertNil(WorkSemantics.rowStatus(strand(drafts: []), runs: runs))
    }

    func testMovingComesSecondAndCountsQueuedRunningAndPaused() {
        let runs = [run("A", .running), run("B", .queued), run("C", .paused),
                    run("D", .reported), run("E", .running, strand: "Other")]
        let status = WorkSemantics.rowStatus(strand(active: 3, drafts: [draft("A1")]),
                                             runs: runs)
        XCTAssertEqual(status, .moving(3))
        XCTAssertEqual(status?.text, "3 moving")
        XCTAssertEqual(status?.isYourTurn, false)
        // Before the runs list has loaded, the snapshot's own counts stand in.
        XCTAssertEqual(WorkSemantics.rowStatus(strand(needsYou: 0, active: 2), runs: [],
                                               runsLoaded: false), .moving(2))
    }

    func testNextComesThirdAndSkipsTickedAndLaterLines() {
        let drafts = [
            draft("A0", checked: true),
            draft("L1", section: "later"),
            draft("A1", title: "Beta on k3s through Flux"),
        ]
        let status = WorkSemantics.rowStatus(strand(drafts: drafts), runs: [])
        XCTAssertEqual(status, .next("Beta on k3s through Flux"))
        XCTAssertEqual(status?.text, "Next: Beta on k3s through Flux")
        // A bridge with no titles: the line's own text.
        let untitled = StrandDraft(id: "A2", text: "Do the thing.")
        XCTAssertEqual(WorkSemantics.rowStatus(strand(drafts: [untitled]), runs: []),
                       .next("Do the thing."))
    }

    func testNothingToSayIsNoLine() {
        XCTAssertNil(WorkSemantics.rowStatus(strand(drafts: [draft("L", section: "later")]),
                                             runs: []))
    }

    // MARK: - Send and Resume

    func testSendNeedsTextAndARunThatTakesAnAnswer() {
        XCTAssertFalse(WorkSemantics.canSend(.needsYou, text: ""))
        XCTAssertFalse(WorkSemantics.canSend(.needsYou, text: "  \n "))
        XCTAssertTrue(WorkSemantics.canSend(.needsYou, text: "main"))
        XCTAssertFalse(WorkSemantics.canSend(.running, text: "main"))
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .needsYou),
                                         makeClient: { FakeRuns() })
        XCTAssertFalse(model.canSend)
        model.composer = "use main"
        XCTAssertTrue(model.canSend)
    }

    func testResumeIsOfferedOnInterruptedOnly() {
        let statuses: [StudioRunStatus] = [.queued, .running, .needsYou, .reported, .failed,
                                           .cancelled, .interrupted, .paused, .unknown("closed")]
        let offered = statuses.filter { status in
            StudioRunDetailModel(runID: "X", run: run("X", status),
                                 makeClient: { FakeRuns() }).showsResume
        }
        XCTAssertEqual(offered, [.interrupted])
    }

    func testARefusedAnswerKeepsItsTextAndShowsTheBridgesWords() async {
        let fake = FakeRuns()
        let model = StudioRunDetailModel(runID: "GD5", run: run("GD5", .needsYou),
                                         makeClient: { fake })
        model.composer = "main"
        fake.replyResult = .failure(StudioRunError.refused(
            status: 409, message: "GD5 asked a question and needs an answer"))
        let sent = await model.send()
        XCTAssertFalse(sent)
        XCTAssertEqual(model.composer, "main")
        XCTAssertEqual(model.alert, "GD5 asked a question and needs an answer")
    }

    // MARK: - Start on Next

    func testStartIsOfferedOnlyOnAStartableFirstNextLine() {
        let first = draft("A1", startable: true)
        let second = draft("A2", startable: true)
        XCTAssertTrue(WorkSemantics.offersStart(first, at: 0))
        XCTAssertFalse(WorkSemantics.offersStart(second, at: 1), "only the first line")
        XCTAssertFalse(WorkSemantics.offersStart(draft("A3", startable: false, reason: "Waits on A1."), at: 0))
        XCTAssertFalse(WorkSemantics.offersStart(draft("A4", startable: nil), at: 0),
                       "an older bridge says nothing, so nothing is offered")
        XCTAssertNil(WorkSemantics.startNote(first, at: 0))
        XCTAssertEqual(WorkSemantics.startNote(draft("A3", startable: false,
                                                     reason: "Waits on A1."), at: 0),
                       "Waits on A1.")
        XCTAssertNil(WorkSemantics.startNote(draft("A5", startable: false, reason: "x"), at: 1))
    }

    func testNextIsTheFirstThreeOpenLinesOutsideLater() {
        let drafts = [draft("A1"), draft("L1", section: "later"), draft("A2", checked: true),
                      draft("A3"), draft("A4", section: "running"), draft("A5"),
                      draft("D1", section: "done")]
        XCTAssertEqual(WorkSemantics.nextLines(strand(drafts: drafts)).map(\.id),
                       ["A1", "A3", "A4"])
    }

    // MARK: - Since your last look

    func testSinceYourLastLookCountsRunsFinishedAfterIt() {
        let look = "2026-11-09T12:00:00.000Z"
        let runs = [
            run("before", .reported, finished: "2026-11-09T11:00:00.000Z"),
            run("after", .reported, finished: "2026-11-09T13:00:00.000Z",
                reportLine: "Merged."),
            run("asks", .needsYou, finished: "2026-11-09T14:00:00Z",
                reportLine: "NEEDS YOU: main?"),
            run("broke", .failed, strand: "Other", finished: "2026-11-09T15:00:00.000Z"),
            run("moving", .running),
            run("cancelled", .cancelled, finished: "2026-11-09T16:00:00.000Z"),
        ]
        let since = WorkSemantics.finishedSince(look, in: runs)
        XCTAssertEqual(since.map(\.runID), ["broke", "asks", "after"])
        XCTAssertEqual(WorkSemantics.sinceLine(count: since.count),
                       "3 finished since your last look")
        XCTAssertEqual(WorkSemantics.finishedSince(look, in: runs, strand: "Gd").map(\.runID),
                       ["asks", "after"])
        XCTAssertNil(WorkSemantics.sinceLine(count: 0))
        XCTAssertEqual(WorkSemantics.finishedSince(nil, in: runs), [], "no look, no baseline")
    }

    func testOpeningWorkPostsTheLastLookButKeepsThisVisitsBaseline() async {
        let fake = FakeRuns()
        fake.lastLookValue = "2026-11-09T12:00:00.000Z"
        fake.list = StudioRunList(runs: [
            run("after", .reported, finished: "2026-11-09T13:00:00.000Z"),
        ])
        let model = StudioRunsModel(makeClient: { fake })
        await model.loadLastLook()
        await model.load()
        XCTAssertEqual(model.finishedSinceLastLook().count, 1)
        await model.markLastLook()
        XCTAssertEqual(fake.marks, 1)
        XCTAssertEqual(model.lastLook, "2026-11-09T12:00:00.000Z",
                       "this visit still compares against the look it opened with")
        XCTAssertEqual(model.finishedSinceLastLook().count, 1)
        // Once per visit.
        await model.markLastLook()
        XCTAssertEqual(fake.marks, 1)
    }

    func testABridgeWithoutTheLastLookShowsNoLine() async {
        let fake = FakeRuns()
        fake.lastLookError = StudioRunError.refused(status: 404, message: "not found")
        let model = StudioRunsModel(makeClient: { fake })
        await model.loadLastLook()
        XCTAssertFalse(model.lastLookLoaded)
        XCTAssertNil(model.lastLook)
    }

    // MARK: - A run's words

    func testTheQuestionIsTheReportLineWithoutItsPrefix() {
        XCTAssertEqual(WorkSemantics.question(run("Q", .needsYou,
                                                  reportLine: "NEEDS YOU: main or release?")),
                       "main or release?")
        XCTAssertEqual(WorkSemantics.headline(run("R", .reported, reportLine: "Merged.")),
                       "Merged.")
        XCTAssertNil(WorkSemantics.question(run("N", .needsYou)))
        XCTAssertEqual(WorkSemantics.title(run("X1", .running, title: "Ship it")), "Ship it")
        XCTAssertEqual(WorkSemantics.title(StudioRun(runID: "X2", itemText: "Do it.")), "Do it.")
        XCTAssertEqual(WorkSemantics.title(StudioRun(runID: "X3")), "Untitled run")
    }

    func testTheYouGateReadsAsAPlainLine() {
        XCTAssertEqual(WorkSemantics.youGate(strand(waiting: "you: create the empty repo")),
                       "Create the empty repo")
        XCTAssertNil(WorkSemantics.youGate(strand(waiting: "A1c")))
        XCTAssertNil(WorkSemantics.youGate(strand()))
    }

    func testTheYourTurnSectionPutsQuestionsFirst() {
        let runs = [run("F", .failed, created: "2026-11-09T12:00:00.000Z"),
                    run("Q", .needsYou, created: "2026-11-09T10:00:00.000Z"),
                    run("I", .interrupted, created: "2026-11-09T11:00:00.000Z"),
                    run("R", .reported)]
        XCTAssertEqual(WorkSemantics.yourTurnRuns("Gd", in: runs).map(\.runID), ["Q", "I", "F"])
    }
}

/// The row's words, by name, so a test reads like the screen.
private enum WorkStatusText {
    static func of(_ status: WorkRowStatus) -> String { status.text }
}
