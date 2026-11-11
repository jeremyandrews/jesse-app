import XCTest
@testable import JesseNetworking

/// The Work view's fields, decoded from JSON the bridge's own serializers wrote
/// (`the_app_wire_fixtures_match_what_the_run_routes_serialize`, `JESSE_RUN_FIXTURE_WRITE=1`),
/// never by hand: one set carrying every new field, and one from the serializers that carry
/// none of them, which is what an older bridge sends.
final class WorkWireTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)",
                                                  withExtension: "json"))
        return try Data(contentsOf: url)
    }

    // MARK: - With every new field

    func testTheListCarriesATitleALastLineAndAReportLinePerRun() throws {
        let list = try StudioRunList.decode(from: fixture("studio-runs-list"))
        let byID = Dictionary(uniqueKeysWithValues: list.runs.map { ($0.runID, $0) })
        let gd5 = try XCTUnwrap(byID["GD5"])
        XCTAssertEqual(gd5.title, "Run the thing on the Studio")
        XCTAssertEqual(gd5.lastLine, "Edit: bridge/src/x.rs")
        XCTAssertEqual(gd5.reportLine, "NEEDS YOU: main or release?")
        // A free run is named after its branch, never its id.
        let free = try XCTUnwrap(byID["s-done"])
        XCTAssertEqual(free.title, "S done")
        XCTAssertEqual(free.reportLine, "Merged.")
        // Queued and never run: no line yet, and null decodes to nil.
        let queued = try XCTUnwrap(byID["GD12"])
        XCTAssertNotNil(queued.title)
        XCTAssertNil(queued.lastLine)
        XCTAssertNil(queued.reportLine)
        for run in list.runs {
            XCTAssertNotEqual(run.title, run.runID, "a title is never the run id")
        }
    }

    func testOneServedRunCarriesTheSameFields() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-needs-you-served"))
        XCTAssertEqual(run.status, .needsYou)
        XCTAssertEqual(run.title, "Run the thing on the Studio")
        XCTAssertEqual(run.reportLine, "NEEDS YOU: main or release?")
        XCTAssertNotNil(run.lastLine)
    }

    func testDraftsCarryATitleAndWhetherTheyCanStart() throws {
        let snapshot = try StrandsSnapshot.decode(from: fixture("strands-annotated"))
        let drafts = try XCTUnwrap(snapshot.strands.first?.drafts)
        let byID = Dictionary(uniqueKeysWithValues: drafts.map { ($0.id, $0) })
        let gd5 = try XCTUnwrap(byID["GD5"])
        XCTAssertEqual(gd5.title, "Run the thing on the Studio")
        XCTAssertEqual(gd5.displayTitle, "Run the thing on the Studio")
        // Runnable, but it has a live run: `studio_runnable` is unchanged, `startable` says no.
        XCTAssertTrue(gd5.studioRunnable)
        XCTAssertEqual(gd5.startable, false)
        XCTAssertEqual(gd5.startableReason, "Already has a live run.")
        let gd10 = try XCTUnwrap(byID["GD10"])
        XCTAssertEqual(gd10.startable, false)
        XCTAssertTrue(gd10.startableReason?.contains("devjob-host") ?? false)
        for draft in drafts {
            XCTAssertNotNil(draft.startable, draft.id)
            XCTAssertEqual(draft.startable == true, draft.startableReason == nil, draft.id)
        }
    }

    func testTheLastLookDecodesSetAndUnset() throws {
        let set = try WorkLastLook.decode(from: fixture("work-last-look"))
        XCTAssertNotNil(set.lastLook)
        XCTAssertNotNil(ISO8601DateFormatter.withFractions.date(from: set.lastLook ?? ""))
        let none = try WorkLastLook.decode(from: fixture("work-last-look-none"))
        XCTAssertNil(none.lastLook)
    }

    // MARK: - Without any of them (an older bridge)

    func testARunWithNoneOfTheNewFieldsStillDecodes() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-needs-you"))
        XCTAssertEqual(run.runID, "GD5")
        XCTAssertEqual(run.status, .needsYou)
        XCTAssertNil(run.title)
        XCTAssertNil(run.lastLine)
        XCTAssertNil(run.reportLine)
    }

    func testASnapshotWithNoneOfTheNewFieldsStillDecodes() throws {
        let snapshot = try StrandsSnapshot.decode(from: fixture("strands-before-runs"))
        XCTAssertFalse(snapshot.strands.isEmpty)
        XCTAssertNil(snapshot.strands.first?.drafts)
        // A draft from a bridge with `studio_runnable` but no Work fields.
        let old = try JSONDecoder().decode(StrandDraft.self, from: Data(
            #"{"id":"A1","text":"Do it.","section":"queue","checked":false,"studio_runnable":true}"#.utf8))
        XCTAssertNil(old.title)
        XCTAssertNil(old.startable)
        XCTAssertNil(old.startableReason)
        XCTAssertEqual(old.displayTitle, "Do it.")
    }
}

extension ISO8601DateFormatter {
    fileprivate static var withFractions: ISO8601DateFormatter {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }
}
