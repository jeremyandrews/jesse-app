import XCTest
import SwiftData
import JesseCore
import JesseConversations
@testable import JesseSearch

// The search pass and the expansion tier together, over a real (in-memory) store: what
// the list actually draws.
@MainActor
final class ConversationSearchTests: XCTestCase {

    private func store(_ titles: [String]) throws -> (ModelContainer, [JesseThread]) {
        let container = try ModelContainer(
            for: jesseCurrentSchema,
            configurations: ModelConfiguration(schema: jesseCurrentSchema,
                                               isStoredInMemoryOnly: true))
        let context = container.mainContext
        var threads: [JesseThread] = []
        for (i, title) in titles.enumerated() {
            let t = JesseThread(title: title, mode: .ask,
                                createdAt: Date(timeIntervalSince1970: 1_000_000 + Double(i)))
            context.insert(t)
            threads.append(t)
        }
        try context.save()
        return (container, threads)
    }

    /// THE GATE THAT SILENCED EXPANSION. A three character query with twenty direct hits
    /// used to stay unexpanded (five or more hits was "plenty"), which is why the "Also
    /// searching" caption almost never appeared. It now expands, and the alternate
    /// term's thread joins the result below every direct hit.
    func testAThreeCharacterQueryWithTwentyDirectHitsExpands() async throws {
        let (container, threads) = try store(
            (1...20).map { "dog walk \($0)" } + ["canine companion"])
        let fake = FakeQueryExpander()
        fake.termsByQuery = ["dog": ["canine"]]
        let m = ThreadSearchModel(expander: fake, index: ThreadSearchIndex(container: container),
                                  debounce: .zero, searchDebounce: .zero)

        m.update(query: "dog", threads: threads)
        await m.settle()
        await m.settle()

        XCTAssertEqual(fake.calledQueries, ["dog"], "twenty direct hits no longer suppress the model")
        XCTAssertEqual(m.result.terms, ["canine"])
        XCTAssertEqual(m.result.hits.count, 21)
        XCTAssertEqual(m.result.hits.last?.kind, .expansion,
                       "the expansion only hit ranks below every direct hit")
        XCTAssertFalse(m.isExpanding)
    }

    /// The list keeps the previous answer until the new pass lands, and a blank field
    /// is inactive at once.
    func testResultLandsAfterThePassAndClearsAtOnce() async throws {
        let (container, threads) = try store(["roof repair", "garden plan"])
        let m = ThreadSearchModel(expander: NoExpansion(),
                                  index: ThreadSearchIndex(container: container),
                                  searchDebounce: .zero)
        m.update(query: "roof", threads: threads)
        XCTAssertFalse(m.result.isActive, "nothing has landed synchronously")
        await m.settle()
        XCTAssertEqual(m.result.hits.map(\.id), [threads[0].id])

        m.update(query: "")
        XCTAssertEqual(m.result, .inactive)
    }

    /// An unavailable model is never asked, and the pass still runs.
    func testAnUnavailableModelIsNeverAsked() async throws {
        let (container, threads) = try store(["roof repair"])
        let fake = FakeQueryExpander()
        fake.availability = .unavailable(reason: "off")
        let m = ThreadSearchModel(expander: fake, index: ThreadSearchIndex(container: container),
                                  debounce: .zero, searchDebounce: .zero)
        m.update(query: "roof", threads: threads)
        await m.settle()
        XCTAssertEqual(fake.callCount, 0)
        XCTAssertFalse(m.isExpanding)
        XCTAssertEqual(m.result.hits.count, 1)
    }
}
