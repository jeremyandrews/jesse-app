import XCTest
import SwiftData
import JesseCore
import JesseConversations
import JesseSearch
import JesseVault
@testable import Jesse_Mac

// Mac sidebar SEARCH wiring, not pixels: with a fake `QueryExpanding` injected and the
// search index attached to an in-memory store, typing a query lands the ranked direct
// matches, and once the on-device expansion terms arrive the layout WIDENS to include a
// thread surfaced only by an expansion term, ranked below the direct hit. The debounce,
// gate and cache behavior itself is covered once in JesseSearchTests; here we assert
// only that the Mac model feeds the shared search and layout.
@MainActor
final class MacThreadSearchTests: XCTestCase {

    /// A scripted fake so the test never depends on a real on-device model (which is
    /// unavailable in CI). `@MainActor` to satisfy the main-actor-isolated seam.
    final class FakeExpander: QueryExpanding {
        var termsByQuery: [String: [String]] = [:]
        private(set) var callCount = 0
        /// The scripted terms, as the one concept of a one word query.
        func expand(_ query: String) async -> [ExpansionConcept] {
            callCount += 1
            let terms = termsByQuery[query.lowercased()] ?? []
            return terms.isEmpty ? [] : [ExpansionConcept(word: query, alternatives: terms)]
        }
    }

    private var container: ModelContainer!

    override func setUp() async throws {
        container = try ModelContainer(
            for: jesseCurrentSchema,
            configurations: ModelConfiguration(schema: jesseCurrentSchema,
                                               isStoredInMemoryOnly: true))
    }

    private let calendar: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        c.locale = Locale(identifier: "en_US_POSIX")
        return c
    }()
    private var now: Date { calendar.date(from: DateComponents(year: 2026, month: 6, day: 25, hour: 12))! }

    private func thread(_ title: String) -> JesseThread {
        let t = JesseThread(mode: .ask)
        t.title = title
        t.updatedAt = now   // same day, so all land in a loose (always-expanded) section
        container.mainContext.insert(t)
        return t
    }

    private func makeModel(_ fake: FakeExpander, enabled: Bool) throws -> MacThreadListModel {
        try container.mainContext.save()
        let model = MacThreadListModel(searchExpander: fake, searchEnabled: enabled,
                                       searchDebounce: .zero, passDebounce: .zero)
        model.search.attach(container)
        return model
    }

    private func memberIDs(_ layout: ThreadListLayout) -> [UUID] {
        switch layout {
        case .flat(let t): return t.map(\.id)
        case .sectioned(let s): return s.flatMap { $0.threads.map(\.id) }
        }
    }

    // The direct match lands first, then the expansion term WIDENS the list.
    func testTypingNarrowsThenExpansionWidens() async throws {
        let fake = FakeExpander()
        fake.termsByQuery = ["dog": ["canine"]]   // "canine" reaches the second thread

        let dog = thread("dog walk plan")
        let canine = thread("canine companion notes")   // matches only the expansion term
        let grocery = thread("grocery list")             // matches neither
        let all = [dog, canine, grocery]

        var model = try makeModel(fake, enabled: true)
        model.searchText = "dog"
        model.updateSearch(all, enabled: true)
        await model.search.settle()

        XCTAssertEqual(fake.callCount, 1)
        XCTAssertEqual(model.search.result.terms, ["canine"])
        XCTAssertEqual(memberIDs(model.layout(all, now: now, calendar: calendar)),
                       [dog.id, canine.id],
                       "the expansion term surfaces the related thread, below the direct hit; the unrelated one stays out")
    }

    // With the tier disabled (Settings toggle off), the expander is never called and
    // the list stays at the direct match set.
    func testDisabledTierStaysTierOne() async throws {
        let fake = FakeExpander()
        fake.termsByQuery = ["dog": ["canine"]]
        let dog = thread("dog walk plan")
        let canine = thread("canine companion notes")
        let all = [dog, canine]

        var model = try makeModel(fake, enabled: false)
        model.searchText = "dog"
        model.updateSearch(all, enabled: false)
        await model.search.settle()

        XCTAssertEqual(fake.callCount, 0, "a disabled tier never calls the expander")
        XCTAssertEqual(model.search.result.terms, [])
        XCTAssertEqual(memberIDs(model.layout(all, now: now, calendar: calendar)), [dog.id],
                       "disabled tier -> the typed query alone, no widening")
    }

    // Search composes with scope: within Favorites, an expansion match that is not a
    // favorite must NOT appear (scope is applied to the ranked hits).
    func testSearchComposesWithFavoritesScope() async throws {
        let fake = FakeExpander()
        fake.termsByQuery = ["dog": ["canine"]]
        let dog = thread("dog walk plan"); dog.setFavorite(true, now: now)
        let canine = thread("canine companion notes")   // matches expansion but NOT a favorite
        let all = [dog, canine]

        var model = try makeModel(fake, enabled: true)
        model.scope = .favorites
        model.searchText = "dog"
        model.updateSearch(all, enabled: true)
        await model.search.settle()

        XCTAssertEqual(model.search.result.terms, ["canine"])
        XCTAssertEqual(memberIDs(model.layout(all, now: now, calendar: calendar)), [dog.id],
                       "the non-favorite expansion match is excluded by the Favorites scope")
    }
}
