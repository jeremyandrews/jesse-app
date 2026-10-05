import Foundation
import FoundationModels
import os

// Tier 2's on-device query expander: the ONLY file in JesseSearch that imports
// FoundationModels. Every other type (and every test) depends on the
// `QueryExpanding` seam instead, so the model dependency is fully contained here
// and the rest of the app never links against a specific model API.
//
// Everything degrades to `[]` when the on-device model is unavailable for ANY reason
// (device ineligible, Apple Intelligence off, model not yet downloaded) or when a call
// errors or times out. Nothing is ever sent off the device; `SystemLanguageModel` runs
// entirely on-device. `availability` says which of those it is, for Settings.
//
// ONE CLEAN SESSION PER CALL. This expander used to keep one `LanguageModelSession` for
// every query, and a session is a conversation: each call carried every earlier prompt
// and answer in its transcript, so each expansion was slower than the last, and once the
// context window filled every call threw and was swallowed to `[]` with a log line. That
// is how expansion went silent. Now every call gets a fresh session holding only the
// instructions; the one `prewarm` builds serves the first call and is then dropped.
//
// FoundationModels ships on iOS 26 and macOS 26, so this same expander backs both
// the iPhone and the Mac search. Availability here is about whether the *model* is
// usable at runtime, not whether the framework is present.

/// On-device query-expansion diagnostics: one line per expansion (query, terms,
/// latency, outcome) and availability. Package-local so the expander doesn't depend on
/// the app target's logging.
private let searchLog = Logger(subsystem: "com.tag1.jesse", category: "search")

/// Guided-generation output: a small, count-bounded list of alternate search
/// terms. `@Generable` + `@Guide` constrain the model to return exactly this shape.
@Generable
private struct ExpansionTerms {
    @Guide(description: "2 to 4 alternative search terms for the same thing, synonyms, rephrasings, or more/less specific variants",
           .count(2...4))
    var terms: [String]
}

/// One expansion conversation with the model: the seam a test stubs to see what each
/// call's session was asked. Main-actor bound like the expander that owns it.
@MainActor
public protocol ExpansionSession: AnyObject, Sendable {
    func prewarm()
    func respond(to prompt: String) async throws -> [String]
}

/// The real session: a `LanguageModelSession` holding only the instructions.
private final class LanguageModelExpansionSession: ExpansionSession {
    private let session = LanguageModelSession(instructions: FoundationModelExpander.instructions)

    nonisolated deinit {}

    func prewarm() { session.prewarm() }

    func respond(to prompt: String) async throws -> [String] {
        try await session.respond(to: prompt, generating: ExpansionTerms.self).content.terms
    }
}

@MainActor
public final class FoundationModelExpander: QueryExpanding {
    public typealias SessionFactory = @MainActor () -> any ExpansionSession

    static let instructions = """
    You expand a search query into a few alternative search terms for the same \
    thing, synonyms, rephrasings, or more/less specific variants. Reply with the \
    terms only, no explanations.
    """

    private let makeSession: SessionFactory
    private let currentAvailability: @MainActor () -> QueryExpansionAvailability
    private let timeout: Duration
    /// The session `prewarm` built, waiting for the first call. Used once.
    private var warmed: (any ExpansionSession)?

    /// The production expander: the system model, a two second budget per call.
    public convenience init() {
        self.init(makeSession: { LanguageModelExpansionSession() },
                  availability: { FoundationModelExpander.systemAvailability() },
                  timeout: .seconds(2))
    }

    /// Injectable session factory, availability and timeout, for tests.
    public init(makeSession: @escaping SessionFactory,
                availability: @escaping @MainActor () -> QueryExpansionAvailability,
                timeout: Duration) {
        self.makeSession = makeSession
        self.currentAvailability = availability
        self.timeout = timeout
    }

    /// `nonisolated` for the same reason as `ThreadSearchModel`'s: under this
    /// module's `.defaultIsolation(MainActor.self)` the synthesized deinit would be
    /// MainActor-isolated, and releasing the expander off the main actor (a unit-test
    /// host tears objects down off-actor) routes through the isolated-deinit executor
    /// hop, which aborts. An empty nonisolated deinit avoids the hop; the session
    /// still releases normally afterward. Any class in this target that could be
    /// released off the main actor needs this.
    nonisolated deinit {}

    public var availability: QueryExpansionAvailability { currentAvailability() }

