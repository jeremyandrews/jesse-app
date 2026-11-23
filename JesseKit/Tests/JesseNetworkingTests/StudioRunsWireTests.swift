import XCTest
@testable import JesseNetworking

/// The Studio run routes, decoded from JSON the bridge's own serializers wrote
/// (`the_app_wire_fixtures_match_what_the_run_routes_serialize` in `bridge/src/hostrun/tests.rs`,
/// `JESSE_RUN_FIXTURE_WRITE=1`). Never hand edited: a key spelled wrong here would decode to a
/// default and pass, which is why the fixtures come from the serializer and the bridge test
/// fails when its keys move.
final class StudioRunsWireTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)",
                                                  withExtension: "json"))
        return try Data(contentsOf: url)
    }

    private func json(_ name: String) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: fixture(name)) as? [String: Any])
    }

    // MARK: - GET /jesse/runs

    func testTheListDecodesEveryRunWithItsStatusAndTheCounts() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list"))
        XCTAssertEqual(list.runs.map(\.runID), ["GD12", "GD10", "s-done", "GD5"])
        XCTAssertEqual(list.runs.map(\.status), [.queued, .paused, .reported, .needsYou])
        XCTAssertEqual(list.counts, StudioRunCounts(running: 0, queued: 1, needsYou: 1, paused: 1,
                                                    needsMe: 1, moving: 2, finished: 1,
                                                    archived: 0))
        let queued = list.runs[0]
        XCTAssertEqual(queued.waitingFor, "limit")
        XCTAssertEqual(queued.strand, "Gd")
        XCTAssertEqual(queued.itemID, "GD12")
        let free = list.runs[2]
        XCTAssertNil(free.strand)
        XCTAssertNil(free.itemID)
        XCTAssertEqual(free.reportPaths.count, 1)
    }

    /// One run in every status the bridge has, and one it might grow, each with a report: the
    /// whole list decodes, no run is dropped, and each keeps its report path (an archived
    /// report's included, which is where the bridge now serves a processed one).
    static let everyStatusList: String = {
        let statuses = ["queued", "running", "needs_you", "reported", "failed", "cancelled",
                        "interrupted", "paused", "closed", "rebasing"]
        let runs = statuses.enumerated().map { i, status in
            """
            {"run_id":"R\(i)","strand":\(i % 2 == 0 ? "\"Jesse\"" : "null"),"item_id":null,
             "repo":"o/n","branch":"r\(i)","origin":"turn","status":"\(status)",
             "created_at":"2026-11-09T2\(i % 10):00:00.000Z","rounds":[{"n":1,
             "report_path":"Inbox/\(i % 3 == 0 ? "archive/" : "")2026-11-09-run-R\(i).md"}],
             "host":"studio","report_paths":["Inbox/\(i % 3 == 0 ? "archive/" : "")2026-11-09-run-R\(i).md"]}
            """
        }
        return #"{"runs":["# + runs.joined(separator: ",")
            + #"],"counts":{"running":1,"queued":1,"needs_you":1,"paused":1},"disk":{},"orphans":[]}"#
    }()

    func testAListHoldingEveryStatusDecodesEveryRunWithItsReport() throws {
        let list = try StudioRunList.decode(from: Data(Self.everyStatusList.utf8))
        XCTAssertEqual(list.runs.count, 10)
        XCTAssertEqual(list.runs.map(\.status),
                       [.queued, .running, .needsYou, .reported, .failed, .cancelled,
                        .interrupted, .paused, .closed, .unknown("rebasing")])
        for (i, run) in list.runs.enumerated() {
            XCTAssertEqual(run.runID, "R\(i)")
            XCTAssertEqual(run.latestReportPath,
                           "Inbox/\(i % 3 == 0 ? "archive/" : "")2026-11-09-run-R\(i).md")
        }
    }

    func testAPausedRunCarriesResumeAtAndARoundWithItsOutcome() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-paused"))
        XCTAssertEqual(run.status, .paused)
        XCTAssertNotNil(run.resumeAt)
        XCTAssertEqual(run.rounds.last?.outcome, .paused)
        XCTAssertTrue(run.reportPaths.isEmpty, "a pause writes no report")
        XCTAssertNotNil(run.resumeAt.flatMap { ISO8601DateFormatter.fractional.date(from: $0) })
    }

    // MARK: - GET /jesse/runs/:run_id

    func testANeedsYouRunDecodesEveryFieldTheDetailShows() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-needs-you"))
        XCTAssertEqual(run.runID, "GD5")
        XCTAssertEqual(run.status, .needsYou)
        XCTAssertEqual(run.strand, "Gd")
        XCTAssertEqual(run.itemID, "GD5")
        XCTAssertEqual(run.itemText, "Run the thing on the Studio.")
        XCTAssertEqual(run.promptPath, "Projects/drafts/gd5-prompt.md")
        XCTAssertEqual(run.repo, "o/n")
        XCTAssertEqual(run.branch, "gd5-run-the-thing-on-the-studio")
        XCTAssertEqual(run.origin, "app")
        XCTAssertEqual(run.host, "studio")
        XCTAssertEqual(run.round, 1)
        XCTAssertFalse(run.createdAt.isEmpty)
        XCTAssertNotNil(run.startedAt)
        XCTAssertNotNil(run.finishedAt)
        XCTAssertNotNil(run.lastEventAt)
        XCTAssertNil(run.resumeAt)
        XCTAssertEqual(run.reportPaths, [try XCTUnwrap(run.rounds.first?.reportPath)])
        XCTAssertTrue(run.latestReportPath?.hasPrefix("Inbox/") ?? false)
        XCTAssertEqual(run.rounds.first?.outcome, .needsYou)
        XCTAssertNil(run.rounds.first?.text, "round 1 runs the prompt, not a reply")
    }

    // MARK: - The other three answers

    func testTheStartAnswerDecodes() throws {
        let started = try StudioRunStarted.decode(from: fixture("studio-run-started"))
        XCTAssertEqual(started, StudioRunStarted(runID: "GD12", status: .queued, waitingFor: "limit"))
    }

    func testTheLogDecodesItsReadableLines() throws {
        let log = try StudioRunLog.decode(from: fixture("studio-run-log"))
        XCTAssertEqual(log.runID, "GD5")
        XCTAssertEqual(log.lines.first, "Reading the README.")
        XCTAssertFalse(log.lines.isEmpty)
    }

    // MARK: - Tolerance

    /// A status a newer bridge serves decodes to `.unknown` carrying its word, never a failed
    /// decode of the whole list.
    func testAnUnknownStatusDecodesToUnknownAndTheListStillDecodes() throws {
        var root = try json("studio-runs-list")
        var runs = try XCTUnwrap(root["runs"] as? [[String: Any]])
        runs[0]["status"] = "rebasing"
        var rounds = try XCTUnwrap(runs[0]["rounds"] as? [[String: Any]])
        rounds[0]["outcome"] = "rebasing"
        runs[0]["rounds"] = rounds
        root["runs"] = runs
        let data = try JSONSerialization.data(withJSONObject: root)
        let list = try StudioRunList.decode(from: data)
        XCTAssertEqual(list.runs.count, 4)
        XCTAssertEqual(list.runs[0].status, .unknown("rebasing"))
        XCTAssertEqual(list.runs[0].status.wire, "rebasing")
        XCTAssertFalse(list.runs[0].status.isLive)
        XCTAssertEqual(list.runs[0].rounds.first?.outcome, .unknown("rebasing"))
        XCTAssertEqual(list.runs[1].status, .paused)
    }

    func testEveryKnownStatusRoundTripsItsWireWord() {
        for word in ["queued", "running", "needs_you", "reported", "failed", "cancelled",
                     "interrupted", "paused", "closed", "reviewing"] {
            let status = StudioRunStatus(wire: word)
            XCTAssertNotEqual(status, .unknown(word), word)
            XCTAssertEqual(status.wire, word)
        }
    }

    // MARK: - The strands snapshot

    func testTheAnnotatedSnapshotCarriesRunsActiveNeedsYouAndRunnableDrafts() throws {
        let snap = try StrandsSnapshot.decode(from: fixture("strands-annotated"))
        let gd = try XCTUnwrap(snap.strand(slug: "Gd"))
        XCTAssertEqual(gd.runsActive, 2)
        XCTAssertEqual(gd.runsNeedsYou, 1)
        let drafts = try XCTUnwrap(gd.drafts)
        XCTAssertEqual(drafts.filter(\.studioRunnable).map(\.id), ["GD5", "GD10", "GD12"])
        let checked = try XCTUnwrap(drafts.first { $0.id == "GD6" })
        XCTAssertTrue(checked.checked)
        XCTAssertFalse(checked.isOpen)
        XCTAssertEqual(drafts.first?.section, "queue")
    }

    /// A bridge that predates Studio runs serves neither key: both are nil, not zero and not
    /// empty, and nothing else about the strand moves.
    func testASnapshotWithoutTheRunFieldsStillDecodes() throws {
        let old = try StrandsSnapshot.decode(from: fixture("strands-before-runs"))
        let new = try StrandsSnapshot.decode(from: fixture("strands-annotated"))
        let gd = try XCTUnwrap(old.strand(slug: "Gd"))
        XCTAssertNil(gd.runsActive)
        XCTAssertNil(gd.runsNeedsYou)
        XCTAssertNil(gd.drafts)
        var stripped = try XCTUnwrap(new.strand(slug: "Gd"))
        stripped.runsActive = nil
        stripped.runsNeedsYou = nil
        stripped.drafts = nil
        // The board row's fields (0.205.0) are annotations too.
        stripped.headline = nil
        stripped.yourAction = nil
        stripped.servesYourAction = false
        stripped.nextTitle = nil
        XCTAssertEqual(gd, stripped)
    }

    // MARK: - Review (bridge 0.211.0)

    /// `reviewing` is a status of its own, decoded and encoded as its wire word.
    func testReviewingDecodesAndEncodesItsWireWord() throws {
        XCTAssertEqual(StudioRunStatus(wire: "reviewing"), .reviewing)
        XCTAssertEqual(StudioRunStatus.reviewing.wire, "reviewing")
        let decoded = try JSONDecoder().decode([StudioRunStatus].self,
                                               from: Data(#"["reviewing"]"#.utf8))
        XCTAssertEqual(decoded, [.reviewing])
        let encoded = try JSONEncoder().encode([StudioRunStatus.reviewing])
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), #"["reviewing"]"#)
    }

    /// Every predicate, every status, against `RunStatus` in `bridge/src/hostrun.rs`:
    /// `is_moving`, `is_live`, `is_active`, `needs_me`, `is_archivable` (`is_terminal`) and
    /// what `cancel` accepts. `isFinished` is the app's Finished filter (the bridge's
    /// `finished` count: neither moving nor needing its owner).
    func testEveryStatusPredicateIsTheBridges() {
        typealias Row = (moving: Bool, live: Bool, active: Bool, finished: Bool, needsMe: Bool,
                         archivable: Bool, cancellable: Bool)
        let bridge: [(StudioRunStatus, Row)] = [
            (.queued, (true, true, false, false, false, false, true)),
            (.running, (true, true, true, false, false, false, true)),
            (.needsYou, (false, true, true, false, true, false, false)),
            (.reported, (false, false, false, true, false, true, false)),
            (.failed, (false, false, false, false, true, true, false)),
            (.cancelled, (false, false, false, true, false, true, false)),
            (.interrupted, (false, false, false, false, true, false, false)),
            (.paused, (true, true, true, false, false, false, true)),
            (.closed, (false, false, false, true, false, true, false)),
            (.reviewing, (true, true, true, false, false, false, true)),
        ]
        for (status, want) in bridge {
            XCTAssertEqual(status.isMoving, want.moving, "moving \(status)")
            XCTAssertEqual(status.isLive, want.live, "live \(status)")
            XCTAssertEqual(status.isActive, want.active, "active \(status)")
            XCTAssertEqual(status.isFinished, want.finished, "finished \(status)")
            XCTAssertEqual(status.needsMe, want.needsMe, "needs me \(status)")
            XCTAssertEqual(status.isArchivable, want.archivable, "archivable \(status)")
            XCTAssertEqual(status.isCancellable, want.cancellable, "cancellable \(status)")
        }
    }

    /// A run in review, as bridge 0.211.0 serves it (`served_one` while the implementing
    /// round's review runs, captured from `a_reviewed_run_implements_is_reviewed_hands_off_
    /// and_finishes_once`): the status, who reviews it, the focus and the one running review.
    func testAReviewingRunDecodesItsReviewerAndItsReview() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-reviewing-served"))
        XCTAssertEqual(run.status, .reviewing)
        XCTAssertEqual(run.reviewer, "codex-write")
        XCTAssertEqual(run.reviewerLabel, "Codex")
        XCTAssertEqual(run.reviewFocus, "the loop bounds")
        XCTAssertTrue(run.isReviewed)
        XCTAssertEqual(run.reviewerName, "Codex")
        XCTAssertFalse(run.isArchived)
        let review = try XCTUnwrap(run.reviews.first)
        XCTAssertEqual(run.reviews.count, 1)
        XCTAssertEqual(review.n, 1)
        XCTAssertEqual(review.reviewer, "codex-write")
        XCTAssertEqual(review.reviewerLabel, "Codex")
        XCTAssertEqual(review.origin, "start")
        XCTAssertEqual(review.status, .running)
        XCTAssertNotNil(review.requestedAt)
        XCTAssertNil(review.finishedAt)
        XCTAssertNil(review.path, "no note before the reviewer answers")
        XCTAssertNil(review.head)
    }

    /// The same run once its answering round reported: the review answered, its note where it
    /// is now (`Inbox/archive/`), the head and base it read.
    func testAnAnsweredReviewServesItsNoteWhereItIsNow() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-reviewed-served"))
        XCTAssertEqual(run.status, .reported)
        let review = try XCTUnwrap(run.reviews.first)
        XCTAssertEqual(review.status, .answered)
        XCTAssertTrue(review.path?.hasPrefix("Inbox/archive/") ?? false, "\(review.path ?? "nil")")
        XCTAssertTrue(review.path?.hasSuffix("-review-1.md") ?? false)
        XCTAssertNotNil(review.finishedAt)
        XCTAssertNotNil(review.head)
        XCTAssertNotNil(review.base)
    }

    /// A run two models reviewed in one cycle, as bridge 0.213.0 serves it once its one
    /// answering round reported (`two_reviewers_review_at_once_and_are_answered_in_one_round_
    /// and_one_push`): `reviewer` is the first one, `reviewer_label` joins both labels, and
    /// `reviews` has an entry per reviewer, both answered.
    func testARunWithTwoReviewsDecodesBoth() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-two-reviews-served"))
        XCTAssertEqual(run.status, .reported)
        XCTAssertEqual(run.reviewer, "codex-write")
        XCTAssertEqual(run.reviewerLabel, "Codex + GLM 5")
        XCTAssertTrue(run.isReviewed)
        XCTAssertEqual(run.reviews.count, 2)
        XCTAssertEqual(run.reviews.map(\.n), [1, 2])
        XCTAssertEqual(run.reviews.map(\.reviewer), ["codex-write", "glm"])
        XCTAssertEqual(run.reviews.map(\.reviewerLabel), ["Codex", "GLM 5"])
        XCTAssertEqual(run.reviews.map(\.status), [.answered, .answered])
        for review in run.reviews {
            XCTAssertTrue(review.path?.hasPrefix("Inbox/archive/") ?? false,
                          "\(review.path ?? "nil")")
            XCTAssertNotNil(review.head)
        }
        XCTAssertTrue(run.reviews[1].path?.hasSuffix("-review-2.md") ?? false)
    }

    /// A run never reviewed, and every run from a bridge before 0.211.0, sends none of the
    /// review keys: it decodes with none of them.
    func testARunWithoutReviewKeysDecodesWithNoReview() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-needs-you-served"))
        XCTAssertNil(run.reviewer)
        XCTAssertNil(run.reviewerLabel)
        XCTAssertNil(run.reviewFocus)
        XCTAssertTrue(run.reviews.isEmpty)
        XCTAssertFalse(run.isReviewed)
        XCTAssertNil(run.reviewerName)
    }

    /// A review status a newer bridge serves, and a review missing every key but `n`, decode
    /// without failing the run.
    func testAnUnknownReviewStatusDecodesWithoutFailing() throws {
        var root = try json("studio-run-reviewing-served")
        var reviews = try XCTUnwrap(root["reviews"] as? [[String: Any]])
        reviews[0]["status"] = "re_reading"
        reviews.append(["n": 2])
        root["reviews"] = reviews
        let run = try StudioRun.decode(from: JSONSerialization.data(withJSONObject: root))
        XCTAssertEqual(run.reviews.map(\.status), [.unknown("re_reading"), .unknown("")])
        XCTAssertEqual(run.reviews[0].status.wire, "re_reading")
        XCTAssertEqual(run.reviews[1].reviewer, "")
        XCTAssertEqual(run.reviewerName, "Codex", "an empty newest review falls back on the run")
        for word in ["running", "ready", "handed_off", "answered", "failed", "dropped"] {
            XCTAssertNotEqual(StudioRunReviewStatus(wire: word), .unknown(word), word)
            XCTAssertEqual(StudioRunReviewStatus(wire: word).wire, word)
        }
    }

    // MARK: - Refusals

    func testARefusalIsTheBridgesSentenceVerbatim() {
        let message = "GD5 is reported; only a queued, running or paused run can be cancelled"
        XCTAssertEqual(StudioRunError.refused(status: 409, message: message).errorDescription,
                       message)
        XCTAssertEqual(StudioRunError.refused(status: 400, message: "  \n").errorDescription,
                       "The bridge answered 400 with no reason.")
    }

    // MARK: - Cloud runs (bridge 0.216.0)

    /// The list with the cloud's runs beside the Studio's: `host` on every run, the cloud's
    /// own tallies, and the flag saying they were listed.
    func testACloudListDecodesHostOnEveryRunAndTheCloudsOwnCounts() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list-cloud"))
        XCTAssertEqual(list.runs.map(\.runID),
                       ["GD12", "GD10", "GD5", "DJ6", "BG4-2", "s-dj2-cloud-live-proof"])
        XCTAssertEqual(list.runs.map(\.host), ["studio", "studio", "studio", "cloud", "cloud",
                                               "cloud"])
        XCTAssertEqual(list.runs.map(\.isCloud), [false, false, false, true, true, true])
        XCTAssertEqual(list.cloudAvailable, true)
        XCTAssertNil(list.cloudUnavailable)
        XCTAssertEqual(list.counts.cloud, StudioRunCloudCounts(running: 2, queued: 0, needsMe: 0,
                                                               moving: 2, finished: 1))
        // The Studio's own tallies keep their meaning.
        XCTAssertEqual(list.counts.needsMe, 1)
        XCTAssertEqual(list.counts.moving, 2)
        let bg = list.runs[4]
        XCTAssertEqual(bg.status, .running)
        XCTAssertEqual(bg.repo, "jeremyandrews/trovato")
        XCTAssertEqual(bg.branch, "jesse/bg4-2-one-install-hosts-many-blogs-by-host-nam")
        XCTAssertEqual(bg.createdAt, "2026-11-23T17:08:27.000Z")
        XCTAssertEqual(bg.startedAt, "2026-11-23T17:08:28.000Z")
        XCTAssertEqual(bg.agent, "claude")
        XCTAssertEqual(bg.jobName, "devjob-bg4-2")
        XCTAssertTrue(bg.rounds.isEmpty)
        let proof = list.runs[5]
        XCTAssertEqual(proof.status, .reported)
        XCTAssertEqual(proof.outcome, "pull request opened")
        XCTAssertEqual(proof.latestReportPath, "Inbox/2026-11-23-run-s-dj2-cloud-live-proof.md")
        // Two hosts, one id space: a cloud run's identity is its own.
        XCTAssertEqual(list.runs[3].id, "cloud:DJ6")
        XCTAssertEqual(list.runs[0].id, "GD12")
    }

    func testAListWhoseCloudIsOutOfReachSaysWhyAndKeepsTheStudiosRuns() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list-cloud-unavailable"))
        XCTAssertEqual(list.cloudAvailable, false)
        XCTAssertEqual(list.cloudUnavailable, "kc1 did not answer within 3 s")
        XCTAssertEqual(list.runs.map(\.runID), ["GD12", "GD10", "GD5"])
        XCTAssertFalse(list.runs.contains(where: \.isCloud))
        XCTAssertNil(list.counts.cloud)
    }

    /// A bridge before cloud rows: no flag, no cloud counts, and every run a Studio run.
    func testAListFromABridgeWithoutCloudRowsDecodesAsBefore() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list"))
        XCTAssertNil(list.cloudAvailable)
        XCTAssertNil(list.cloudUnavailable)
        XCTAssertNil(list.counts.cloud)
        XCTAssertFalse(list.runs.contains(where: \.isCloud))
        XCTAssertEqual(list.runs.map(\.id), list.runs.map(\.runID))
    }

    /// A run with no `host` at all (a bridge before 0.188.0 served none) is a Studio run.
    func testARunWithoutHostIsAStudioRun() throws {
        let run = try StudioRun.decode(from: Data(#"{"run_id":"GD5","status":"reported"}"#.utf8))
        XCTAssertNil(run.host)
        XCTAssertFalse(run.isCloud)
        XCTAssertEqual(run.id, "GD5")
        let cloud = try StudioRun.decode(from: Data(
            #"{"run_id":"GD5","host":"cloud","status":"running"}"#.utf8))
        XCTAssertTrue(cloud.isCloud)
        XCTAssertEqual(cloud.id, "cloud:GD5")
    }

    func testACloudRunsDetailDecodesItsStatusAndLastLine() throws {
        let run = try StudioRun.decode(from: fixture("cloud-run-detail"))
        XCTAssertEqual(run.runID, "DJ6")
        XCTAssertTrue(run.isCloud)
        XCTAssertEqual(run.status, .running)
        XCTAssertEqual(run.repo, "jeremyandrews/jesse-app")
        XCTAssertEqual(run.lastLine, "Running cargo test")
        XCTAssertNil(run.outcome)
        XCTAssertTrue(run.reportPaths.isEmpty)
    }

    func testACloudRunIsAskedForWithHostCloudAndAStudioRunAsBefore() {
        XCTAssertEqual(JesseBridgeClient.hostQuery(cloud: true),
                       [URLQueryItem(name: "host", value: "cloud")])
        XCTAssertEqual(JesseBridgeClient.hostQuery(cloud: false), [])
    }
}

private extension ISO8601DateFormatter {
    static var fractional: ISO8601DateFormatter {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }
}
