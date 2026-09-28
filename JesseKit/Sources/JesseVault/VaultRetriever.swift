import Foundation

// WHAT THE MODEL IS SHOWN, AND WHY IT IS SO LITTLE.
//
// Retrieval first, model last. The index already knows where the answer is — that is
// what an inverted index over seven thousand notes IS — and the model's entire job is
// to read four paragraphs and say which sentence answers the question. Every line here
// exists to make those four paragraphs the right four and to keep them inside a budget
// this device actually has.
//
// THE BUDGET IS MEASURED, NOT ASSUMED. `ModelProbe` grows a prompt on the real device
// until the session refuses it. That number (33,500 characters on the phone, 21,000 on
// the Mac, measured 2026-09-22) is persisted by the diagnostics screen and read here.
// Absent it, the fallback is deliberately small rather than optimistic: a prompt that
// is refused is not a degraded answer, it is no answer at all.
//
// TWO ORDERS, FUSED. The lexical order knows about words; a sentence embedding knows
// that "concert" and "recital" are the same thing and that a note about a brick order is
// not about either. Neither is reliable alone, and choosing between them per query is a
// tuning problem nobody can see the inputs to, so both orders are computed and combined
// by reciprocal rank fusion — which needs only the RANKS, never a calibration between two
// score scales that have nothing to do with each other. With no embedding available, the
// lexical order simply stands. (That order is the CONCEPT RANK below, with bm25 breaking
// its ties, rather than bm25 alone.)
//
// `Inbox/` IS NEVER RETRIEVED FROM. It holds pasted mail and scan output: text written
// by other people, sitting in the vault, that would otherwise be lifted verbatim into a
// model prompt. Excluding it is not tidiness, it is the boundary that keeps this path
// from reading instructions off an email.
//
// `archive/` IS RETRIEVED FROM LAST. Not excluded — a closed research report is the only
// source for what it concluded — but every live note first, because a question asked today
// is almost never answered by finished work. Two archived notes answering "what's my
// birthday?" with the start date of a trip is what put this rule here.
//
// A FIRST PERSON QUESTION IS ABOUT SOMEBODY, and the vault knows his name. "my", "me" and
// "I" are grammar with no counterpart in a note: nothing in this vault refers to its owner
// as "my". So the question's own first-person words are replaced by the owner's NAME from
// the app's setting, and `what's my birthday` goes to the index as `birthday Jeremy` —
// which is how the precise pass finds the one heading that answers it instead of every
// note that says "birthday".
//
// THE NAME PROMOTES, IT DOES NOT GATE, and nor does any other word of the question. That
// is the 2026-09-27 fix and it replaced the three widening passes this file used to run:
// the question is PLANNED into concept groups (`LookupPlan`), ONE query ORs every term of
// every group, and a chunk is ranked by HOW MANY GROUPS it matched. A booking that never
// says whose it is still comes back; a booking that says `JEREMIAH` and carries today's
// date comes back ahead of it. The reasoning, and why a query per term is the thing to
// avoid, is written at the top of `LookupPlan`.

/// One chunk, retrieved whole, with the two facts that let a citation open it.
public struct RetrievedChunk: Equatable, Sendable {
    /// Path relative to the vault root — the citation's identity.
    public let path: String
    /// The 1-based line the chunk starts at.
    public let line: Int
    public let title: String
    public let heading: String
    /// The chunk's body, clipped to its share of the budget.
    public let text: String

    public init(path: String, line: Int, title: String, heading: String, text: String) {
        self.path = path
        self.line = line
        self.title = title
        self.heading = heading
        self.text = text
    }

    /// `Workshop/Kiln-Rebuild.md:12` — how the prompt names a note and how a citation
    /// is displayed.
    public var reference: String { "\(path):\(line)" }
}

/// Similarity between two pieces of text, as the re-ranker needs it.
///
/// A seam for the same reason every model dependency in this package is one: the real
/// implementation links a framework, and a test must be able to state "this chunk is
/// closer than that one" without one.
public protocol ChunkEmbedding: Sendable {
    /// False when this device has no sentence embedding, in which case bm25's order
    /// stands and nothing is re-ranked.
    var isAvailable: Bool { get }
    /// Cosine similarity in roughly -1...1, higher meaning closer. Nil for a pair the
    /// embedding cannot place, which is treated as "no opinion" rather than as zero.
    func similarity(_ lhs: String, _ rhs: String) -> Double?
}

