import Foundation
import SwiftData
import JesseCore
import JesseVault

// THE CONVERSATION SEARCH INDEX: one folded text document per thread, built once and
// kept current, and the one search pass the list runs per settled query.
//
// WHY IT EXISTS. The list used to filter with `threadMatches` from computed view
// properties: every keystroke walked every turn of every thread through
// `localizedStandardContains`, three times over (the base count, the union and the
// layout), on the main actor, faulting every turn body into memory on the way. On a
// 500 conversation store that was 140 to 170 ms of main thread work per keystroke.
//
// WHAT IT DOES INSTEAD.
//   * Each thread's searchable text (its title, then every turn's `searchableTexts` in
//     chronological order) is case, diacritic and width folded ONCE into UTF-8 bytes.
//     A search is then a `memmem` over bytes per needle, never a per turn
//     `localizedStandardContains` and never a turn fault.
//   * Documents are rebuilt only for threads whose title or `updatedAt` changed since
//     the last pass. Every code path that appends a turn bumps `updatedAt` (it is what
//     orders the list), so the pair is the thread's change stamp.
//   * The rebuild reads the store through its OWN `ModelContext` on this actor, off the
//     main actor, so even the first build never faults a turn body on the main thread.
//   * Ranking is title hits, then body hits, then threads found only through an
//     expansion concept (`CompiledConceptQuery`); each group newest first.
//
// MATCH SEMANTICS, by the trimmed query's length:
//   * 1 character: a word start in the TITLE only. "j" matching "just" in every reply
//     body would match the whole store, which is no search at all.
//   * 2 characters: a word start in the title or any body.
//   * 3 or more: exactly the old rule. Tokens of two or more characters, each found
//     anywhere (title or body), order and gap independent; a query of only short tokens
//     is matched as one raw needle. Every thread `threadMatches` finds, this finds.

// MARK: - Document

/// One thread's searchable text, folded once.
public nonisolated struct ThreadSearchDocument: Sendable {
    public let id: UUID
    public let updatedAt: Date
    /// The change stamp this document was built from.
    let title: String
    /// Original texts, in order: the title, then every searchable text of every turn in
    /// chronological order. Index 0 is always the title. Kept so a snippet can be cut
    /// from the one source a hit came from, without touching the thread's turns.
    public let sources: [String]
    /// The title, folded.
    let foldedTitle: [UInt8]
    /// Every source after the title, folded and joined with "\n" (a byte no needle can
    /// contain, since needles are whitespace-split).
    let foldedBody: [UInt8]
    /// The byte offset in `foldedBody` where each body source starts; entry i is
    /// `sources[i + 1]`.
    let bodyOffsets: [Int]

    public init(id: UUID, updatedAt: Date, title: String, texts: [String]) {
        self.id = id
        self.updatedAt = updatedAt
        self.title = title
        self.sources = [title] + texts
        self.foldedTitle = searchFold(title)
        var body: [UInt8] = []
        var offsets: [Int] = []
        offsets.reserveCapacity(texts.count)
        for text in texts {
            if !body.isEmpty { body.append(0x0A) }
            offsets.append(body.count)
            body.append(contentsOf: searchFold(text))
        }
        self.foldedBody = body
        self.bodyOffsets = offsets
    }

    /// The source index (into `sources`) a body byte offset falls in.
    func sourceIndex(forBodyOffset offset: Int) -> Int {
        // Last start <= offset; offsets ascend, so a binary search.
        var lo = 0, hi = bodyOffsets.count - 1
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if bodyOffsets[mid] <= offset { lo = mid } else { hi = mid - 1 }
        }
        return lo + 1
    }
}

/// Case, diacritic and width folding, the same three insensitivities
/// `localizedStandardContains` applies, done once so matching is a byte search.
public nonisolated func searchFold(_ text: String) -> [UInt8] {
    Array(text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive],
                       locale: .current).utf8)
}

// MARK: - Query

/// How one query entry matches, by its trimmed length (see the file header).
public nonisolated enum ThreadQueryMode: Sendable, Equatable {
    case titleWordStart
    case wordStart
    case substring

    /// The mode for a trimmed query.
    public static func of(_ trimmed: String) -> ThreadQueryMode {
        switch trimmed.count {
        case 1: return .titleWordStart
        case 2: return .wordStart
        default: return .substring
        }
    }
}

