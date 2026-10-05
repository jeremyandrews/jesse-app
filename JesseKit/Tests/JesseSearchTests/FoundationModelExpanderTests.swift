import XCTest
import JesseVault
@testable import JesseSearch

/// A stub model session that behaves like a real one in the way that matters here: it
/// is a conversation, so everything it has been asked stays in its transcript.
@MainActor
final class StubExpansionSession: ExpansionSession {
    private(set) var transcript: [String] = []
    private(set) var prewarmed = false
    var groups: [ExpansionConcept] = [ExpansionConcept(word: "bridge", alternatives: ["span", "overpass"])]
    var delay: Duration = .zero

    func prewarm() { prewarmed = true }

    func respond(to prompt: String) async throws -> [ExpansionConcept] {
        transcript.append(prompt)
        if delay > .zero { try await Task.sleep(for: delay) }
        return groups
    }
}

@MainActor
final class FoundationModelExpanderTests: XCTestCase {

    private var made: [StubExpansionSession] = []

    private func expander(available: Bool = true, delay: Duration = .zero,
                          timeout: Duration = .seconds(2)) -> FoundationModelExpander {
        FoundationModelExpander(
            makeSession: { [unowned self] in
                let s = StubExpansionSession()
                s.delay = delay
                self.made.append(s)
                return s
            },
            availability: { available ? .available : .unavailable(reason: "off") },
            timeout: timeout)
    }

    /// THE EVER GROWING SESSION. One reused session carried every earlier prompt and
    /// answer into each call, slower each time until the context window filled and
    /// every call failed. Each call now starts from a clean transcript.
    func testTheSecondCallDoesNotCarryTheFirstCallsTranscript() async {
        let e = expander()
        _ = await e.expand("bridge")
        _ = await e.expand("garden")

        XCTAssertEqual(made.count, 2, "each call gets its own session")
        XCTAssertEqual(made[1].transcript.count, 1,
                       "the second call's session holds only its own prompt")
        XCTAssertFalse(made[1].transcript[0].contains("bridge"))
    }

    /// Prewarm still pays off: the warmed session serves the first call, then is dropped.
    func testThePrewarmedSessionServesTheFirstCallOnly() async {
        let e = expander()
        e.prewarm()
        XCTAssertEqual(made.count, 1)
        XCTAssertTrue(made[0].prewarmed)
        _ = await e.expand("bridge")
        XCTAssertEqual(made.count, 1, "the first call used the warmed session")
        _ = await e.expand("garden")
        XCTAssertEqual(made.count, 2)
        XCTAssertEqual(made[1].transcript.count, 1)
    }

    /// A call that outlives the timeout yields nothing, promptly.
    func testATimeoutYieldsNothing() async {
        let e = expander(delay: .seconds(5), timeout: .milliseconds(50))
        let clock = ContinuousClock()
        let started = clock.now
        let terms = await e.expand("bridge")
        XCTAssertEqual(terms, [])
        XCTAssertLessThan(clock.now - started, .seconds(1))
    }

    /// An unavailable model is never given a session, and says why.
    func testAnUnavailableModelMakesNoSession() async {
        let e = expander(available: false)
        e.prewarm()
        let terms = await e.expand("bridge")
        XCTAssertEqual(terms, [])
        XCTAssertTrue(made.isEmpty)
        XCTAssertEqual(e.availability, .unavailable(reason: "off"))
    }

    /// Groups come back filtered: trimmed, deduplicated, never the word itself.
    func testGroupsAreFiltered() async {
        let e = expander()
        e.prewarm()
        made[0].groups = [ExpansionConcept(word: "bridge",
                                           alternatives: ["Bridge", " span ", "span", "overpass"])]
        let concepts = await e.expand("bridge")
        XCTAssertEqual(concepts, [ExpansionConcept(word: "bridge", alternatives: ["span", "overpass"])])
    }

    /// THE OBSERVED EXPANSION, App 1.0 (187). The model was asked for whole alternative
    /// queries and paraphrased around `keys`: `missing keys`, `keys not found`, `search
    /// for keys`, `found keys`. Each restates a query word, so nothing survives and the
    /// expander reports no expansion rather than four ways to narrow inside `keys`.
    func testTheObservedRestatementsYieldNoExpansion() async {
        let e = expander()
        e.prewarm()
        let observed = ["missing keys", "keys not found", "search for keys", "found keys"]
        made[0].groups = [ExpansionConcept(word: "lost", alternatives: observed),
                          ExpansionConcept(word: "keys", alternatives: observed)]
        let concepts = await e.expand("lost keys")
        XCTAssertEqual(concepts, [], "no group may only restate `keys`")
    }

    /// The model is asked per word: the prompt lists the query's concept words, never a
    /// stop word, and the instructions carry the worked example.
    func testThePromptAsksPerWord() async {
        let e = expander()
        e.prewarm()
        made[0].groups = []
        _ = await e.expand("search for keys")
        XCTAssertEqual(made[0].transcript.count, 1)
        XCTAssertTrue(made[0].transcript[0].contains("Words: search, keys."))
        XCTAssertTrue(FoundationModelExpander.instructions.contains("keys: key, keychain, fob, car key"))
        XCTAssertTrue(FoundationModelExpander.instructions.contains("never contain the word"))
    }

    /// A query of nothing but stop words has no concept to expand: no session is spent.
    func testAQueryOfOnlyStopWordsAsksNothing() async {
        let e = expander()
        let concepts = await e.expand("what is it")
        XCTAssertEqual(concepts, [])
        XCTAssertTrue(made.isEmpty)
    }

    /// The vault search keeps its string contract: it is handed whole queries with one
    /// word replaced at a time, best first, at most four.
    func testTheVaultIsHandedSubstitutedQueries() async {
        let e = expander()
        e.prewarm()
        made[0].groups = [ExpansionConcept(word: "lost", alternatives: ["misplaced", "missing"]),
                          ExpansionConcept(word: "keys", alternatives: ["keychain", "fob"])]
        let queries = await VaultModelExpansion(e).expand("lost keys")
        XCTAssertEqual(queries, ["misplaced keys", "lost keychain", "missing keys", "lost fob"])
    }
}
