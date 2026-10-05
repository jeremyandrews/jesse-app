import Foundation
import JesseCore
import JesseVault

// The highlighted matched snippet a search row shows. Shared by the iPhone and the Mac,
// and cut from ONE source text: the search pass already knows which text a hit came from
// (`ThreadSearchHit.snippetSource`), so a visible row never rescans or re-sorts its
// thread's turns. Foundation-only and view-free so it stays unit-testable.

/// A windowed excerpt of the text that matched, plus the ranges within `text`
/// that should be highlighted. `ranges` index into `text` (an `AttributedString`
/// built by the view highlights exactly those). Non-empty `ranges` by construction.
public nonisolated struct SearchSnippet: Equatable, Sendable {
    public let text: String
    public let ranges: [Range<String.Index>]

    public init(text: String, ranges: [Range<String.Index>]) {
        self.text = text
        self.ranges = ranges
    }
}

/// The snippet for one search hit, cut from the source the search pass recorded.
public nonisolated func searchSnippet(for hit: ThreadSearchHit,
                                      queries: [String],
                                      contextWords: Int = 4) -> SearchSnippet? {
    searchSnippet(sources: [hit.snippetSource], queries: queries, contextWords: contextWords)
}

/// A windowed, highlighted excerpt centered on the FIRST matched token for a thread,
/// or nil when the query list is empty/blank (search inactive → no snippet).
///
/// The source is the title when a token matched there, else the first turn body
/// containing a match. Kept for callers holding a thread rather than a search hit.
public func searchSnippet(for thread: JesseThread,
                          queries: [String],
                          contextWords: Int = 4) -> SearchSnippet? {
    searchSnippet(sources: [thread.title] + thread.orderedTurns.flatMap(\.searchableTexts),
                  queries: queries, contextWords: contextWords)
}

/// The excerpt from the first of `sources` with a match. A few words of context are
/// kept on each side and the excerpt is ellipsized when it doesn't reach the text's
/// start/end. A one or two character entry highlights word starts only, the same rule
/// the search applies to it.
public nonisolated func searchSnippet(sources: [String],
                                      queries: [String],
                                      contextWords: Int = 4) -> SearchSnippet? {
    let all = snippetTokens(from: queries)
    guard !all.isEmpty else { return nil }
    for source in sources {
        let tokens = tokensPresent(all, in: source)
        guard let first = firstMatchRange(in: source, tokens: tokens) else { continue }
        return windowedSnippet(from: source, around: first, tokens: tokens,
                               contextWords: contextWords)
    }
    return nil
}

/// One token to highlight, and whether only a word-start occurrence counts.
private nonisolated struct SnippetToken {
    let text: String
    let wordStart: Bool
}

/// The significant tokens to highlight for a snippet: the >=2-char tokens of each
/// active query entry, or — when an entry is entirely short — that raw entry, so a
/// short search still highlights. Deduped, blanks dropped.
private nonisolated func snippetTokens(from queries: [String]) -> [SnippetToken] {
    var out: [SnippetToken] = []
    for q in queries {
        let trimmed = q.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { continue }
        let wordStart = ThreadQueryMode.of(trimmed) != .substring
        let toks = SearchQueryRules.significantTokens(trimmed)
        let pieces = toks.isEmpty || wordStart ? [trimmed] : toks.map(String.init)
        for p in pieces where !out.contains(where: { $0.text.caseInsensitiveCompare(p) == .orderedSame }) {
            out.append(SnippetToken(text: p, wordStart: wordStart))
        }
    }
    return out
}

/// The tokens that occur in `source` at all, decided over the folded bytes the search
/// pass uses. A locale-aware `range(of:)` for a token that is absent walks the whole
/// source, and an expansion hit carries a dozen alternatives, most of them absent from
/// any one text: looking each one up that way cost a visible row over a millisecond.
/// One fold and a `memmem` per token is the cheap presence test; the ranged search then
/// runs only for tokens that will be found. A single token is passed through untested.
private nonisolated func tokensPresent(_ tokens: [SnippetToken], in source: String) -> [SnippetToken] {
    guard tokens.count > 1 else { return tokens }
    let folded = searchFold(source)
    return tokens.filter { byteSearch(folded, searchFold($0.text), wordStart: false) != nil }
}

