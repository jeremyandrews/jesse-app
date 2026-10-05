import Foundation
import JesseVault

// The query-expansion seam (Tier 2, framework-agnostic half). Kept Foundation-only
// and free of any model import so the orchestration model and its tests never pull
// in FoundationModels, mirroring how JesseClientProtocol isolates the network.
//
// A `QueryExpanding` turns one search query into concepts: for each significant word,
// the words that may stand in for it (`ExpansionConcept`). It is deliberately
// TOTAL: it NEVER throws to the caller. Unavailable, disabled, or failed all
// collapse to `[]`, so the search tier above can treat "no expansion" and "the
// model isn't here" identically and degrade silently to the multi-token base match.
//
// It also says whether it CAN expand right now (`availability`), so the gate skips a
// call that could only come back empty and Settings can say why the tier is idle.

/// Whether an expander can run, and if not, why, in words a person can act on.
public nonisolated enum QueryExpansionAvailability: Equatable, Sendable {
    case available
    case unavailable(reason: String)

    public var isAvailable: Bool { self == .available }
}

// Under the package's MainActor-default isolation this protocol (and its
// conformers) are main-actor-isolated; `expand` is `async` so it still suspends,
// letting a query change cancel an in-flight expansion, without blocking the list.
public protocol QueryExpanding {
    /// One concept per significant word of `query`, alternatives filtered. Returns `[]`
    /// when expansion is unavailable, fails, or found nothing to add; never throws.
    func expand(_ query: String) async -> [ExpansionConcept]

    /// Warm any expensive backing resource (e.g. an on-device model session) ahead
    /// of the first real query, called when the search field gains focus. Optional:
    /// the default does nothing, so a fake/plain expander needn't implement it.
    func prewarm()

    /// Whether `expand` can produce anything right now. The default is available, so a
    /// fake or plain expander needn't implement it.
    var availability: QueryExpansionAvailability { get }
}

extension QueryExpanding {
    public func prewarm() {}
    public var availability: QueryExpansionAvailability { .available }
}

/// The inert expander: always returns no alternate terms, so a `ThreadSearchModel`
/// built on it is pure Tier-1 (the typed query only). It is the safe default when a
/// real on-device expander shouldn't be constructed, e.g. SwiftUI previews and the
/// list-model unit tests, which must not instantiate the FoundationModels-backed
/// expander (a real model is unavailable there). Production injects
/// `FoundationModelExpander` explicitly.
public struct NoExpansion: QueryExpanding {
    public init() {}
    public func expand(_ query: String) async -> [ExpansionConcept] { [] }
}