/// No embedding on this device. bm25 alone, which is a complete answer and not a
/// degraded one.
public struct NoChunkEmbedding: ChunkEmbedding {
    public init() {}
    public var isAvailable: Bool { false }
    public func similarity(_ lhs: String, _ rhs: String) -> Double? { nil }
}

/// How many chunks, and how many characters of them.
public struct VaultRetrievalBudget: Equatable, Sendable {
    /// How many chunks go in the prompt.
    public let chunkCount: Int
    /// The ceiling on all of their text together.
    public let totalCharacters: Int

    public init(chunkCount: Int, totalCharacters: Int) {
        self.chunkCount = max(1, chunkCount)
        self.totalCharacters = max(1, totalCharacters)
    }

    /// One chunk's share. Equal shares rather than "fill greedily from the top",
    /// because a greedy fill lets one long chunk eat the other three — and the fourth
    /// chunk is disproportionately often the one with the date in it.
    public var perChunkCharacters: Int {
        max(1, totalCharacters / chunkCount)
    }

    /// Four chunks, or two on a device whose measured window is small.
    public static let defaultChunkCount = 4
    public static let smallChunkCount = 2
    /// Below this measured maximum, four chunks cannot each be long enough to be worth
    /// sending, so the count halves and each one gets a real share instead.
    public static let smallWindowCharacters = 6_000
    /// The share of the measured maximum the snippets may spend. The rest is the
    /// question, the instructions, and the answer the model still has to produce.
    public static let snippetShare = 0.6
    /// What is used when the probe has never been run on this device. Deliberately
    /// below the smallest window measured so far rather than at it.
    public static let unmeasuredCharacters = 12_000

    /// The budget implied by the probe's measured maximum prompt size.
    public static func forMeasuredPrompt(_ measured: Int?) -> VaultRetrievalBudget {
        guard let measured, measured > 0 else {
            return VaultRetrievalBudget(chunkCount: defaultChunkCount,
                                        totalCharacters: unmeasuredCharacters)
        }
        let count = measured < smallWindowCharacters ? smallChunkCount : defaultChunkCount
        return VaultRetrievalBudget(chunkCount: count,
                                    totalCharacters: Int(Double(measured) * snippetShare))
    }

    /// The sentence the Settings row shows, so the number and what it implies are never
    /// two separate claims a reader has to reconcile.
    public static func describe(_ measured: Int?) -> String {
        let budget = forMeasuredPrompt(measured)
        guard let measured else {
            return "Prompt size not measured — using \(budget.chunkCount) note extracts, "
                + "\(budget.totalCharacters) characters."
        }
        return "Measured prompt size \(measured) characters — using \(budget.chunkCount) "
            + "note extracts, \(budget.totalCharacters) characters."
    }
}

/// Reciprocal rank fusion. Pure, and stated over ranks alone.
public enum RankFusion {
    /// The constant that decides how fast a rank's contribution decays. 60 is the
    /// value the method was published with and the value every later comparison is
    /// stated against; it is not tuned here, because tuning it against a fixture
    /// corpus would be fitting a constant to six invented notes.
    public static let k = 60.0

    /// Fuse several orderings of the same items into one.
    ///
    /// Each ordering is a list of keys, best first. An item absent from an ordering
    /// simply contributes nothing from it, which is what makes a partial second
    /// opinion usable: the embedding can decline to place a chunk without that chunk
    /// being pushed to the bottom.
    public static func fuse(_ orderings: [[String]]) -> [String: Double] {
        var scores: [String: Double] = [:]
        for ordering in orderings {
            for (index, key) in ordering.enumerated() {
                scores[key, default: 0] += 1.0 / (k + Double(index + 1))
            }
        }
        return scores
    }
}

/// The chunks one question gets answered from.
public struct VaultRetriever: Sendable {
    /// How many hits survive into the re-ranking. Twenty rather than the four that
    /// survive it, because the fusion has to have something to reorder.
    public static let searchLimit = 20
    /// How many chunks the one lexical query asks the index for, before anything is
    /// ranked by concept.
    ///
    /// Larger than `searchLimit` because bm25 is not the order that decides here: the
    /// chunk that matches three of the question's concepts has to be IN the rows SQL
    /// returned before the group rank can put it first. A hundred and twenty rows cost
    /// one more page of the index and a body read each; the alternative is a query per
    /// term, which is what this replaced.
    public static let lexicalScanLimit = 120
    /// Below this many base hits the expansion tier is worth spending — the same
    /// threshold the conversation list and the vault search already use.
    public static let expansionThreshold = 5
    /// The one directory that is never retrieved from, with its whole subtree.
    public static let excludedPrefix = "Inbox/"
    /// The directory name that means finished work, anywhere in a path.
    public static let archiveSegment = VaultArchive.segment

