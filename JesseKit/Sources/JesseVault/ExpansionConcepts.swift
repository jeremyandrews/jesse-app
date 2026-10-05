import Foundation

// QUERY EXPANSION AS CONCEPTS: one group of alternatives per word of the query.
//
// WHY GROUPS. The expander used to ask the model for whole alternative queries, and a
// small model answers that by paraphrasing around the strongest noun and keeping it
// verbatim: `lost keys` came back as `missing keys`, `keys not found`, `search for keys`,
// `found keys`. Each alternative was then matched as an AND of its own tokens, so every
// one of them needed `keys` and could only narrow inside the threads the typed query
// already reached. A thread that said `keychain` or `fob` was unreachable, and that
// thread is the whole reason expansion exists.
//
// So the model is now asked, per word, for other words that mean the same thing or name
// the same object, and a thread matches when every word of the query is present OR
// replaced by one of its alternatives: AND across concepts, OR within one.
//
// The types and the deterministic filter live here, in the leaf, for the reason
// `SearchQueryRules` does: the conversation index and the vault search both read them.

/// One significant word of a query and the alternatives that may stand in for it.
public nonisolated struct ExpansionConcept: Sendable, Equatable, Hashable {
    /// The query word, as typed (surrounding punctuation trimmed).
    public let word: String
    /// Words or short phrases that may replace it, best first. Never contain `word`.
    public let alternatives: [String]

    public init(word: String, alternatives: [String]) {
        self.word = word
        self.alternatives = alternatives
    }
}

extension SearchQueryRules {

    /// Most alternatives kept for one word.
    public static let maxAlternativesPerConcept = 4
    /// Most alternatives kept across the whole query.
    public static let maxAlternativesOverall = 12