    /// The system model's availability, in words for Settings.
    public static func systemAvailability() -> QueryExpansionAvailability {
        switch SystemLanguageModel.default.availability {
        case .available:
            return .available
        case .unavailable(let reason):
            switch reason {
            case .deviceNotEligible:
                return .unavailable(reason: "This device does not support Apple Intelligence.")
            case .appleIntelligenceNotEnabled:
                return .unavailable(reason: "Apple Intelligence is turned off in Settings.")
            case .modelNotReady:
                return .unavailable(reason: "The on-device model is still downloading.")
            @unknown default:
                return .unavailable(reason: "The on-device model is unavailable.")
            }
        }
    }

    /// Warm a session when the search field gains focus, so the first real query
    /// doesn't pay cold-start latency. Silent no-op when unavailable or already warm.
    public func prewarm() {
        guard availability.isAvailable, warmed == nil else { return }
        let session = makeSession()
        session.prewarm()
        warmed = session
    }

    /// Alternate search terms for `query`, or `[]` when the model is unavailable, the
    /// call fails, or it takes longer than the timeout. Never throws: the search tier
    /// treats `[]` as "no expansion".
    public func expand(_ query: String) async -> [String] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        guard availability.isAvailable else {
            searchLog.info("expansion query=\(trimmed, privacy: .public) outcome=unavailable")
            return []
        }

        // A clean transcript for every call: the prewarmed session once, else a new one.
        let session = warmed ?? makeSession()
        warmed = nil
        let prompt = """
        Give 2 to 4 alternative search terms for this query, for finding the \
        same thing in a list of past conversations. Terms only. Query: "\(trimmed)"
        """
        let clock = ContinuousClock()
        let started = clock.now
        let outcome = await respond(session, prompt)
        let ms = Int((clock.now - started) / .milliseconds(1))

        switch outcome {
        case .terms(let raw):
            let terms = filterExpansionTerms(raw, original: trimmed)
            searchLog.info("expansion query=\(trimmed, privacy: .public) terms=\(terms.joined(separator: ", "), privacy: .public) latency=\(ms)ms outcome=\(terms.isEmpty ? "empty" : "terms", privacy: .public)")
            return terms
        case .timedOut:
            searchLog.info("expansion query=\(trimmed, privacy: .public) latency=\(ms)ms outcome=timeout")
            return []
        case .failed(let error):
            searchLog.error("expansion query=\(trimmed, privacy: .public) latency=\(ms)ms outcome=error \(error.localizedDescription, privacy: .public)")
            return []
        }
    }

    private enum Outcome: Sendable {
        case terms([String])
        case timedOut
        case failed(any Error)
    }

    /// One model call, its error kept as a value.
    private static func attempt(_ session: any ExpansionSession, _ prompt: String) async -> Outcome {
        do { return .terms(try await session.respond(to: prompt)) } catch { return .failed(error) }
    }

    /// The model call raced against the timeout; whichever finishes first wins and the
    /// other is cancelled.
    private func respond(_ session: any ExpansionSession, _ prompt: String) async -> Outcome {
        let timeout = self.timeout
        return await withTaskGroup(of: Outcome.self) { group in
            group.addTask { await Self.attempt(session, prompt) }
            group.addTask {
                try? await Task.sleep(for: timeout)
                return .timedOut
            }
            let first = await group.next() ?? .timedOut
            group.cancelAll()
            return first
        }
    }
}

/// Pure result-filtering for expansion terms: the unit-testable core of the model
/// path (the real model is unavailable in CI / the Simulator). Trims each term,
/// drops blanks, drops any term equal (case-insensitively) to the original query,
/// de-duplicates case-insensitively, and caps at `maxTerms`. Empty in -> empty out.
///
/// Foundation-only and free of any FoundationModels type, so it is testable from a
/// target that never imports the model framework.
// `nonisolated` explicitly: JesseSearch compiles under `.defaultIsolation(MainActor.self)`
// for the model and the expander, and this pure helper is the documented exception. Same
// convention as the pure declarations in `FlagSync.swift`.
public nonisolated func filterExpansionTerms(_ raw: [String], original: String, maxTerms: Int = 4) -> [String] {
    let originalKey = original.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    var out: [String] = []
    for term in raw {
        let trimmed = term.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { continue }
        let key = trimmed.lowercased()
        if key == originalKey { continue }
        if out.contains(where: { $0.lowercased() == key }) { continue }
        out.append(trimmed)
        if out.count == maxTerms { break }
    }
    return out
}