    private let index: VaultIndex
    private let expander: any VaultQueryExpanding
    private let embedding: any ChunkEmbedding
    /// How the vault names the person asking, from the app's owner-name setting: one
    /// name, or several spellings separated by commas. Nil on a device that has no name
    /// for him, where every question behaves as it always did.
    private let ownerName: String?
    /// What day it is, which is the only way "today" can mean anything to an index.
    private let clock: VaultClock

    public init(index: VaultIndex,
                expander: any VaultQueryExpanding = NoVaultExpansion(),
                embedding: any ChunkEmbedding = NoChunkEmbedding(),
                ownerName: String? = nil,
                clock: VaultClock = .device) {
        self.index = index
        self.expander = expander
        self.embedding = embedding
        self.ownerName = ownerName
        self.clock = clock
    }

    /// What one question retrieved, and what it cost — the numbers the diagnostics
    /// list shows, kept beside the chunks so a row can never describe a different run.
    public struct Result: Sendable {
        public let chunks: [RetrievedChunk]
        /// Hits the search returned, after the `Inbox/` exclusion.
        public let hitCount: Int
        /// Whether a sentence embedding actually contributed an ordering.
        public let embedded: Bool

        public init(chunks: [RetrievedChunk], hitCount: Int, embedded: Bool) {
            self.chunks = chunks
            self.hitCount = hitCount
            self.embedded = embedded
        }

        /// The characters actually put in front of the model.
        public var characters: Int { chunks.reduce(0) { $0 + $1.text.count } }
    }

    /// Retrieve, re-rank, and clip to `budget`.
    ///
    /// ONE QUERY, RANKED BY CONCEPT, and a widening tier behind it for the rare question
    /// nothing at all matches:
    ///
    ///   1. THE PLAN'S TERMS, ORed. Every content word of the question, every spelling
    ///      of the owner's name, and every absolute form of the day a relative word
    ///      named. Nothing is required, so no single word can empty the result — which
    ///      is what "today" and the owner's name each did before this.
    ///   2. The app's own expander, on the same threshold the conversation list and the
    ///      vault search use, and only when pass 1 found almost nothing. It can only
    ///      ADD: pass 1's hits keep their place.
    ///
    /// Ahead of pass 1, for a first-person question with an owner name only, the chunks
    /// whose title or heading says they are his (`LookupPlan.subjectExpression`).
    ///
    /// Then the rank: how many of the plan's concept GROUPS each chunk matches, then
    /// whether it is the owner's, bm25 breaking the ties, one chunk per file; the owner's
    /// chunks lead the live ones, and the archive demotion and the embedding fusion are
    /// exactly as they were.
    public func retrieve(question: String, budget: VaultRetrievalBudget) async -> Result {
        let searcher = VaultSearcher(index: index,
                                     expansionThreshold: Self.expansionThreshold,
                                     limit: Self.searchLimit)
        let plan = LookupPlan.make(question: question, ownerName: ownerName, clock: clock)

        // THE CHUNKS THAT SAY THEY ARE THE OWNER'S come first, from a query of their own,
        // because the OR scan below is only so many rows deep and a vault full of the
        // owner's names can fill every one of them (`LookupPlan.subjectExpression`). First
        // so that a chunk both queries find carries this query's bm25, and chunks this one
        // found are compared with each other on one scale.
        var hits: [VaultSearchHit] = []
        if let subject = plan.subjectExpression {
            hits = Self.allowed(searcher.matching(expression: subject,
                                                  limit: Self.lexicalScanLimit))
        }
        var found = Set(hits.map(\.id))
        for hit in Self.allowed(searcher.matchingAny(plan.terms, limit: Self.lexicalScanLimit))
        where found.insert(hit.id).inserted {
            hits.append(hit)
        }
        if hits.count < Self.expansionThreshold {
            var seen = Set(hits.map(\.id))
            let query = plan.contentWords.joined(separator: " ")
            for hit in Self.allowed(await searcher.search(query, expander: expander).hits)
            where !seen.contains(hit.id) {
                seen.insert(hit.id)
                hits.append(hit)
            }
        }
        guard !hits.isEmpty else {
            return Result(chunks: [], hitCount: 0, embedded: false)
        }
        let hitCount = hits.count

        // The snippet a hit carries is fifteen words around the match. The BODY is what
        // gets read, so a hit whose chunk is no longer in the index (a reindex between
        // the search and this read) is dropped rather than sent as its own snippet.
        var bodies: [(hit: VaultSearchHit, text: String)] = []
        for hit in hits {
            guard let body = index.chunkText(path: hit.path, line: hit.line),
                  !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            else { continue }
            bodies.append((hit, body))
        }
        guard !bodies.isEmpty else {
            return Result(chunks: [], hitCount: hitCount, embedded: false)
        }
        bodies = Array(Self.byConcept(bodies, plan: plan).prefix(Self.searchLimit))

        // ARCHIVED NOTES RANK BEHIND LIVE ONES, ALWAYS. Fusing the two groups separately
        // and concatenating is what makes that absolute: the embedding reorders WITHIN a
        // group and can never lift a finished draft back above a note in use.
        //
        // AND A LIVE NOTE ABOUT THE OWNER LEADS THE LIVE ONES, by the same mechanism and for
        // the same reason: on 2026-09-27 the device's embedding lifted a note that only
        // mentioned him over the note whose heading is his birthday. `ownerLed` says which.
        let live = bodies.filter { !Self.isArchived($0.hit.path) }
        let archived = bodies.filter { Self.isArchived($0.hit.path) }
        let (lead, rest) = Self.ownerLed(live, plan: plan)
        let ordered = Self.fused(lead, question: question, embedding: embedding)
            + Self.fused(rest, question: question, embedding: embedding)
            + Self.fused(archived, question: question, embedding: embedding)
        let kept = ordered.prefix(budget.chunkCount)
        let chunks = kept.map { entry in
            RetrievedChunk(path: entry.hit.path,
                           line: entry.hit.line,
                           title: entry.hit.title,
                           heading: entry.hit.heading,
                           text: Self.clip(entry.text, to: budget.perChunkCharacters))
        }
        return Result(chunks: Array(chunks), hitCount: hitCount,
                      embedded: embedding.isAvailable)
    }

