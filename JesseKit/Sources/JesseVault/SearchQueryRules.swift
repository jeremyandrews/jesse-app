import Foundation

// THE TWO QUERY RULES EVERY SEARCH IN THIS APP SHARES, in one place.
//
// They were written for the conversation list — `significantTokens` in
// JesseConversations, `shouldExpand` in JesseSearch — and the vault search built on
// top of this target has to obey both, or a query would mean one thing over chats
// and another over notes.
//
// SO WHY DO THEY LIVE HERE, of all places? Because of the direction of the arrows.
// JesseSearch depends on JesseConversations, which depends on JesseCore, which is
// the SwiftData model layer. This target deliberately depends on NOTHING, so it
// cannot reach either of them without dragging the whole chat layer into the vault
// index — which is the one thing `Package.swift` says this target must never do.
// The only place all three can reach is a leaf, and this target is the leaf.
//
// The two public entry points callers already know — `significantTokens(_:)` in
// JesseConversations and `shouldExpand(query:enabled:available:)` in
// JesseSearch — are now one-line forwarders to these. One rule, one implementation,
// three callers.

/// The tokenizing and gating rules shared by conversation search and vault search.
public enum SearchQueryRules {

    /// Tokens (length >= 2) of a trimmed query, split on whitespace.
    ///
    /// Lowercasing is deliberately NOT done here: the callers match with
    /// `localizedStandardContains` (case- and diacritic-insensitive) or through FTS5's
    /// `unicode61 remove_diacritics 2` tokenizer, and a token lowercased twice is a
    /// token whose original spelling is no longer available for a display caption.
    public static func significantTokens(_ trimmed: String) -> [Substring] {
        trimmed.split(whereSeparator: \.isWhitespace).filter { $0.count >= 2 }
    }

    /// Whether the query-expansion tier is worth invoking: a trimmed query of three or
    /// more characters, with expansion enabled and the model available.
    ///
    /// The base match count is deliberately NOT a condition. It used to be (expand only
    /// below five direct hits), and almost every real query finds five, so the model was
    /// almost never asked and the "Also searching" caption almost never appeared.
    /// Expansion only widens the set, and its hits rank below every direct hit, so a
    /// plentiful direct set loses nothing to it. Pure and deterministic.
    public static func shouldExpand(query: String, enabled: Bool = true,
                                    available: Bool = true) -> Bool {
        guard enabled, available else { return false }
        return query.trimmingCharacters(in: .whitespacesAndNewlines).count >= 3
    }
}

/// Alternate search terms for a query, as the vault search tier asks for them.
///
/// The same total contract `QueryExpanding` carries in JesseSearch, restated here for
/// the reason `SearchQueryRules` is here: this target cannot see that protocol without
/// depending on the chat layer. It NEVER throws — unavailable, disabled and failed all
/// collapse to `[]`, so the tier above treats "no expansion" and "no model on this
/// device" identically.
///
/// JesseSearch carries the one-line adapter that wraps its `QueryExpanding` expanders
/// (including the FoundationModels-backed one) in this, so the app injects the SAME
/// expander the conversation list uses and no second model session is ever created.
public protocol VaultQueryExpanding: Sendable {
    func expand(_ query: String) async -> [String]
}

/// The inert expander: no alternate terms, ever. The safe default for previews and for
/// tests, which must never construct a real on-device model session.
public struct NoVaultExpansion: VaultQueryExpanding {
    public init() {}
    public func expand(_ query: String) async -> [String] { [] }
}
