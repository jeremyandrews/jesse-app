import XCTest
@testable import JesseSearch

/// A stub model session that behaves like a real one in the way that matters here: it
/// is a conversation, so everything it has been asked stays in its transcript.
@MainActor
final class StubExpansionSession: ExpansionSession {
    private(set) var transcript: [String] = []
    private(set) var prewarmed = false
    var terms: [String] = ["alpha", "beta"]
    var delay: Duration = .zero

    func prewarm() { prewarmed = true }

    func respond(to prompt: String) async throws -> [String] {
        transcript.append(prompt)
        if delay > .zero { try await Task.sleep(for: delay) }
        return terms
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

    /// Terms come back filtered: trimmed, deduplicated, never the query itself.
    func testTermsAreFiltered() async {
        let e = expander()
        e.prewarm()
        made[0].terms = ["Bridge", " span ", "span", "overpass"]
        let terms = await e.expand("bridge")
        XCTAssertEqual(terms, ["span", "overpass"])
    }
}