/// A query entry compiled to folded needles.
public nonisolated struct CompiledThreadQuery: Sendable {
    public let mode: ThreadQueryMode
    let needles: [[UInt8]]

    /// Nil for a blank query.
    public init?(_ query: String) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        mode = ThreadQueryMode.of(trimmed)
        switch mode {
        case .titleWordStart, .wordStart:
            needles = [searchFold(trimmed)]
        case .substring:
            let tokens = SearchQueryRules.significantTokens(trimmed)
            needles = tokens.isEmpty ? [searchFold(trimmed)] : tokens.map { searchFold(String($0)) }
        }
    }

    /// `.title` when every needle is in the title, `.body` when every needle is in the
    /// title or the body, nil otherwise. `snippetSource` is the source the earliest body
    /// match sits in (0, the title, for a title hit).
    func match(_ doc: ThreadSearchDocument) -> (kind: ThreadMatchKind, snippetSource: Int)? {
        let wordStart = mode != .substring
        var allInTitle = true
        var firstBody: Int?
        for needle in needles {
            if byteSearch(doc.foldedTitle, needle, wordStart: wordStart) != nil { continue }
            allInTitle = false
            if mode == .titleWordStart { return nil }
            guard let at = byteSearch(doc.foldedBody, needle, wordStart: wordStart) else {
                return nil
            }
            firstBody = min(firstBody ?? at, at)
        }
        if allInTitle { return (.title, 0) }
        return (.body, doc.sourceIndex(forBodyOffset: firstBody ?? 0))
    }
}

/// An expansion, compiled to folded needles: one concept per significant word of the
/// query, each its word plus its alternatives.
///
/// AND ACROSS CONCEPTS, OR WITHIN ONE. A thread matches when, for every concept, it
/// contains the word or one of its alternatives, and at least one concept was met only
/// through an alternative (otherwise the thread is a direct hit, or a near miss the typed
/// query rejected for a stop word it required). An alternative is matched whole, as a
/// phrase: `can't find` is one needle, not `can't` AND `find`. That is the point of the
/// concept form: the old shape matched each alternative query as an AND of its own
/// tokens, so `keys not found` required `keys`, `not` and `found`, and expansion could
/// only ever narrow inside the threads containing `keys`.
public nonisolated struct CompiledConceptQuery: Sendable {
    /// Per concept: the word's needle first, then its alternatives' needles. An
    /// alternative with an apostrophe also gets its curly spelling, since replies are
    /// typeset with `’` and the fold does not straighten it.
    let concepts: [(word: [UInt8], alternatives: [[UInt8]])]

    /// Nil when no concept carries an alternative: there is nothing to widen with.
    public init?(_ concepts: [ExpansionConcept]) {
        guard SearchQueryRules.hasAlternatives(concepts) else { return nil }
        self.concepts = concepts.map { concept in
            var needles: [[UInt8]] = []
            for alt in concept.alternatives {
                needles.append(searchFold(alt))
                if alt.contains("'") {
                    needles.append(searchFold(alt.replacingOccurrences(of: "'", with: "\u{2019}")))
                }
            }
            return (searchFold(concept.word), needles)
        }
    }

    /// The source the earliest body match sits in (0, the title, when every concept was
    /// met in the title), or nil when the thread is not an expansion hit.
    func match(_ doc: ThreadSearchDocument) -> Int? {
        var usedAlternative = false
        var allInTitle = true
        var firstBody: Int?
        for concept in concepts {
            if byteSearch(doc.foldedTitle, concept.word, wordStart: false) != nil { continue }
            if let at = byteSearch(doc.foldedBody, concept.word, wordStart: false) {
                allInTitle = false
                firstBody = min(firstBody ?? at, at)
                continue
            }
            var met = false
            for alt in concept.alternatives {
                if byteSearch(doc.foldedTitle, alt, wordStart: false) != nil {
                    met = true
                    break
                }
                if let at = byteSearch(doc.foldedBody, alt, wordStart: false) {
                    allInTitle = false
                    firstBody = min(firstBody ?? at, at)
                    met = true
                    break
                }
            }
            guard met else { return nil }
            usedAlternative = true
        }
        guard usedAlternative else { return nil }
        if allInTitle { return 0 }
        return doc.sourceIndex(forBodyOffset: firstBody ?? 0)
    }
}

// MARK: - Result

/// Why a thread is in a search result, which is also its rank group.
public nonisolated enum ThreadMatchKind: Int, Sendable, Comparable {
    case title = 0
    case body = 1
    case expansion = 2

    public static func < (a: Self, b: Self) -> Bool { a.rawValue < b.rawValue }
}

