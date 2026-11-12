import XCTest
@testable import JesseNetworking

/// The board row's fields (bridge 0.205.0), decoded from JSON the bridge's own serializers wrote
/// (`the_app_wire_fixtures_match_what_the_run_routes_serialize`, `JESSE_RUN_FIXTURE_WRITE=1`),
/// never by hand. `strands-annotated-0.204.json` is that test's output at bridge 0.204.0, before
/// `headline`, `your_action` and `next_title`, kept as written then: an older bridge still decodes.
final class WorkRowWireTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)",
                                                  withExtension: "json"))
        return try Data(contentsOf: url)
    }

    func testAStrandCarriesItsHeadlineItsActionAndItsNextTitle() throws {
        let snapshot = try StrandsSnapshot.decode(from: fixture("strands-annotated"))
        let gd = try XCTUnwrap(snapshot.strands.first)
        // `GD6 merged (PR 12): the thing runs.` reworded around GD6's title.
        XCTAssertEqual(gd.headline, "Merged: Checked one")
        // `you: pick `main` or a release branch for GD5 (asked 9 Nov); then the rest.`
        XCTAssertEqual(gd.yourAction, "pick main or a release branch")
        XCTAssertTrue(gd.servesYourAction)
        XCTAssertEqual(gd.nextTitle, "Run the thing on the Studio")
    }

    func testAnOlderBridgeDecodesWithNoneOfThem() throws {
        for name in ["strands-annotated-0.204", "strands-before-runs"] {
            let snapshot = try StrandsSnapshot.decode(from: fixture(name))
            let gd = try XCTUnwrap(snapshot.strands.first, name)
            XCTAssertNil(gd.headline, name)
            XCTAssertNil(gd.yourAction, name)
            XCTAssertNil(gd.nextTitle, name)
            XCTAssertFalse(gd.servesYourAction, "\(name): no key, so the app reads the gate")
        }
    }

    func testANullActionIsServedAndNotAMissingOne() throws {
        let json = #"{"strands":[{"slug":"A","your_action":null,"headline":null,"next_title":null}]}"#
        let a = try XCTUnwrap(StrandsSnapshot.decode(from: Data(json.utf8)).strands.first)
        XCTAssertNil(a.yourAction)
        XCTAssertTrue(a.servesYourAction)
    }
}