    // MARK: - Pure halves, asserted directly

    /// THE RANK THIS FILE EXISTS FOR: how many of the question's concept groups a chunk
    /// matches, bm25 breaking the ties, one chunk per file.
    ///
    /// bm25 alone cannot express it. It scores a chunk that says "flight" six times
    /// above one that says "flight" once and carries the date and the traveller's name,
    /// because it is counting words and the question is asking about things. Counting
    /// GROUPS is the whole difference, and it is why the owner's name and the day both
    /// promote a chunk without either being able to exclude one.
    ///
    /// A chunk is matched over its body, its heading, its title and its path, which is
    /// the same four columns the index searches — a flight table under `## Flight
    /// Details` answers "flight" through its heading and would otherwise be scored as
    /// though the word were not there.
    ///
    /// THE QUESTION'S OWN WORDS ARE THE FIRST KEY and the whole group count the second,
    /// for the reason `LookupPlan.score` gives: the owner and the date PROMOTE a chunk
    /// that is already about what was asked, and cannot carry one that is about nothing
    /// else.
    ///
    /// THEN WHETHER THE CHUNK IS ABOUT THE OWNER (`namesOwnerAsSubject`, over its title and
    /// heading), before the group count: among chunks that matched the question's words
    /// equally, `### Jeremy's Birthday` is his and a travellers line that names him is not.
    /// It is also what picks the right chunk of a note that lists everyone's birthday.
    ///
    /// COLLAPSED BY FILE AFTERWARDS, never before: the best chunk of a file is the one
    /// that matched the most concepts, not the one bm25 liked.
    public static func byConcept(_ bodies: [(hit: VaultSearchHit, text: String)],
                                 plan: LookupPlan) -> [(hit: VaultSearchHit, text: String)] {
        let ranked = bodies.enumerated().map {
            entry -> (index: Int, content: Int, subject: Bool, total: Int) in
            let searchable = [entry.element.hit.title, entry.element.hit.heading,
                              entry.element.hit.path, entry.element.text].joined(separator: " ")
            let score = plan.score(inTokens: LookupPlan.tokenize(searchable))
            return (entry.offset, score.content, isAboutOwner(entry.element.hit, plan: plan),
                    score.total)
        }
        let ordered = ranked.sorted { lhs, rhs in
            if lhs.content != rhs.content { return lhs.content > rhs.content }
            if lhs.subject != rhs.subject { return lhs.subject }
            if lhs.total != rhs.total { return lhs.total > rhs.total }
            let a = bodies[lhs.index].hit, b = bodies[rhs.index].hit
            if a.score != b.score { return a.score < b.score }
            if a.path != b.path { return a.path < b.path }
            return a.line < b.line
        }
        var seen = Set<String>()
        return ordered.compactMap { entry in
            let body = bodies[entry.index]
            guard seen.insert(body.hit.path).inserted else { return nil }
            return body
        }
    }

