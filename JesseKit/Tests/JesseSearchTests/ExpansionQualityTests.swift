import XCTest
import JesseVault
@testable import JesseSearch

/// THE REAL MODEL, where there is one. Every other expander test stubs the session; this
/// one runs the production expander over a fixed list of queries and prints the groups
/// it produced, so a change to the instructions can be judged on actual output.
///
/// It asserts nothing about the words the model chooses (that is a judgment, and the
/// model changes with the OS), only the contract the filter guarantees. On a host where
/// the on-device model is unavailable (CI, the simulator, a machine without Apple
/// Intelligence) it skips and says why; it never fails for want of a model.
@MainActor
final class ExpansionQualityTests: XCTestCase {

    static let queries = ["lost keys", "bridge deploy", "vet appointment",
                          "flight to Amsterdam", "wine harvest", "run bridge"]

    func testRealModelGroupsForTheFixedQueries() async throws {
        let availability = FoundationModelExpander.systemAvailability()
        guard availability.isAvailable else {
            if case .unavailable(let reason) = availability {
                throw XCTSkip("On-device model unavailable on this host: \(reason)")
            }
            return
        }
        let expander = FoundationModelExpander()
        for query in Self.queries {
            let concepts = await expander.expand(query)
            print("EXPANSION \(query) -> \(concepts.isEmpty ? "(none)" : SearchQueryRules.logDescription(concepts))")
            let words = SearchQueryRules.conceptWords(query).map(SearchQueryRules.fold)
            for concept in concepts {
                XCTAssertLessThanOrEqual(concept.alternatives.count, 4)
                for alt in concept.alternatives {
                    let folded = SearchQueryRules.fold(alt)
                    XCTAssertFalse(words.contains { folded.contains($0) },
                                   "'\(alt)' restates a word of '\(query)'")
                }
            }
        }
    }
}