    /// The words of a query worth expanding and matching as concepts: the significant
    /// tokens with function words removed (`LookupQuery.isStopWord`) and surrounding
    /// punctuation trimmed. `search for keys` gives `search`, `keys`: a stop word is
    /// never a required needle of an expansion.
    public static func conceptWords(_ query: String) -> [String] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        var out: [String] = []
        for token in significantTokens(trimmed) {
            let word = String(token).trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
            guard word.count >= 2, !LookupQuery.isStopWord(word) else { continue }
            if out.contains(where: { fold($0) == fold(word) }) { continue }
            out.append(word)
        }
        return out
    }

    /// Case, diacritic and width folded, with whitespace runs collapsed and a curly
    /// apostrophe straightened: the form every comparison here is made in.
    public static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive],
                     locale: .current)
            .replacingOccurrences(of: "\u{2019}", with: "'")
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
    }

    /// THE DETERMINISTIC FILTER over what the model returned, one concept per concept
    /// word of `query`, in query order. Pure.
    ///
    /// Groups are matched to the query's words by folded equality; a group for a word
    /// the query does not have is ignored. Within a group each alternative is trimmed,
    /// and dropped when it is blank, shorter than two characters, a single stop word, or when
    /// its folded form CONTAINS its own word (`car keys` for `keys` adds nothing under
    /// substring matching, and `keys not found` is exactly the restatement this exists
    /// to stop) or any other word of the query as a whole word (`missing keys` offered for `lost` is a
    /// rewritten query, not a replacement for one word; `missing` alone covers it). An
    /// alternative the word contains is kept (`key` for `keys` is broader).
    /// Duplicates within a group go, then each group is capped at
    /// `maxAlternativesPerConcept` and the whole at `maxAlternativesOverall`, taken
    /// round robin so every word keeps its best alternatives. A word nobody offered
    /// anything for is its own concept with no alternatives.
    public static func filterConcepts(_ raw: [ExpansionConcept], query: String,
                                      perConcept: Int = maxAlternativesPerConcept,
                                      overall: Int = maxAlternativesOverall) -> [ExpansionConcept] {
        let words = conceptWords(query)
        let keys = words.map(fold)
        var candidates: [[String]] = words.map { word in
            let key = fold(word)
            var kept: [String] = []
            var seen: Set<String> = []
            for group in raw where fold(group.word.trimmingCharacters(
                in: CharacterSet.alphanumerics.inverted)) == key {
                for alt in group.alternatives {
                    let trimmed = alt.trimmingCharacters(in: .whitespacesAndNewlines)
                        .split(whereSeparator: \.isWhitespace).joined(separator: " ")
                    let folded = fold(trimmed)
                    // A stop word only as a WHOLE alternative: `can't find` is a phrase
                    // that means something, though `can` alone is grammar.
                    let isStopWord = !folded.contains(" ") && LookupQuery.isStopWord(folded)
                    // Its own word anywhere inside (substring matching would find it
                    // anyway); another query word only as a whole word, so `ai` in a
                    // query does not cull `email`.
                    let altWords = Set(folded.split(separator: " ").map {
                        $0.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
                    })
                    guard folded.count >= 2, !isStopWord, !folded.contains(key),
                          !keys.contains(where: { altWords.contains($0) }),
                          !seen.contains(folded) else { continue }
                    seen.insert(folded)
                    kept.append(trimmed)
                    if kept.count == perConcept { break }
                }
                if kept.count == perConcept { break }
            }
            return kept
        }

        // Round robin: every word's best alternative before any word's second.
        var taken = Array(repeating: 0, count: words.count)
        var total = 0
        var rank = 0
        while total < overall {
            var progressed = false
            for i in words.indices where rank < candidates[i].count && total < overall {
                taken[i] += 1
                total += 1
                progressed = true
            }
            if !progressed { break }
            rank += 1
        }
        for i in words.indices { candidates[i] = Array(candidates[i].prefix(taken[i])) }
        return zip(words, candidates).map { ExpansionConcept(word: $0, alternatives: $1) }
    }

    /// Whether any concept carries an alternative: an expansion with none adds nothing.
    public static func hasAlternatives(_ concepts: [ExpansionConcept]) -> Bool {
        concepts.contains { !$0.alternatives.isEmpty }
    }

    /// Every alternative, flattened in concept order: what a snippet highlights.
    public static func alternatives(_ concepts: [ExpansionConcept]) -> [String] {
        concepts.flatMap(\.alternatives)
    }

    /// The caption form, grouped by the word each alternative replaces:
    /// `misplaced, missing · key, keychain`. Empty when there are no alternatives.
    public static func caption(_ concepts: [ExpansionConcept]) -> String {
        concepts.filter { !$0.alternatives.isEmpty }
            .map { $0.alternatives.joined(separator: ", ") }
            .joined(separator: " · ")
    }

    /// The log form: `lost: misplaced, missing; keys: key, keychain`.
    public static func logDescription(_ concepts: [ExpansionConcept]) -> String {
        concepts.map { "\($0.word): \($0.alternatives.joined(separator: ", "))" }
            .joined(separator: "; ")
    }

    /// Whole alternate queries for a search that takes strings (the vault's FTS path):
    /// the typed query with ONE concept word replaced by one of its alternatives, best
    /// first. Round robin over the words by rank, so `lost keys` gives `misplaced keys`,
    /// `lost key`, `missing keys`, `lost keychain`. Capped at `limit`, deduplicated, and
    /// never the query itself.
    public static func substitutionQueries(_ query: String, concepts: [ExpansionConcept],
                                           limit: Int = 4) -> [String] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let tokens = trimmed.split(whereSeparator: \.isWhitespace).map(String.init)
        var out: [String] = []
        var seen: Set<String> = [fold(trimmed)]
        let deepest = concepts.map(\.alternatives.count).max() ?? 0
        for rank in 0..<deepest {
            for concept in concepts where rank < concept.alternatives.count {
                let key = fold(concept.word)
                guard let at = tokens.firstIndex(where: {
                    fold($0.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)) == key
                }) else { continue }
                var replaced = tokens
                replaced[at] = concept.alternatives[rank]
                let candidate = replaced.joined(separator: " ")
                guard seen.insert(fold(candidate)).inserted else { continue }
                out.append(candidate)
                if out.count == limit { return out }
            }
        }
        return out
    }
}