    /// Whether a hit's title or heading says it is the owner's: his name in possessive or
    /// subject position before a word of the question. Title and heading are read apart,
    /// so a title that ends in his name and a heading that starts with the word are not
    /// joined into a claim neither makes.
    static func isAboutOwner(_ hit: VaultSearchHit, plan: LookupPlan) -> Bool {
        [hit.title, hit.heading].contains {
            plan.namesOwnerAsSubject(inTokens: LookupPlan.tokenize($0))
        }
    }

    /// THE LIVE HITS SPLIT IN TWO: the ones about the owner that match as many of the
    /// question's words as any live hit does, and the rest, each keeping `byConcept`'s
    /// order.
    ///
    /// Fused as separate groups for the archive rule's reason: RRF over two orders lets
    /// an embedding that ranks the owner's heading low lift a decoy one place above it,
    /// which is what the device's embedding did on 2026-09-27. Splitting is the only way
    /// that ordering is not up to the embedding.
    ///
    /// ONLY AT THE TOP CONTENT COUNT. A chunk about the owner that matched fewer of the
    /// question's words has answered less of it — "my flight to Paris" is not answered by
    /// `Jeremy's Flight` to somewhere else over the Paris booking — so the split can only
    /// break a tie on the question's words, never overturn them. With no owner in the plan
    /// nothing leads and the order is exactly what it was.
    static func ownerLed(_ bodies: [(hit: VaultSearchHit, text: String)], plan: LookupPlan)
        -> (lead: [(hit: VaultSearchHit, text: String)],
            rest: [(hit: VaultSearchHit, text: String)]) {
        guard !plan.ownerForms.isEmpty else { return ([], bodies) }
        let content = bodies.map { body in
            plan.score(inTokens: LookupPlan.tokenize(
                [body.hit.title, body.hit.heading, body.hit.path, body.text]
                    .joined(separator: " "))).content
        }
        let top = content.max() ?? 0
        var lead: [(hit: VaultSearchHit, text: String)] = []
        var rest: [(hit: VaultSearchHit, text: String)] = []
        for (index, body) in bodies.enumerated() {
            if content[index] == top, isAboutOwner(body.hit, plan: plan) {
                lead.append(body)
            } else {
                rest.append(body)
            }
        }
        return (lead, rest)
    }

    /// Everything outside `Inbox/`, in the order it arrived.
    public static func allowed(_ hits: [VaultSearchHit]) -> [VaultSearchHit] {
        hits.filter { !$0.path.hasPrefix(excludedPrefix) }
    }

    /// Whether a note is FINISHED WORK: anywhere under an `archive/` directory.
    ///
    /// DEMOTED, NEVER EXCLUDED. `Projects/drafts/archive/` and `Projects/Research/archive/`
    /// hold delivered drafts and closed research, and on 2026-09-26 two of them answered
    /// "what's my birthday?" with the start date of a trip — they were ranked exactly like
    /// a note in use. They are still the only source for plenty of questions ("what did
    /// that report conclude"), so they stay retrievable and simply queue behind every live
    /// note.
    ///
    /// The test is `VaultArchive.isArchived`, the one the Vault tab folds its Archived
    /// section by, so the two can never disagree about what is finished.
    public static func isArchived(_ path: String) -> Bool {
        VaultArchive.isArchived(path)
    }

