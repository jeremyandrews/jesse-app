import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// The Work board row's words (HO5), one test per use case: what a project is about (A), what is
/// his to do (B), what is moving (C), what is next when nothing else is (D), a long Waiting on (E).
/// The strings are sanitised shapes of the live board's.
@MainActor
final class WorkRowTextTests: XCTestCase {

    private func run(_ id: String, _ status: StudioRunStatus, strand: String = "P",
                     title: String? = nil, reportLine: String? = nil,
                     created: String = "2026-11-12T06:00:00.000Z") -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: id, status: status, createdAt: created,
                  reportPaths: reportLine == nil ? [] : ["Inbox/r-\(id).md"],
                  title: title ?? "\(id) title", reportLine: reportLine)
    }

    private func strand(headline: String? = nil, yourAction: String? = nil,
                        waiting: String? = nil, servesYourAction: Bool = true,
                        nextTitle: String? = nil, drafts: [StrandDraft]? = [],
                        needsYou: Int = 0) -> Strand {
        Strand(slug: "P", title: "Project", waiting: waiting.map { StrandWaiting(text: $0) },
               runsActive: needsYou, runsNeedsYou: needsYou, drafts: drafts,
               headline: headline, yourAction: yourAction,
               servesYourAction: servesYourAction, nextTitle: nextTitle)
    }

    private func text(_ s: Strand, runs: [StudioRun] = []) -> WorkRowText? {
        WorkSemantics.rowText(s, runs: runs)
    }

    // MARK: A. What the project is about

    func testAHeadlineSitsBesideTheTitle() {
        let t = text(strand(headline: "Two issues drafted, none released",
                            yourAction: "generate the signing keypair",
                            waiting: "you: generate the signing keypair (deferred)."))
        XCTAssertEqual(t?.detail, "Two issues drafted, none released")
        XCTAssertEqual(t?.line, "For you: generate the signing keypair")
        XCTAssertEqual(t?.tone, .you)
        XCTAssertNil(t?.marker, "nothing is moving")
    }

    // MARK: B. What is his to do

    func testARunsQuestionComesFirstWithoutItsAsides() {
        let q = run("K04", .needsYou,
                    reportLine: "NEEDS YOU: Will you grant Local Network to the runner (the job K04 lives in), or run it yourself?")
        let t = text(strand(yourAction: "grant Local Network", waiting: "you: grant it.",
                            needsYou: 1), runs: [q])
        XCTAssertEqual(t?.line,
                       "For you: answer \u{201C}Will you grant Local Network to the runner, or run it yourself?\u{201D}")
        XCTAssertEqual(t?.tone, .you)
    }

    func testACutOffRunAndAFailedOneAreHisToo() {
        XCTAssertEqual(text(strand(), runs: [run("A2", .interrupted, title: "Ablation A2")])?.line,
                       "For you: resume Ablation A2")
        XCTAssertEqual(text(strand(), runs: [run("A3", .failed, title: "HTTP/2")])?.line,
                       "For you: see why HTTP/2 failed")
    }

    func testTheNotesActionIsTheBridgesWords() {
        let t = text(strand(yourAction: "branch protection on release-0.19 matching main's",
                            waiting: "you: branch protection on `release-0.19` (GO197)."))
        XCTAssertEqual(t?.line, "For you: branch protection on release-0.19 matching main's")
        // The project view's Your turn leads with the same words.
        XCTAssertEqual(WorkSemantics.gatePhrase(strand(yourAction: "read the essays",
                                                      waiting: "you: read the essays.")),
                       "read the essays")
    }

    func testAnOlderBridgeFallsBackOnTheGateButANullActionDoesNot() {
        let older = strand(waiting: "you: Create the repo.", servesYourAction: false)
        XCTAssertEqual(text(older)?.line, "For you: create the repo.")
        // The bridge read the gate and found nothing to say: no raw text on the row.
        let served = strand(waiting: "you: PD2.", servesYourAction: true,
                            nextTitle: "The local gate")
        XCTAssertEqual(text(served)?.line, "Next: The local gate")
        XCTAssertEqual(text(served)?.tone, .plain)
    }

    func testACountOfQuestionsWithNoRunListedIsStillHis() {
        // The snapshot says a run waits on him before the runs list has it.
        XCTAssertEqual(text(strand(needsYou: 1))?.line, "For you: answer a question")
        // A waiting run with no question line is answered by name.
        XCTAssertEqual(text(strand(), runs: [run("Q1", .needsYou, title: "Proxy")])?.line,
                       "For you: answer Proxy")
    }

    func testAnOlderBridgesGateKeepsAWordInCapitals() {
        XCTAssertEqual(WorkSemantics.gatePhrase(strand(waiting: "you: SOC 2 renewal.",
                                                      servesYourAction: false)),
                       "SOC 2 renewal.")
        XCTAssertNil(WorkSemantics.gatePhrase(strand(waiting: "you:", servesYourAction: false)))
    }

    func testTheSpokenStateFollowsTheSameRuleAsTheOrangeLine() {
        // The bridge read the gate and found nothing his: no orange line, and no "Your turn".
        let served = strand(waiting: "you: PD2.", servesYourAction: true)
        XCTAssertNil(WorkSemantics.action(served, runs: []))
        XCTAssertNotEqual(WorkSemantics.rowStatus(served, runs: []), .yourTurn)
        let his = strand(yourAction: "read the essays", waiting: "you: read the essays.")
        XCTAssertEqual(WorkSemantics.rowStatus(his, runs: []), .yourTurn)
    }

    // MARK: C. What is moving

    func testBeforeTheRunsLoadTheSnapshotsCountSaysWhatIsMoving() {
        let s = Strand(slug: "P", title: "Project", runsActive: 3, runsNeedsYou: 1, drafts: [])
        let t = WorkSemantics.rowText(s, runs: [], runsLoaded: false)
        XCTAssertEqual(t?.detail, "2 moving")
        XCTAssertEqual(t?.marker, "2 moving")
        let waiting = Strand(slug: "P", title: "Project", runsActive: 1, runsNeedsYou: 1, drafts: [])
        XCTAssertNil(WorkSemantics.doing(waiting, runs: [], runsLoaded: false))
    }

    func testTheRunsBesideTheFirstAreCountedByWhatTheyAreDoing() {
        let two = [run("A", .running, title: "One", created: "2026-11-12T07:00:00.000Z"),
                   run("B", .running, title: "Two", created: "2026-11-12T06:00:00.000Z")]
        XCTAssertEqual(WorkSemantics.doing(strand(), runs: two, runsLoaded: true),
                       "Running: One and 1 more")
        let paused = [run("A", .running, title: "One"), run("B", .paused, title: "Two")]
        XCTAssertEqual(WorkSemantics.doing(strand(), runs: paused, runsLoaded: true),
                       "Running: One, 1 paused")
        XCTAssertEqual(WorkSemantics.doing(strand(), runs: [run("C", .queued, title: "Three")],
                                           runsLoaded: true),
                       "Queued: Three")
    }

    func testSeveralRunningLinesAreCountedAndTheCutTitleLosesItsEllipsis() {
        let drafts = [StrandDraft(id: "S1", text: "x", section: "running",
                                  title: "Targeted rebalance of the pool\u{2026}"),
                      StrandDraft(id: "S2", text: "y", section: "running", title: "Another")]
        XCTAssertEqual(WorkSemantics.doing(strand(drafts: drafts), runs: [], runsLoaded: true),
                       "Running: Targeted rebalance of the pool and 1 more")
    }

    func testWhenTheGreyTextDoesNotFitLineTwoKeepsTheRightThing() {
        // His action stays.
        XCTAssertEqual(WorkRowText(detail: "Two drafted", line: "For you: sign", tone: .you)
                        .fallbackLine, "For you: sign")
        // Nothing his and a run moving: the run.
        XCTAssertEqual(WorkRowText(detail: "Running: Handoff", line: "Next: List",
                                   marker: "Running").fallbackLine, "Running: Handoff")
        // Nothing his, nothing moving: Next, else the headline.
        XCTAssertEqual(WorkRowText(detail: "177 lbs, a new low", line: "Next: Tape it").fallbackLine,
                       "Next: Tape it")
        XCTAssertEqual(WorkRowText(detail: "The wine in the garage").fallbackLine,
                       "The wine in the garage")
    }

    func testAMarkerOnlyWhileTheGreyTextIsTheRun() {
        // The run's line repeats Next, so the grey text goes, and so does the marker.
        let drafts = [StrandDraft(id: "R", text: "x", section: "running", title: "Upstream list"),
                      StrandDraft(id: "Q", text: "y", section: "queue", title: "Upstream list")]
        let t = text(strand(drafts: drafts))
        XCTAssertNil(t?.detail)
        XCTAssertNil(t?.marker)
    }

    func testARunningRunNamesItselfAndCountsTheQueuedApart() {
        let runs = [run("GO256", .running, title: "Ablation A2"),
                    run("GO284", .queued, title: "HTTP/2 by default")]
        let t = text(strand(headline: "Merged: arm64 fallback chain",
                            yourAction: "branch protection on release-0.19",
                            waiting: "you: branch protection."), runs: runs)
        XCTAssertEqual(t?.detail, "Running: Ablation A2, 1 queued")
        XCTAssertEqual(t?.marker, "Running")
        XCTAssertEqual(t?.line, "For you: branch protection on release-0.19")
    }

    func testALineTheNoteFilesUnderRunningIsMovingTooUnlessARunHasIt() {
        let drafts = [StrandDraft(id: "S16b", text: "Rebalance.", section: "running",
                                  title: "Targeted rebalance of the pool"),
                      StrandDraft(id: "P55d", text: "Jellyfin.", section: "queue",
                                  title: "Jellyfin fix and finish")]
        let t = text(strand(headline: "kc1 fixed for good", drafts: drafts))
        XCTAssertEqual(t?.detail, "Running: Targeted rebalance of the pool")
        XCTAssertEqual(t?.marker, "Running")
        XCTAssertEqual(t?.line, "Next: Jellyfin fix and finish")
        // A line whose step is a Studio run is that run's, said once.
        let k04 = [StrandDraft(id: "K04", text: "Proxy.", section: "running", title: "Model proxy")]
        let asked = run("K04", .needsYou, reportLine: "NEEDS YOU: Grant it?")
        XCTAssertNil(WorkSemantics.doing(strand(drafts: k04, needsYou: 1), runs: [asked],
                                         runsLoaded: true))
    }

    func testACutRunTitleDoesNotEndOnASmallWord() {
        let t = text(strand(), runs: [run("HO5", .running,
                                          title: "Work board rows that say what each project is doing and\u{2026}")])
        XCTAssertEqual(t?.detail, "Running: Work board rows that say what each project is doing\u{2026}")
    }

    // MARK: D. Nothing his, nothing moving

    func testNextSaysWhatComesNextWithoutAsidesOrAStaleDeadline() {
        let t = text(strand(headline: "Not shortlisted",
                            nextTitle: "Sign off the services rate card (with the team), due end of today"))
        XCTAssertEqual(t?.detail, "Not shortlisted")
        XCTAssertEqual(t?.line, "Next: Sign off the services rate card")
        XCTAssertEqual(t?.tone, .plain)
    }

    func testAHeadlineThatRepeatsNextGivesWayToIt() {
        let t = text(strand(headline: "The library card in town and the gym pass",
                            nextTitle: "The library card at the desk and the gym pass"))
        XCTAssertNil(t?.detail)
        XCTAssertEqual(t?.line, "Next: The library card at the desk and the gym pass")
        // A one word title inside a headline is not a repeat.
        XCTAssertFalse(WorkSemantics.sameLead("Finished the Blog rewrite", "Next: Blog"))
        // The same name after a moved verb is.
        XCTAssertTrue(WorkSemantics.sameLead("Launched: Video handoff", "Next: Video handoff"))
        // So is a headline that holds a three word Next.
        XCTAssertTrue(WorkSemantics.sameLead("Proof two of the book is out", "Next: of the book"))
        XCTAssertFalse(WorkSemantics.sameLead("Anything at all", ""))
    }

    func testARunningLineIsNotNext() {
        let drafts = [StrandDraft(id: "EB10", text: "Handoff.", section: "running", title: "Handoff"),
                      StrandDraft(id: "EB9", text: "Upstream list.", section: "queue",
                                  title: "Upstream list")]
        XCTAssertEqual(text(strand(drafts: drafts))?.line, "Next: Upstream list")
    }

    func testNothingToSayIsNoText() {
        XCTAssertNil(text(strand()))
    }

    // MARK: E. A long Waiting on

    func testOnlyTheFirstGateReachesTheRow() {
        // The bridge cut `for S23, Part 0: …; for D2b, …; for P27, …` to its first gate.
        let t = text(strand(yourAction: "the reader read only user on the router and the laptop password file",
                            waiting: "you: for S23, Part 0: the reader …; for D2b, …; for P27: …"))
        XCTAssertEqual(t?.line,
                       "For you: the reader read only user on the router and the laptop password file")
    }

    // MARK: The line, fitted on a word

    func testAFittedLineNeverEndsMidWordOrOnASmallWord() {
        let v = WordFitLine.variants("You: generate the keypair per the runbook")
        XCTAssertEqual(v.first, "You: generate the keypair per the runbook")
        for cut in v.dropFirst() {
            XCTAssertTrue(cut.hasSuffix("\u{2026}"), cut)
            let last = cut.dropLast().split(separator: " ").last.map(String.init) ?? ""
            XCTAssertFalse(WordFitLine.dangling.contains(last.lowercased()), cut)
        }
        XCTAssertTrue(v.contains("You: generate the keypair\u{2026}"))
    }

    func testACutQuotationStillCloses() {
        let v = WordFitLine.variants("For you: answer \u{201C}Will you grant it now?\u{201D}")
        XCTAssertTrue(v.contains("For you: answer \u{201C}Will you grant\u{2026}\u{201D}"), "\(v)")
        XCTAssertTrue(v.dropFirst().allSatisfy { $0.hasSuffix("\u{2026}\u{201D}") || !$0.contains("\u{201C}") })
        // One word cannot be cut on a word.
        XCTAssertEqual(WordFitLine.variants("Supercalifragilistic"), ["Supercalifragilistic"])
    }

    func testAMinimumKeepsStubsOut() {
        let v = WordFitLine.variants("The daily living notes roll is live", minWords: 4)
        XCTAssertTrue(v.allSatisfy { $0.split(separator: " ").count >= 4 }, "\(v)")
    }
}