/// One ranked hit.
public nonisolated struct ThreadSearchHit: Sendable, Equatable {
    public let id: UUID
    public let kind: ThreadMatchKind
    /// The original text the snippet is cut from.
    public let snippetSource: String
}

/// The answer to one settled query: the ranked hits, with the expansion that produced
/// them.
public nonisolated struct ThreadSearchResult: Sendable, Equatable {
    /// The typed query this result answers, trimmed; empty means search is inactive.
    public let query: String
    /// The expansion concepts that were applied, one per significant query word.
    public let concepts: [ExpansionConcept]
    /// Title hits, then body hits, then expansion only hits, each newest first.
    public let hits: [ThreadSearchHit]

    public init(query: String, concepts: [ExpansionConcept], hits: [ThreadSearchHit]) {
        self.query = query
        self.concepts = concepts
        self.hits = hits
    }

    public static let inactive = ThreadSearchResult(query: "", concepts: [], hits: [])

    public var isActive: Bool { !query.isEmpty }

    /// Every alternative the expansion applied, flattened in concept order.
    public var terms: [String] { SearchQueryRules.alternatives(concepts) }

    /// Every query entry the result was matched with: the typed query plus the
    /// alternatives.
    public var queries: [String] { [query] + terms }

    /// The query entries one hit's snippet highlights. A direct hit matched the typed
    /// words, so only they are looked for; an expansion hit also carries the
    /// alternatives, which is what it matched through. Scanning a long reply for a dozen
    /// alternatives it cannot contain cost every visible direct row a full pass per
    /// alternative, which broke the keystroke budget.
    public func queries(for hit: ThreadSearchHit) -> [String] {
        hit.kind == .expansion ? queries : [query]
    }
}

/// The one search pass: match every document against the typed query and the
/// expansion concepts, then rank. Pure, and checks for cancellation as it goes, so a
/// stale pass stops early (its partial answer is never published).
public nonisolated func searchThreads(_ docs: [ThreadSearchDocument],
                                      query: String,
                                      concepts: [ExpansionConcept]) -> ThreadSearchResult {
    let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let typed = CompiledThreadQuery(trimmed) else { return .inactive }
    let expansion = CompiledConceptQuery(concepts)

    var found: [(doc: ThreadSearchDocument, kind: ThreadMatchKind, source: Int)] = []
    for (i, doc) in docs.enumerated() {
        if i % 64 == 0 && Task.isCancelled { break }
        if let m = typed.match(doc) {
            found.append((doc, m.kind, m.snippetSource))
            continue
        }
        if let source = expansion?.match(doc) {
            found.append((doc, .expansion, source))
        }
    }
    found.sort {
        if $0.kind != $1.kind { return $0.kind < $1.kind }
        if $0.doc.updatedAt != $1.doc.updatedAt { return $0.doc.updatedAt > $1.doc.updatedAt }
        return $0.doc.id.uuidString < $1.doc.id.uuidString
    }
    return ThreadSearchResult(
        query: trimmed, concepts: concepts,
        hits: found.map { ThreadSearchHit(id: $0.doc.id, kind: $0.kind,
                                          snippetSource: $0.doc.sources[$0.source]) })
}

// MARK: - Byte search

/// The first offset of `needle` in `hay`, or nil. With `wordStart`, only an occurrence
/// that begins a word counts: the character before it is not a letter or digit.
nonisolated func byteSearch(_ hay: [UInt8], _ needle: [UInt8], wordStart: Bool) -> Int? {
    guard !needle.isEmpty, needle.count <= hay.count else { return nil }
    return hay.withUnsafeBufferPointer { h -> Int? in
        needle.withUnsafeBufferPointer { n -> Int? in
            guard let hBase = h.baseAddress, let nBase = n.baseAddress else { return nil }
            var from = 0
            while from <= h.count - n.count {
                guard let p = memmem(hBase + from, h.count - from, nBase, n.count) else {
                    return nil
                }
                let at = UnsafePointer<UInt8>(p.assumingMemoryBound(to: UInt8.self)) - hBase
                if !wordStart || startsWord(h, at) { return at }
                from = at + 1
            }
            return nil
        }
    }
}

