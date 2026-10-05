import XCTest
import SwiftData
import JesseCore
import JesseConversations
import JesseVault

// The conversation search index: semantics, ranking, incremental upkeep, and the
// keystroke budget on a realistic 500 thread store.
@MainActor
final class ThreadSearchIndexTests: XCTestCase {

    private func doc(_ title: String, _ texts: [String] = [], at seconds: TimeInterval = 0,
                     id: UUID = UUID()) -> ThreadSearchDocument {
        ThreadSearchDocument(id: id, updatedAt: Date(timeIntervalSince1970: seconds),
                             title: title, texts: texts)
    }

    private func ids(_ r: ThreadSearchResult) -> [UUID] { r.hits.map(\.id) }

    // MARK: - Ranking

    /// Title hits, then body hits, then expansion only hits, each newest first.
    func testRankingIsTitleThenBodyThenExpansionEachNewestFirst() {
        let oldTitle = doc("bridge restart", at: 1)
        let newTitle = doc("the bridge again", at: 9)
        let oldBody = doc("ops", ["restarted the bridge"], at: 2)
        let newBody = doc("ops", ["bridge is up"], at: 8)
        let oldExp = doc("launchd plist", at: 3)
        let newExp = doc("launchd agent", at: 7)
        let neither = doc("garden", ["tomatoes"], at: 10)

        let r = searchThreads([oldTitle, oldBody, oldExp, neither, newExp, newBody, newTitle],
                              query: "bridge", concepts: [ExpansionConcept(word: "bridge", alternatives: ["launchd"])])

        XCTAssertEqual(ids(r), [newTitle.id, oldTitle.id, newBody.id, oldBody.id,
                                newExp.id, oldExp.id])
        XCTAssertEqual(r.hits.map(\.kind), [.title, .title, .body, .body, .expansion, .expansion])
        XCTAssertEqual(r.terms, ["launchd"])
    }

    /// A thread the typed query finds is never demoted to an expansion hit.
    func testADirectHitIsNeverCountedAsExpansion() {
        let both = doc("bridge", ["launchd"])
        let r = searchThreads([both], query: "bridge", concepts: [ExpansionConcept(word: "bridge", alternatives: ["launchd"])])
        XCTAssertEqual(r.hits.map(\.kind), [.title])
    }

    // MARK: - Expansion concepts

    private let lostKeys = [ExpansionConcept(word: "lost", alternatives: ["misplaced"]),
                            ExpansionConcept(word: "keys", alternatives: ["keychain"])]

    /// THE THREAD EXPANSION EXISTS FOR. Each model term used to be matched as an AND of
    /// its own tokens, and every term repeated `keys`, so a thread that says only
    /// `misplaced my keychain` was unreachable. As concepts, AND across words and OR
    /// within one, it is an expansion hit, and the snippet comes from where it matched.
    func testAThreadWithOnlyAlternativesIsAnExpansionHit() {
        let target = doc("errands", ["I misplaced my keychain at the gym"])
        let r = searchThreads([target], query: "lost keys", concepts: lostKeys)
        XCTAssertEqual(r.hits.map(\.kind), [.expansion])
        XCTAssertEqual(r.hits.first?.snippetSource, "I misplaced my keychain at the gym")
        XCTAssertEqual(r.concepts, lostKeys)
        XCTAssertEqual(r.terms, ["misplaced", "keychain"])
    }

    /// One word typed, the other replaced: still an expansion hit.
    func testOneWordDirectAndOneReplacedIsAnExpansionHit() {
        let target = doc("errands", ["lost the keychain again"])
        XCTAssertEqual(searchThreads([target], query: "lost keys", concepts: lostKeys)
            .hits.map(\.kind), [.expansion])
    }

    /// Every concept must be met: an alternative for one word is not enough alone.
    func testEveryConceptMustBeMet() {
        let half = doc("errands", ["misplaced the remote"])
        XCTAssertTrue(searchThreads([half], query: "lost keys", concepts: lostKeys).hits.isEmpty)
    }

    /// FUNCTION WORDS AS NEEDLES. `search for keys` as a model term required `search`,
    /// `for` and `keys`, so a thread with those words was an "expansion" hit for `lost
    /// keys` that never said lost or anything like it. Concepts never match through a
    /// stop word, nor through a thread that meets every concept only with the typed words.
    func testAThreadMeetingNoAlternativeIsNotAnExpansionHit() {
        let noise = doc("ops", ["time to search for keys in the config"])
        XCTAssertTrue(searchThreads([noise], query: "lost keys", concepts: lostKeys).hits.isEmpty)

        let stopWordShort = doc("ops", ["search keys"])
        let concepts = [ExpansionConcept(word: "search", alternatives: ["look"]),
                        ExpansionConcept(word: "keys", alternatives: ["keychain"])]
        XCTAssertTrue(searchThreads([stopWordShort], query: "search for keys", concepts: concepts)
            .hits.isEmpty, "every word typed, only `for` missing: not an expansion")
    }

