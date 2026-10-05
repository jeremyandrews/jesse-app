import Foundation
import FoundationModels
import JesseVault
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
// ONE GROUP PER WORD, NOT WHOLE QUERIES. Asked for "alternative search terms for the
// same thing", the model paraphrased around the strongest noun and kept it: `lost keys`
// gave `missing keys`, `keys not found`, `search for keys`, `found keys`, every one of
// them repeating `keys` and none bringing a new word for it. It is now asked, for each
// significant word of the query, for other words that mean the same thing or name the
// same object, never containing the word itself, and `SearchQueryRules.filterConcepts`
// enforces that deterministically whatever the model returns.
//
// FoundationModels ships on iOS 26 and macOS 26, so this same expander backs both
// the iPhone and the Mac search. Availability here is about whether the *model* is
// usable at runtime, not whether the framework is present.

/// On-device query-expansion diagnostics: one line per expansion (query, terms,
/// latency, outcome) and availability. Package-local so the expander doesn't depend on
/// the app target's logging.
private let searchLog = Logger(subsystem: "com.tag1.jesse", category: "search")

/// Guided-generation output: one group per query word. `@Generable` + `@Guide`
/// constrain the model to return exactly this shape.
@Generable
private struct ExpansionGroups {
    @Guide(description: "One group for each listed word of the query, in the order listed",
           .maximumCount(8))
    var groups: [ExpansionGroup]
}

@Generable
private struct ExpansionGroup {
    @Guide(description: "The query word this group replaces, exactly as listed")
    var word: String
    @Guide(description: "Up to 4 other words or short phrases that mean the same thing or name the same object, including the singular or plural and common variants. Never containing the query word itself.",
           .maximumCount(4))
    var alternatives: [String]
}

/// One expansion conversation with the model: the seam a test stubs to see what each
/// call's session was asked. Main-actor bound like the expander that owns it.
@MainActor
public protocol ExpansionSession: AnyObject, Sendable {
    func prewarm()
    /// The model's groups, unfiltered.
    func respond(to prompt: String) async throws -> [ExpansionConcept]
}

/// The real session: a `LanguageModelSession` holding only the instructions.
private final class LanguageModelExpansionSession: ExpansionSession {
    private let session = LanguageModelSession(instructions: FoundationModelExpander.instructions)

    nonisolated deinit {}

    func prewarm() { session.prewarm() }

    func respond(to prompt: String) async throws -> [ExpansionConcept] {
        try await session.respond(to: prompt, generating: ExpansionGroups.self).content.groups
            .map { ExpansionConcept(word: $0.word, alternatives: $0.alternatives) }
    }
}

@MainActor
public final class FoundationModelExpander: QueryExpanding {
    public typealias SessionFactory = @MainActor () -> any ExpansionSession

    static let instructions = """
    You help search a list of past conversations. You are given the words of a search \
    query. For each word, give up to 4 other words or short phrases that mean the same \
    thing or name the same object, including the singular or plural and common \
    variants. An alternative must never contain the word it replaces, and must never \
    repeat the rest of the query: give replacements for one word, not rewritten queries.

    Example. Query: "lost keys". Words: lost, keys.
    lost: misplaced, missing, can't find, forgot
    keys: key, keychain, fob, car key
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

    /// One concept per significant word of `query`, filtered, or `[]` when the model is
    /// unavailable, the query has no word worth expanding, the call fails, or it takes
    /// longer than the timeout. Never throws: the search tier treats `[]` as "no
    /// expansion".
    public func expand(_ query: String) async -> [ExpansionConcept] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        guard availability.isAvailable else {
            searchLog.info("expansion query=\(trimmed, privacy: .public) outcome=unavailable")
            return []
        }
        let words = SearchQueryRules.conceptWords(trimmed)
        guard !words.isEmpty else {
            searchLog.info("expansion query=\(trimmed, privacy: .public) outcome=nowords")
            return []
        }

        // A clean transcript for every call: the prewarmed session once, else a new one.
        let session = warmed ?? makeSession()
        warmed = nil
        let prompt = Self.prompt(query: trimmed, words: words)
        let clock = ContinuousClock()
        let started = clock.now
        let outcome = await respond(session, prompt)
        let ms = Int((clock.now - started) / .milliseconds(1))

        switch outcome {
        case .groups(let raw):
            let concepts = SearchQueryRules.filterConcepts(raw, query: trimmed)
            let useful = SearchQueryRules.hasAlternatives(concepts)
            searchLog.info("expansion query=\(trimmed, privacy: .public) groups=\(SearchQueryRules.logDescription(concepts), privacy: .public) latency=\(ms)ms outcome=\(useful ? "groups" : "empty", privacy: .public)")
            return useful ? concepts : []
        case .timedOut:
            searchLog.info("expansion query=\(trimmed, privacy: .public) latency=\(ms)ms outcome=timeout")
            return []
        case .failed(let error):
            searchLog.error("expansion query=\(trimmed, privacy: .public) latency=\(ms)ms outcome=error \(error.localizedDescription, privacy: .public)")
            return []
        }
    }

    /// The per-call prompt: the query and the words to give groups for.
    static func prompt(query: String, words: [String]) -> String {
        """
        Query: "\(query)". Words: \(words.joined(separator: ", ")).
        For each word, give up to 4 alternatives that never contain that word.
        """
    }

    private enum Outcome: Sendable {
        case groups([ExpansionConcept])
        case timedOut
        case failed(any Error)
    }

    /// One model call, its error kept as a value.
    private static func attempt(_ session: any ExpansionSession, _ prompt: String) async -> Outcome {
        do { return .groups(try await session.respond(to: prompt)) } catch { return .failed(error) }
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