/// Every range of `token` in `text` (case/diacritic-insensitive), keeping only word
/// starts when the token asks for them; with `first`, stops at the first one.
private nonisolated func ranges(of token: SnippetToken, in text: String,
                                first: Bool = false) -> [Range<String.Index>] {
    var out: [Range<String.Index>] = []
    var searchStart = text.startIndex
    while searchStart < text.endIndex,
          let r = text.range(of: token.text, options: [.caseInsensitive, .diacriticInsensitive],
                             range: searchStart..<text.endIndex) {
        if !token.wordStart || r.lowerBound == text.startIndex
            || !isWordCharacter(text[text.index(before: r.lowerBound)]) {
            out.append(r)
            if first { break }
        }
        searchStart = r.upperBound
    }
    return out
}

private nonisolated func isWordCharacter(_ c: Character) -> Bool { c.isLetter || c.isNumber }

/// The earliest range in `text` matched by any token.
private nonisolated func firstMatchRange(in text: String,
                                         tokens: [SnippetToken]) -> Range<String.Index>? {
    var earliest: Range<String.Index>?
    for token in tokens {
        if let r = ranges(of: token, in: text, first: true).first,
           earliest == nil || r.lowerBound < earliest!.lowerBound {
            earliest = r
        }
    }
    return earliest
}

/// Build the windowed excerpt around `match` in `source`, keeping `contextWords`
/// whole words on each side, ellipsizing when the window doesn't reach an end, and
/// re-locating every token's range within the produced excerpt for highlighting.
private nonisolated func windowedSnippet(from source: String,
                                         around match: Range<String.Index>,
                                         tokens: [SnippetToken],
                                         contextWords: Int) -> SearchSnippet {
    // Word boundaries around the match, expanded by `contextWords` on each side.
    let (lo, hi, atStart, atEnd) = windowBounds(in: source, around: match,
                                                contextWords: contextWords)
    var excerpt = String(source[lo..<hi])
    if !atStart { excerpt = "…" + excerpt }
    if !atEnd { excerpt = excerpt + "…" }

    // Highlight every token occurrence within the produced excerpt.
    var found: [Range<String.Index>] = []
    for token in tokens {
        found.append(contentsOf: ranges(of: token, in: excerpt))
    }
    found.sort { $0.lowerBound < $1.lowerBound }
    return SearchSnippet(text: excerpt, ranges: found)
}

/// Compute the character bounds of a snippet window: starting from the match, walk
/// out `contextWords` whitespace-separated words on each side. Returns the bounds
/// and whether each side reached the text's true start/end (so the caller knows
/// whether to ellipsize).
private nonisolated func windowBounds(in source: String,
                                      around match: Range<String.Index>,
                                      contextWords: Int) -> (String.Index, String.Index, Bool, Bool) {
    // Walk left from the match's lower bound over `contextWords` words.
    var lo = match.lowerBound
    var wordsLeft = contextWords
    while lo > source.startIndex {
        let prev = source.index(before: lo)
        // Skip a run of whitespace, then a run of non-whitespace = one word.
        if source[prev].isWhitespace {
            // At a whitespace boundary: consuming another word costs one budget.
            if wordsLeft == 0 { break }
            wordsLeft -= 1
            // Skip contiguous whitespace.
            var i = prev
            while i > source.startIndex && source[source.index(before: i)].isWhitespace {
                i = source.index(before: i)
            }
            lo = i
        } else {
            lo = prev
        }
    }

    // Walk right from the match's upper bound over `contextWords` words.
    var hi = match.upperBound
    var wordsRight = contextWords
    while hi < source.endIndex {
        if source[hi].isWhitespace {
            if wordsRight == 0 { break }
            wordsRight -= 1
            var i = hi
            while i < source.endIndex && source[i].isWhitespace {
                i = source.index(after: i)
            }
            hi = i
        } else {
            hi = source.index(after: hi)
        }
    }

    let atStart = lo == source.startIndex
    let atEnd = hi == source.endIndex
    return (lo, hi, atStart, atEnd)
}
