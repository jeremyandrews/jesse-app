import XCTest
@testable import JesseVault

// The deterministic half of query expansion: which words of a query are concepts, what
// survives of the model's groups, and the whole queries the vault search is handed.
final class ExpansionConceptsTests: XCTestCase {

    private func c(_ word: String, _ alternatives: [String]) -> ExpansionConcept {
        ExpansionConcept(word: word, alternatives: alternatives)
    }

    // MARK: - Concept words

    /// FUNCTION WORDS WERE REQUIRED NEEDLES. Every token of two or more characters was
    /// mandatory, so `for` in `search for keys` had to appear in a thread. A concept word
    /// is never a stop word.
    func testConceptWordsDropStopWordsAndPunctuation() {
        XCTAssertEqual(SearchQueryRules.conceptWords("search for keys"), ["search", "keys"])
        XCTAssertEqual(SearchQueryRules.conceptWords("  Lost keys? "), ["Lost", "keys"])
        XCTAssertEqual(SearchQueryRules.conceptWords("flight to Amsterdam"), ["flight", "Amsterdam"])
        XCTAssertEqual(SearchQueryRules.conceptWords("what is it"), [])
        XCTAssertEqual(SearchQueryRules.conceptWords("keys Keys"), ["keys"])
    }

    // MARK: - The filter

    /// THE OBSERVED EXPANSION, App 1.0 (187): every term repeated `keys`. Whichever word
    /// the model files them under, none survives: each restates a query word.
    func testTheObservedRestatementsOfKeysAllGo() {
        let observed = ["missing keys", "keys not found", "search for keys", "found keys"]
        for raw in [[c("keys", observed)], [c("lost", observed)],
                    [c("lost", observed), c("keys", observed)]] {
            let out = SearchQueryRules.filterConcepts(raw, query: "lost keys")
            XCTAssertEqual(out, [c("lost", []), c("keys", [])])
            XCTAssertFalse(SearchQueryRules.hasAlternatives(out))
        }
    }

    /// The worked example survives whole, in query order, whatever order the groups came in.
    func testTheWorkedExampleSurvives() {
        let raw = [c("keys", ["key", "keychain", "fob", "car key"]),
                   c("lost", ["misplaced", "missing", "can't find", "forgot"])]
        XCTAssertEqual(SearchQueryRules.filterConcepts(raw, query: "lost keys"),
                       [c("lost", ["misplaced", "missing", "can't find", "forgot"]),
                        c("keys", ["key", "keychain", "fob", "car key"])])
    }

    /// An alternative CONTAINING its word adds nothing under substring matching; one the
    /// word contains is broader and stays.
    func testContainingTheWordGoesContainedByTheWordStays() {
        let out = SearchQueryRules.filterConcepts([c("keys", ["car keys", "KEYS", "key", "fob"])],
                                                  query: "keys")
        XCTAssertEqual(out, [c("keys", ["key", "fob"])])
    }

    /// Another query word culls an alternative only as a whole word.
    func testAnotherQueryWordCullsOnlyAsAWholeWord() {
        let out = SearchQueryRules.filterConcepts([c("mail", ["ai letter", "letter", "post"])],
                                                  query: "ai mail")
        XCTAssertEqual(out, [c("ai", []), c("mail", ["letter", "post"])])
        let kept = SearchQueryRules.filterConcepts([c("message", ["email", "note"])],
                                                   query: "ai message")
        XCTAssertEqual(kept, [c("ai", []), c("message", ["email", "note"])])
    }

    /// Case and diacritics fold before the comparisons.
    func testFoldsCaseAndDiacritics() {
        let out = SearchQueryRules.filterConcepts([c("cafe", ["Café Nero", "coffee shop", "Coffee Shop"])],
                                                  query: "café")
        XCTAssertEqual(out, [c("café", ["coffee shop"])])
    }

    /// A single stop word goes; a phrase that starts with one stays.
    func testAStopWordAlternativeIsDropped() {
        let out = SearchQueryRules.filterConcepts([c("lost", ["the", "for", "can't find", "  ", "x"])],
                                                  query: "lost")
        XCTAssertEqual(out, [c("lost", ["can't find"])])
    }

    func testTrimsCollapsesAndDedupes() {
        let out = SearchQueryRules.filterConcepts([c("lost", ["  misplaced ", "Misplaced", "can't   find"])],
                                                  query: "lost")
        XCTAssertEqual(out, [c("lost", ["misplaced", "can't find"])])
    }

    /// A group for a word the query does not have is ignored; a word nobody offered
    /// anything for is its own concept.
    func testGroupsAreAlignedToTheQuery() {
        let out = SearchQueryRules.filterConcepts([c("wallet", ["purse"]), c("keys", ["fob"])],
                                                  query: "lost keys")
        XCTAssertEqual(out, [c("lost", []), c("keys", ["fob"])])
    }

    /// Four per word, twelve overall, round robin so a later word keeps its best ones.
    func testCapsPerConceptAndOverall() {
        let six = (1...6).map { "alt\($0)" }
        let perWord = SearchQueryRules.filterConcepts([c("one", six)], query: "one")
        XCTAssertEqual(perWord.first?.alternatives.count, 4)

        let words = ["aa", "bb", "cc", "dd"]
        let raw = words.map { w in c(w, (1...4).map { "\(w)x\($0)".replacingOccurrences(of: w, with: "z") }) }
        let out = SearchQueryRules.filterConcepts(raw, query: words.joined(separator: " "))
        XCTAssertEqual(out.map(\.alternatives.count), [3, 3, 3, 3])
        XCTAssertEqual(SearchQueryRules.alternatives(out).count, 12)
    }

    // MARK: - Presentation

    func testCaptionGroupsByWord() {
        let concepts = [c("lost", ["misplaced", "missing"]), c("keys", ["key", "keychain"])]
        XCTAssertEqual(SearchQueryRules.caption(concepts), "misplaced, missing · key, keychain")
        XCTAssertEqual(SearchQueryRules.caption([c("lost", []), c("keys", ["fob"])]), "fob")
        XCTAssertEqual(SearchQueryRules.logDescription(concepts),
                       "lost: misplaced, missing; keys: key, keychain")
    }

    // MARK: - The vault's alternate queries

    /// One concept substituted at a time, best first, four at most.
    func testSubstitutionQueriesAreBestFirstAndCapped() {
        let concepts = [c("lost", ["misplaced", "missing", "can't find"]),
                        c("keys", ["key", "keychain", "fob"])]
        XCTAssertEqual(SearchQueryRules.substitutionQueries("lost keys", concepts: concepts),
                       ["misplaced keys", "lost key", "missing keys", "lost keychain"])
    }

    /// Stop words the person typed stay where they were.
    func testSubstitutionKeepsTheTypedWords() {
        let concepts = [c("flight", ["plane"]), c("Amsterdam", [])]
        XCTAssertEqual(SearchQueryRules.substitutionQueries("flight to Amsterdam", concepts: concepts),
                       ["plane to Amsterdam"])
        XCTAssertEqual(SearchQueryRules.substitutionQueries("lost", concepts: []), [])
    }
}
