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
        XCTAssertEqual(list.counts, StudioRunCounts(running: 0, queued: 1, needsYou: 1, paused: 1))
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
                        "interrupted", "paused", "closed"]
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
        XCTAssertEqual(list.runs.count, 9)
        XCTAssertEqual(list.runs.map(\.status),
                       [.queued, .running, .needsYou, .reported, .failed, .cancelled,
                        .interrupted, .paused, .unknown("closed")])
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
                     "interrupted", "paused"] {
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
        XCTAssertEqual(gd, stripped)
    }

    // MARK: - Refusals

    func testARefusalIsTheBridgesSentenceVerbatim() {
        let message = "GD5 is reported; only a queued, running or paused run can be cancelled"
        XCTAssertEqual(StudioRunError.refused(status: 409, message: message).errorDescription,
                       message)
        XCTAssertEqual(StudioRunError.refused(status: 400, message: "  \n").errorDescription,
                       "The bridge answered 400 with no reason.")
    }
}

private extension ISO8601DateFormatter {
    static var fractional: ISO8601DateFormatter {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }
}