    /// The lexical order — `byConcept`'s, which the caller has already applied — and the
    /// embedding order, fused.
    ///
    /// The embedding is asked only when it says it is available, and a pair it cannot
    /// place is left out of its ordering entirely rather than scored zero — zero is a
    /// real similarity and would rank such a chunk above genuinely dissimilar ones.
    static func fused(_ bodies: [(hit: VaultSearchHit, text: String)],
                      question: String,
                      embedding: any ChunkEmbedding) -> [(hit: VaultSearchHit, text: String)] {
        let keys = bodies.map { "\($0.hit.path)#\($0.hit.line)" }
        var orderings = [keys]

        if embedding.isAvailable {
            var scored: [(key: String, score: Double)] = []
            for (index, body) in bodies.enumerated() {
                if let score = embedding.similarity(question, body.text) {
                    scored.append((keys[index], score))
                }
            }
            if !scored.isEmpty {
                // Ties break on the bm25 position, so the fusion is deterministic for a
                // corpus where several chunks embed identically.
                let position = Dictionary(uniqueKeysWithValues: keys.enumerated().map { ($1, $0) })
                let order = scored.sorted {
                    $0.score == $1.score ? (position[$0.key] ?? 0) < (position[$1.key] ?? 0)
                                         : $0.score > $1.score
                }.map(\.key)
                orderings.append(order)
            }
        }

        let scores = RankFusion.fuse(orderings)
        let position = Dictionary(uniqueKeysWithValues: keys.enumerated().map { ($1, $0) })
        return bodies.enumerated().sorted { lhs, rhs in
            let a = scores[keys[lhs.offset]] ?? 0
            let b = scores[keys[rhs.offset]] ?? 0
            if a != b { return a > b }
            return (position[keys[lhs.offset]] ?? 0) < (position[keys[rhs.offset]] ?? 0)
        }.map(\.element)
    }

    /// One chunk's text, at most `limit` characters.
    ///
    /// Clipped on a whitespace boundary when there is one within the last fifth of the
    /// allowance, so a chunk does not end mid-word — a half word at the end of a note
    /// extract is the kind of thing a small model completes rather than ignores.
    public static func clip(_ text: String, to limit: Int) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count > limit else { return trimmed }
        let head = trimmed.prefix(limit)
        let floor = head.index(head.startIndex, offsetBy: (limit * 4) / 5)
        if let breakPoint = head.lastIndex(where: { $0.isWhitespace }), breakPoint > floor {
            return String(head[head.startIndex..<breakPoint])
        }
        return String(head)
    }
}

/// A TYPED QUESTION IS NOT A SEARCH QUERY, and this is the difference.
///
/// `VaultSearchQuery` requires every token: a search field's rule, and the right one,
/// because the person typing into it chose those words. A question chooses its words for
/// a human listener, and most of them are grammar — "when", "is", "the", "did", "we".
/// Requiring a note to contain "when" is requiring the wrong thing.
///
/// Pure, and deliberately dumb: no stemming, no synonyms, no weighting. The expander
/// tier above already does the clever half, and a clever tokenizer here would be a
/// second, invisible one that disagreed with it.
public enum LookupQuery {

    /// Words that carry no retrieval signal in a question.
    ///
    /// Question words and auxiliaries, not a general English stop list: "no" and "not"
    /// stay, because a note that says a thing did NOT happen is exactly the note wanted,
    /// and so do short nouns that a general list would eat.
    public static let stopWords: Set<String> = [
        "a", "about", "all", "am", "an", "and", "any", "are", "as", "at", "be", "been",
        "but", "by", "can", "could", "did", "do", "does", "for", "from", "get", "give",
        "had", "has", "have", "he", "her", "hers", "him", "his", "how", "i", "if", "in",
        "into", "is", "it", "its", "may", "me", "mine", "much", "must", "my", "of", "on",
        "or", "our", "ours", "out", "over", "please", "she", "should", "so", "some",
        "tell", "than", "that", "the", "their", "theirs", "them", "then", "there",
        "these", "they", "this", "those", "to", "up", "us", "was", "we", "were", "what",
        "when", "where", "which", "who", "whom", "whose", "why", "will", "with", "would",
        "you", "your", "yours",
    ]

    /// The words that mean "the person asking", which is the one thing a personal vault
    /// never writes down.
    ///
    /// Every one of them is also a stop word above, and that is exactly the incident: they
    /// were dropped and NOTHING was put in their place, so "what's my birthday" went to the
    /// index as `birthday` and retrieval had no idea whose birthday was wanted. The vault
    /// says the owner's NAME, so the name is what these have to be replaced by.
    ///
    /// The contracted forms are here spelled the way `VaultSearchQuery.tokens` produces
    /// them — apostrophe intact — and a typed curly apostrophe is folded to a straight one
    /// before the comparison, because a phone's keyboard produces `I’m` and not `I'm`.
    public static let firstPersonWords: Set<String> = [
        "i", "i'm", "i've", "i'd", "i'll", "me", "my", "mine", "myself",
    ]

