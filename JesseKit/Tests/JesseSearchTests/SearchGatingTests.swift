import XCTest
@testable import JesseSearch

// The Tier-2 expansion GATING decision (`shouldExpand`): when the on-device query
// expansion tier is worth invoking. Pure and deterministic, so it is asserted
// directly with no model and no view host. Shared by iOS and macOS via JesseSearch.
final class SearchGatingTests: XCTestCase {

    func testShouldExpandGating() {
        // Trivial (short) query: never expand.
        XCTAssertFalse(shouldExpand(query: "hi"))
        XCTAssertFalse(shouldExpand(query: "  a "))
        // A real query: expand, however many direct hits it has.
        XCTAssertTrue(shouldExpand(query: "dog"))
        XCTAssertTrue(shouldExpand(query: "bridge"))
        // Off in Settings, or no model to ask: never.
        XCTAssertFalse(shouldExpand(query: "bridge", enabled: false))
        XCTAssertFalse(shouldExpand(query: "bridge", available: false))
    }
}