    /// An alternative is a phrase, and a typed straight apostrophe also matches a reply's
    /// curly one.
    func testAlternativesMatchAsPhrasesEitherApostrophe() {
        let concepts = [ExpansionConcept(word: "lost", alternatives: ["can't find"]),
                        ExpansionConcept(word: "keys", alternatives: ["fob"])]
        let curly = doc("car", ["I can\u{2019}t find the fob"])
        let split = doc("car", ["can't you find the fob"])
        let r = searchThreads([curly, split], query: "lost keys", concepts: concepts)
        XCTAssertEqual(ids(r), [curly.id])
    }

    /// A direct hit's snippet looks for the typed words only; an expansion hit's for the
    /// alternatives too.
    func testSnippetQueriesArePerHit() {
        let direct = doc("lost keys again")
        let expanded = doc("errands", ["misplaced my keychain"])
        let r = searchThreads([direct, expanded], query: "lost keys", concepts: lostKeys)
        XCTAssertEqual(r.queries(for: r.hits[0]), ["lost keys"])
        XCTAssertEqual(r.queries(for: r.hits[1]), ["lost keys", "misplaced", "keychain"])
    }

    /// Concepts with no alternatives widen nothing.
    func testConceptsWithoutAlternativesWidenNothing() {
        let d = doc("errands", ["lost keys"])
        let other = doc("x", ["keychain"])
        let concepts = [ExpansionConcept(word: "lost", alternatives: []),
                        ExpansionConcept(word: "keys", alternatives: [])]
        XCTAssertEqual(ids(searchThreads([d, other], query: "lost keys", concepts: concepts)), [d.id])
    }

    // MARK: - Short queries

    /// One character: a word start in the title only.
    func testOneCharacterMatchesTitleWordStartsOnly() {
        let titleStart = doc("Jesse release")
        let titleMid = doc("project notes")            // "j" inside "project"
        let bodyStart = doc("notes", ["jesse is up"])  // body only
        let r = searchThreads([titleStart, titleMid, bodyStart], query: "j", concepts: [])
        XCTAssertEqual(ids(r), [titleStart.id])
    }

    /// Two characters: a word start in the title or a body.
    func testTwoCharactersMatchWordStartsInTitlesAndBodies() {
        let titleStart = doc("Jesse release", at: 2)
        let bodyStart = doc("notes", ["about “jesse” today"], at: 1)  // curly quote boundary
        let mid = doc("notes", ["object"])                              // "je" mid-word
        let r = searchThreads([titleStart, bodyStart, mid], query: "je", concepts: [])
        XCTAssertEqual(ids(r), [titleStart.id, bodyStart.id])
        XCTAssertEqual(r.hits.map(\.kind), [.title, .body])
    }

    /// Three or more: substring anywhere, as before.
    func testThreeCharactersMatchAnywhere() {
        let mid = doc("notes", ["an object here"])
        XCTAssertEqual(searchThreads([mid], query: "jec", concepts: []).hits.count, 1)
    }

    func testFoldingMatchesCaseAndDiacritics() {
        let d = doc("Trip", ["We stopped at a café in Málaga."])
        XCTAssertEqual(searchThreads([d], query: "CAFE", concepts: []).hits.count, 1)
        XCTAssertEqual(searchThreads([d], query: "malaga", concepts: []).hits.count, 1)
    }

    func testABlankQueryIsInactive() {
        XCTAssertEqual(searchThreads([doc("x")], query: "   ", concepts: [ExpansionConcept(word: "x", alternatives: ["y"])]), .inactive)
    }

    // MARK: - Snippet source

    /// The hit carries the one text its snippet is cut from: the title for a title hit,
    /// else the first body text holding a match.
    func testTheHitCarriesItsSnippetSource() {
        let d = doc("ops", ["nothing here", "the bridge restarted", "bridge again"])
        let hit = searchThreads([d], query: "bridge", concepts: []).hits[0]
        XCTAssertEqual(hit.snippetSource, "the bridge restarted")
        let snippet = searchSnippet(for: hit, queries: ["bridge"])
        XCTAssertEqual(snippet.map { s in s.ranges.map { String(s.text[$0]) } }, ["bridge"])
    }

    // MARK: - Same matches as before for three or more characters

    /// Every thread the old matcher (`threadMatches`) finds for a query of three or more
    /// characters, the index finds too, on the realistic fixture.
    func testEveryThreadTheOldMatcherFoundIsStillFound() async throws {
        let store = try SearchFixture.make(threads: 120)
        defer { store.remove() }
        let threads = try store.threads()
        let index = ThreadSearchIndex(container: store.container)
        let stamps = threads.map(ThreadSearchStamp.init)
        for q in ["jes", "bridge", "run bridge", "cafe", "MÁLAGA", "weigh-in", "a b",
                  "kamado pizza", "sqlite cache", "xyzzy"] {
            let old = Set(threads.filter { threadMatches($0, query: q) }.map(\.id))
            let new = Set(await index.search(stamps, query: q, concepts: []).hits.map(\.id))
            XCTAssertTrue(old.isSubset(of: new), "'\(q)': lost \(old.subtracting(new).count)")
            XCTAssertEqual(old, new, "'\(q)': same set as before")
        }
    }

