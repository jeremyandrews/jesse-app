import XCTest
@testable import JesseNetworking

/// Archiving on the wire, decoded from JSON the bridge's own serializers wrote
/// (`the_app_wire_fixtures_match_what_the_run_routes_serialize`, `JESSE_RUN_FIXTURE_WRITE=1`),
/// never by hand: `studio-runs-list-archived.json` (`?archived=all` with one run archived by
/// hand) carries `archived_at`, `archived_reason` and the filter counts;
/// `studio-runs-list-0.203.json` is the same test's output at bridge 0.203.0, before any of them.
final class StudioRunsArchiveWireTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)",
                                                  withExtension: "json"))
        return try Data(contentsOf: url)
    }

    func testAListWithAnArchivedRunCarriesItsStampItsReasonAndTheFilterCounts() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list-archived"))
        let byID = Dictionary(uniqueKeysWithValues: list.runs.map { ($0.runID, $0) })
        let done = try XCTUnwrap(byID["s-done"])
        XCTAssertEqual(done.status, .reported)
        XCTAssertNotNil(done.archivedAt)
        XCTAssertEqual(done.archivedReason, "archived by Jeremy")
        XCTAssertTrue(done.isArchived)
        let gd5 = try XCTUnwrap(byID["GD5"])
        XCTAssertNil(gd5.archivedAt)
        XCTAssertNil(gd5.archivedReason)
        XCTAssertFalse(gd5.isArchived)
        XCTAssertEqual(list.counts.needsMe, 1)
        XCTAssertEqual(list.counts.moving, 2)
        XCTAssertEqual(list.counts.finished, 0)
        XCTAssertEqual(list.counts.archived, 1)
        // The tallies the list had before archiving are still there.
        XCTAssertEqual(list.counts.needsYou, 1)
        XCTAssertEqual(list.counts.paused, 1)
    }

    func testTheDefaultListCountsEveryFilterAndHasNoArchivedRun() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list"))
        XCTAssertEqual(list.counts.needsMe, 1)
        XCTAssertEqual(list.counts.moving, 2)
        XCTAssertEqual(list.counts.finished, 1)
        XCTAssertEqual(list.counts.archived, 0)
        XCTAssertTrue(list.runs.allSatisfy { $0.archivedAt == nil && !$0.isArchived })
    }

    func testAListFromABridgeBeforeArchivingStillDecodesWithNoArchiveFields() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list-0.203"))
        XCTAssertEqual(list.runs.count, 4)
        XCTAssertTrue(list.runs.allSatisfy { $0.archivedAt == nil && $0.archivedReason == nil })
        XCTAssertNil(list.counts.needsMe)
        XCTAssertNil(list.counts.moving)
        XCTAssertNil(list.counts.finished)
        XCTAssertNil(list.counts.archived)
        XCTAssertEqual(list.counts.needsYou, 1)
    }

    func testClosedIsAStatusOfItsOwnAndAnArchivedRunAReplyRequeuedIsNotArchived() {
        XCTAssertEqual(StudioRunStatus(wire: "closed"), .closed)
        XCTAssertEqual(StudioRunStatus.closed.wire, "closed")
        XCTAssertTrue(StudioRunStatus.closed.isFinished)
        for s: StudioRunStatus in [.needsYou, .interrupted, .failed] {
            XCTAssertTrue(s.needsMe, "\(s)")
            XCTAssertFalse(s.isFinished, "\(s)")
        }
        let stamp = "2026-11-10T10:00:00.000Z"
        XCTAssertTrue(StudioRun(runID: "A", status: .reported, archivedAt: stamp).isArchived)
        XCTAssertFalse(StudioRun(runID: "A", status: .queued, archivedAt: stamp).isArchived)
        XCTAssertFalse(StudioRun(runID: "A", status: .needsYou, archivedAt: stamp).isArchived)
    }

    func testTheModeIsAQueryOnlyWhenItIsNotTheDefault() {
        XCTAssertEqual(StudioRunArchivedMode.hide.queryItems, [])
        XCTAssertEqual(StudioRunArchivedMode.only.queryItems,
                       [URLQueryItem(name: "archived", value: "only")])
        XCTAssertEqual(StudioRunArchivedMode.all.queryItems,
                       [URLQueryItem(name: "archived", value: "all")])
    }
}

/// A run id reaches the bridge as written: a free run's `s-…` id with its hyphens.
final class StudioRunPathTests: XCTestCase {
    func testARunIdKeepsItsHyphensAndDotsAndEscapesOnlyTheRest() {
        XCTAssertEqual(JesseBridgeClient.runPathEscaped("s-tidewater-woodcuts-e17"),
                       "s-tidewater-woodcuts-e17")
        XCTAssertEqual(JesseBridgeClient.runPathEscaped("HO1b"), "HO1b")
        XCTAssertEqual(JesseBridgeClient.runPathEscaped("a_b.c~d"), "a_b.c~d")
        XCTAssertEqual(JesseBridgeClient.runPathEscaped("../x"), "..%2Fx")
        XCTAssertEqual(JesseBridgeClient.runPathEscaped("a b"), "a%20b")
    }
}