    /// Whether the question is about the person asking it.
    ///
    /// Read from the RAW question rather than from `VaultSearchQuery.tokens`, because that
    /// tokenizer drops every token shorter than two characters — so a bare "I" ("when was
    /// I born") does not survive to be recognised.
    public static func isFirstPerson(_ question: String) -> Bool {
        question.split(whereSeparator: \.isWhitespace).contains {
            firstPersonWords.contains(bareWord($0))
        }
    }

    /// One typed word, as the stop list and the first-person list spell words: lower case,
    /// no surrounding punctuation, straight apostrophe.
    static func bareWord(_ word: some StringProtocol) -> String {
        word.replacingOccurrences(of: "\u{2019}", with: "'")
            .trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
            .lowercased()
    }

    /// Whether a token carries no retrieval signal.
    ///
    /// The stop list is a list of WORDS, and the tokenizer hands over contractions whole,
    /// so `what's` was not on it — and on 2026-09-26 that alone was enough to wreck a
    /// question: `what's` became a required keyword, no note contains it, the precise pass
    /// found nothing, and the answer came from the single-keyword fallback. A token whose
    /// stem before the apostrophe is a stop word is a stop word: `what's`, `when's`,
    /// `it's`, `we're`, `I've`. A possessive of a MEANINGFUL word is untouched, because its
    /// stem is not on the list — `Marta's` stays `Marta's`.
    static func isStopWord(_ token: String) -> Bool {
        let bare = bareWord(token)
        if stopWords.contains(bare) { return true }
        guard let apostrophe = bare.firstIndex(of: "'") else { return false }
        return stopWords.contains(String(bare[bare.startIndex..<apostrophe]))
    }

    /// The owner's name as searchable tokens, or empty when there is no usable name.
    ///
    /// Tokenized by the same rule as a query, so a name written `Jeremy` matches a note's
    /// `Jeremy's` — FTS5's own tokenizer splits the possessive, and the prefix term the
    /// search builds from `Jeremy` matches its first half.
    static func ownerTokens(_ ownerName: String?) -> [String] {
        guard let ownerName else { return [] }
        return VaultSearchQuery.tokens(ownerName).filter { !isStopWord($0) }
    }

    /// EVERY SPELLING OF THE OWNER'S NAME the setting holds, as one concept.
    ///
    /// The setting is one name, or several separated by COMMAS, because one person has
    /// several names in his own notes and a prefix term matches none of the others: the
    /// owner is `Jeremy` in prose, `Jeremiah` on anything legal (in capitals, on a
    /// boarding pass), `Jeremia` to Italians, and `Andrews` in a list of surnames.
    /// `"Jeremy"*` matches exactly one of those four.
    ///
    /// A form of several words stays whole — `Jeremiah Kirsten Andrews` is one spelling,
    /// not three — and becomes an FTS5 phrase. Blank entries and a trailing comma are
    /// dropped, so a setting that has only ever held one name behaves exactly as it did.
    public static func ownerForms(_ ownerName: String?) -> [String] {
        guard let ownerName else { return [] }
        return ownerName.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .map { VaultSearchQuery.tokens($0).joined(separator: " ") }
            .filter { !$0.isEmpty }
    }

    /// The words worth searching for, in the order they were typed, with the owner's name
    /// appended when the question is about him.
    ///
    /// Falls back to the question's own tokens when every word is a stop word ("what is
    /// it"), because an empty query retrieves nothing and "nothing" is a worse answer
    /// than a bad one here — the abstain downstream is what catches a bad one.
    ///
    /// With no owner name, or an empty one, this is exactly what it always was.
    public static func keywords(_ question: String, ownerName: String? = nil) -> [String] {
        let tokens = VaultSearchQuery.tokens(question)
        let kept = tokens.filter { !isStopWord($0) }
        let base = kept.isEmpty ? tokens : kept
        guard isFirstPerson(question) else { return base }
        let present = Set(base.map { bareWord($0) })
        let owner = ownerTokens(ownerName).filter { !present.contains(bareWord($0)) }
        return base + owner
    }
}