    // MARK: - Incremental upkeep

    /// An unchanged store rebuilds nothing; a changed thread rebuilds only itself; a
    /// deleted thread leaves the index.
    func testOnlyChangedThreadsAreRebuilt() async throws {
        let store = try SearchFixture.make(threads: 30)
        defer { store.remove() }
        var threads = try store.threads()
        let index = ThreadSearchIndex(container: store.container)

        _ = await index.search(threads.map(ThreadSearchStamp.init), query: "bridge", concepts: [])
        let first = await index.buildCount
        XCTAssertEqual(first, 30)
        _ = await index.search(threads.map(ThreadSearchStamp.init), query: "garden", concepts: [])
        let second = await index.buildCount
        XCTAssertEqual(second, 30, "nothing changed, nothing rebuilt")

        // A new turn on one thread, saved, as the app does it: append and bump updatedAt.
        let changed = threads[3]
        changed.turns.append(Turn(role: .jesse, text: "zanzibar quokka", createdAt: .now))
        changed.updatedAt = .now
        try store.context.save()
        let hits = await index.search(threads.map(ThreadSearchStamp.init),
                                      query: "quokka", concepts: []).hits
        XCTAssertEqual(hits.map(\.id), [changed.id])
        let third = await index.buildCount
        XCTAssertEqual(third, 31, "only the changed thread was rebuilt")

        let gone = threads.removeFirst()
        let rest = await index.search(threads.map(ThreadSearchStamp.init), query: "the", concepts: [])
        XCTAssertFalse(rest.hits.contains { $0.id == gone.id })
    }

    // MARK: - The budget

    /// THE KEYSTROKE BUDGET, on 500 realistic threads. Before this index the list ran
    /// every match on the main actor per keystroke: 140 to 170 ms for an ordinary query
    /// on this fixture. Now:
    ///   * main thread work per keystroke (the update call, stamping the threads, the
    ///     search layout and the visible rows' snippets) stays under 16 ms,
    ///   * a settled result, after the debounce, lands within 100 ms for `j`, `je` and
    ///     `bridge`, and for `run bridge` and `lost keys` with a full set of expansion
    ///     concepts applied (the concept pass runs on the index actor, like the rest).
    ///     With its concepts `run bridge` is 36 expansion hits and 1 direct one, so its
    ///     visible rows are the snippet's worst case: each looks for every alternative.
    func testKeystrokeBudgetOnFiveHundredThreads() async throws {
        let store = try SearchFixture.make()
        defer { store.remove() }
        let threads = try store.threads()
        let index = ThreadSearchIndex(container: store.container)

        func ms(_ start: UInt64) -> Double {
            Double(DispatchTime.now().uptimeNanoseconds - start) / 1e6
        }

        // The first pass builds every document, off the main actor.
        var t0 = DispatchTime.now().uptimeNanoseconds
        _ = await index.search(threads.map(ThreadSearchStamp.init), query: "x", concepts: [])
        print(String(format: "SEARCH index cold build: %.1f ms (off main)", ms(t0)))

        let expanded: [String: [ExpansionConcept]] = [
            "lost keys": [ExpansionConcept(word: "lost", alternatives: ["misplaced", "missing", "can't find", "forgot"]),
                          ExpansionConcept(word: "keys", alternatives: ["key", "keychain", "fob", "car key"])],
            "run bridge": [ExpansionConcept(word: "run", alternatives: ["start", "launch", "execute"]),
                           ExpansionConcept(word: "bridge", alternatives: ["agent", "server", "daemon"])],
        ]
        for q in ["j", "je", "jes", "bridge", "run bridge", "lost keys"] {
            // Main: stamping the threads for the pass.
            t0 = DispatchTime.now().uptimeNanoseconds
            let stamps = threads.map(ThreadSearchStamp.init)
            let stamping = ms(t0)

            // Off main: the pass.
            t0 = DispatchTime.now().uptimeNanoseconds
            let result = await index.search(stamps, query: q, concepts: expanded[q] ?? [])
            let settled = ms(t0)

            // Main: applying it, the ranked layout and the visible rows' snippets.
            t0 = DispatchTime.now().uptimeNanoseconds
            let rows = threadSearchLayout(threads, result: result, favoritesOnly: false)
            for row in rows.prefix(12) {
                _ = searchSnippet(for: row.hit, queries: result.queries(for: row.hit))
            }
            let apply = ms(t0)

            let main = stamping + apply
            print(String(format: "SEARCH %-12@ hits=%3d main=%6.2f ms (stamp %5.2f, layout+snippets %5.2f) settled=%6.2f ms",
                         "'\(q)'", result.hits.count, main, stamping, apply, settled))
            XCTAssertLessThan(main, 16, "'\(q)': main thread work per keystroke")
            if ["j", "je", "bridge", "run bridge", "lost keys"].contains(q) {
                XCTAssertLessThan(settled, 100, "'\(q)': settled result after the debounce")
            }
        }
    }
}
