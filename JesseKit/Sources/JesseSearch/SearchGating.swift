import Foundation
import JesseVault

// The Tier-2 expansion GATING decision: pure, deterministic, view-free, so both
// the orchestration model and its tests can reason about WHEN the on-device query
// expander is worth invoking without a real model or a view host. Extracted into
// the shared JesseSearch library so iOS and macOS gate the expansion tier the same
// way (the pure multi-token match predicate itself lives in JesseConversations).

/// Whether the query expansion tier is worth invoking: a trimmed query of three or more
/// characters (so trivial 1 to 2 character queries never spend the model), with the
/// tier enabled and the model available. The base match count is not a condition; see
/// `SearchQueryRules.shouldExpand`. Pure and deterministic.
// `nonisolated` explicitly, for the reason given on `filterExpansionTerms`: a pure decision
// in a MainActor-default target, asserted directly from a nonisolated test.
///
/// ONE LINE, and it forwards, for the reason `significantTokens` does: the vault search
/// gates its own expansion tier and must gate it identically. The implementation moved to
/// `SearchQueryRules.shouldExpand` in JesseVault, the one target both searches can reach.
public nonisolated func shouldExpand(query: String, enabled: Bool = true,
                                     available: Bool = true) -> Bool {
    SearchQueryRules.shouldExpand(query: query, enabled: enabled, available: available)
}