/// Whether offset `at` begins a word: the preceding character is not alphanumeric.
/// ASCII is decided from the byte; anything else decodes the preceding scalar, so a
/// curly quote before a word still counts as a boundary and a letter does not.
private nonisolated func startsWord(_ h: UnsafeBufferPointer<UInt8>, _ at: Int) -> Bool {
    guard at > 0 else { return true }
    let prev = h[at - 1]
    if prev < 0x80 {
        let c = Character(Unicode.Scalar(prev))
        return !(c.isLetter || c.isNumber)
    }
    // Walk back to the lead byte (at most 4 bytes) and decode the scalar.
    var start = at - 1
    while start > 0 && at - start < 4 && h[start] & 0xC0 == 0x80 { start -= 1 }
    var decoder = UTF8()
    var it = h[start..<at].makeIterator()
    guard case .scalarValue(let scalar) = decoder.decode(&it) else { return true }
    return !(scalar.properties.isAlphabetic || scalar.properties.numericType != nil)
}

// MARK: - The index

/// What the list hands the index per pass: a thread's identity and its change stamp.
/// Plain values, read from the main actor's threads without touching any turn.
public nonisolated struct ThreadSearchStamp: Sendable, Equatable {
    public let id: UUID
    public let title: String
    public let updatedAt: Date

    public init(id: UUID, title: String, updatedAt: Date) {
        self.id = id
        self.title = title
        self.updatedAt = updatedAt
    }
}

/// The index and its search, off the main actor.
///
/// It holds one document per thread and, on each pass, rebuilds only the documents
/// whose stamp changed, reading those threads through a fresh `ModelContext` of its
/// own (fresh per rebuild, so it always sees what the app last saved). A thread the
/// store does not have yet (inserted on the main context, not saved) has no document
/// until its next pass after the save.
public actor ThreadSearchIndex {
    private let container: ModelContainer
    private var documents: [UUID: ThreadSearchDocument] = [:]

    /// Instrumentation: how many documents have been (re)built. A test asserts an
    /// unchanged store rebuilds nothing on the second pass.
    public private(set) var buildCount = 0

    public init(container: ModelContainer) {
        self.container = container
    }

    /// Bring the documents for `stamps` up to date, then run the one search pass.
    /// The result's hits are limited to the stamped threads.
    public func search(_ stamps: [ThreadSearchStamp], query: String,
                       concepts: [ExpansionConcept]) -> ThreadSearchResult {
        refresh(stamps)
        if Task.isCancelled { return .inactive }
        var docs: [ThreadSearchDocument] = []
        docs.reserveCapacity(stamps.count)
        for stamp in stamps {
            if let doc = documents[stamp.id] { docs.append(doc) }
        }
        return searchThreads(docs, query: query, concepts: concepts)
    }

    /// Rebuild the documents whose stamp changed, and drop those for threads gone.
    public func refresh(_ stamps: [ThreadSearchStamp]) {
        var stale: [UUID] = []
        for stamp in stamps {
            if let doc = documents[stamp.id], doc.title == stamp.title,
               doc.updatedAt == stamp.updatedAt { continue }
            stale.append(stamp.id)
        }
        if documents.count > stamps.count {
            let live = Set(stamps.map(\.id))
            documents = documents.filter { live.contains($0.key) }
        }
        guard !stale.isEmpty else { return }
        for doc in Self.build(stale, container: container) {
            documents[doc.id] = doc
            buildCount += 1
        }
    }

    /// Read `ids` from the store and fold them. Batched so the first build is a few
    /// fetches rather than one per thread.
    private static func build(_ ids: [UUID], container: ModelContainer) -> [ThreadSearchDocument] {
        let context = ModelContext(container)
        var out: [ThreadSearchDocument] = []
        out.reserveCapacity(ids.count)
        var start = 0
        while start < ids.count {
            let batch = Array(ids[start..<min(start + 100, ids.count)])
            start += 100
            var descriptor = FetchDescriptor<JesseThread>(
                predicate: #Predicate { batch.contains($0.id) })
            descriptor.relationshipKeyPathsForPrefetching = [\.turns]
            guard let threads = try? context.fetch(descriptor) else { continue }
            for thread in threads {
                let turns = thread.turns.sorted { $0.createdAt < $1.createdAt }
                var texts: [String] = []
                texts.reserveCapacity(turns.count)
                for turn in turns { texts.append(contentsOf: turn.searchableTexts) }
                out.append(ThreadSearchDocument(id: thread.id, updatedAt: thread.updatedAt,
                                                title: thread.title, texts: texts))
            }
        }
        return out
    }
}
