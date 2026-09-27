import Foundation

// WHAT A SPOKEN QUESTION ASKS FOR, AS CONCEPTS RATHER THAN AS WORDS.
//
// Two questions asked on 2026-09-27 with the bridge unreachable — "When is my flight
// today?" and "When is the KLM flight to Amsterdam today?" — were both answered "Not
// found in the vault on this device", over a vault holding the answer in three separate
// live notes. Every cause was the same shape: A WORD OF THE QUESTION WAS TREATED AS A
// REQUIREMENT ON THE NOTE.
//
//   * `VaultSearchQuery.matchExpression` joins every token with `AND`. That is the
//     search FIELD's rule, where the person chose the words. A question chooses its
//     words for a listener, and "today" is a word no note uses for a date.
//   * The owner's name was appended to a first-person question and then required too.
//     A booking, a calendar line or a Today item rarely names whose it is, and the
//     booking that DID name him spelled it `JEREMIAH KIRSTEN ANDREWS`, which the prefix
//     term `"Jeremy"*` does not match.
//   * "today" itself was never resolved to anything. The notes say `Sun 27 Sep`.
//
// So a question is planned, not tokenized. The plan holds CONCEPT GROUPS: the content
// words, one group each; every spelling of the owner's name as ONE group; and the
// absolute forms of whatever day a relative word named, as one more. Nothing is
// required. One FTS5 query ORs every term of every group, and a chunk is ranked by HOW
// MANY GROUPS it matches, bm25 breaking the ties.
//
// WHY GROUPS RATHER THAN MORE TERMS. Four spellings of a name are four chances to match
// one concept, not four concepts: a note that says `Jeremy` twelve times has not
// answered more of the question than one that says `JEREMIAH` once. Counting groups is
// what keeps a widened query from turning into a popularity contest between synonyms —
// which is exactly what the retriever's old per-keyword pass did, and why its union of
// twenty-hit lists per word buried the answer.
//
// WHY NOT A QUERY PER FORM. That is the same pass, again: each term's own top twenty,
// fused into a list where the chunk that matched three concepts and the chunk that
// matched one arrive side by side. One query, ranked by groups, cannot do that.

/// The device's idea of NOW: the clock, and the calendar and locale its dates are read
/// and spelled in.
///
/// One value because the two halves of "today" must agree. The retriever turns today
/// into the terms a note might spell it with, and the prompt tells the model what day it
/// is; if those came from two clocks, a question asked at midnight could retrieve one
/// day's notes and be told it is another day.
///
/// `.device` everywhere in the app, and every test pins it: nothing in this package may
/// depend on the real date.
public struct VaultClock: Sendable {
    /// The wall clock.
    public let now: @Sendable () -> Date
    /// The calendar the day is read in — and, through `calendar.timeZone`, the zone.
    public let calendar: Calendar
    /// The locale a date is spelled in.
    public let locale: Locale

    public init(now: @escaping @Sendable () -> Date = { Date() },
                calendar: Calendar = .autoupdatingCurrent,
                locale: Locale = .autoupdatingCurrent) {
        self.now = now
        self.calendar = calendar
        self.locale = locale
    }

    /// This device, right now.
    public static let device = VaultClock()

    /// A clock stopped at one instant. The only kind a test uses.
    public static func fixed(_ date: Date,
                             calendar: Calendar = .autoupdatingCurrent,
                             locale: Locale = .autoupdatingCurrent) -> VaultClock {
        VaultClock(now: { date }, calendar: calendar, locale: locale)
    }

    /// The day the question is being asked on.
    public var today: Date { now() }

    /// `Sunday, 27 September 2026` — the one date a prompt states, in this device's
    /// locale and zone.
    public var todaySentence: String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = "EEEE, d MMMM yyyy"
        return formatter.string(from: today)
    }
}

/// A RELATIVE DAY WORD, and the day it means.
///
/// The words a person says instead of a date. None of them appears in a note as a date,
/// so every one of them is dropped from the lexical query and replaced — where it names
/// a single day — by the absolute forms the notes actually use.
public struct RelativeDayWord: Equatable, Sendable {
    /// The phrase, lower case, as whole words.
    public let phrase: String
    /// Days from today, or nil for a phrase that names a SPAN rather than a day.
    ///
    /// "this week" is seven dates, and seven days of date forms is forty-odd terms —
    /// the flood this whole design exists to avoid. Such a phrase still comes OUT of the
    /// lexical query (no note says "this week" either) and simply contributes no date
    /// group.
    public let dayOffset: Int?

    public init(phrase: String, dayOffset: Int?) {
        self.phrase = phrase
        self.dayOffset = dayOffset
    }

    /// The phrase as the words a question is tokenized into.
    var words: [String] { phrase.split(separator: " ").map(String.init) }
}

/// One question, planned: which concepts it asks about, and the terms that stand for
/// each.
public struct LookupPlan: Equatable, Sendable {
    /// The question's own words worth searching for, in the order they were typed, with
    /// stop words and relative day words removed.
    public let contentWords: [String]
    /// Every spelling of the owner's name, or empty when the question is not about him
    /// or the device has no name for him.
    public let ownerForms: [String]
    /// The absolute forms of the day a relative word named, or empty.
    public let dateForms: [String]

    public init(contentWords: [String], ownerForms: [String], dateForms: [String]) {
        self.contentWords = contentWords
        self.ownerForms = ownerForms
        self.dateForms = dateForms
    }

    /// The concept groups: one per content word, then the owner, then the date. Empty
    /// groups are not groups.
    public var groups: [[String]] {
        var out = contentWords.map { [$0] }
        if !ownerForms.isEmpty { out.append(ownerForms) }
        if !dateForms.isEmpty { out.append(dateForms) }
        return out
    }

    /// Every term of every group, in group order.
    public var terms: [String] { groups.flatMap { $0 } }

    /// The ONE FTS5 expression this plan searches with: every term, ORed.
    public var expression: String? { VaultSearchQuery.anyMatchExpression(terms) }

    /// How many of the groups `text` matches. The rank.
    ///
    /// A group matches when ANY of its terms does, and counts once however many of them
    /// do — which is the whole point of a group.
    public func groupMatchCount(in text: String) -> Int {
        groupMatchCount(inTokens: LookupPlan.tokenize(text))
    }

    /// The same, over text already tokenized — what the retriever uses, because a chunk
    /// is scored against every group and tokenizing it once per group would be the same
    /// work several times over.
    public func groupMatchCount(inTokens tokens: [String]) -> Int {
        score(inTokens: tokens).total
    }

    /// THE TWO NUMBERS A CHUNK IS RANKED BY: how many of the question's own words it
    /// matched, and how many groups in all.
    ///
    /// THE QUESTION'S WORDS COME FIRST, and the owner and the date are MODIFIERS on top
    /// of them. "how do I get to Perugia tomorrow" is a first-person question, so the
    /// owner group is in the plan, and both itineraries in the fixture name him in
    /// capitals — on a single count they tie with the Perugia note, which matched the
    /// only word the question was actually about, and outrank it on bm25. A chunk that
    /// matches NO word of the question has not answered it, whoever it names.
    public func score(inTokens tokens: [String]) -> (content: Int, total: Int) {
        func matched(_ group: [String]) -> Bool {
            group.contains { LookupPlan.matches(term: $0, inTokens: tokens) }
        }
        let content = contentWords.filter { matched([$0]) }.count
        var total = content
        if !ownerForms.isEmpty, matched(ownerForms) { total += 1 }
        if !dateForms.isEmpty, matched(dateForms) { total += 1 }
        return (content, total)
    }

    // MARK: - Planning

    /// Plan `question` for `ownerName` as of `clock`.
    public static func make(question: String, ownerName: String?,
                            clock: VaultClock = .device) -> LookupPlan {
        let tokens = VaultSearchQuery.tokens(question)
        let relatives = relativeWords(in: tokens)
        let covered = coveredIndices(in: tokens, by: relatives)
        let kept = tokens.enumerated()
            .filter { !covered.contains($0.offset) && !LookupQuery.isStopWord($0.element) }
            .map(\.element)
        // A question made of nothing but grammar and a relative day ("what's on today")
        // would otherwise search for nothing at all, and nothing retrieves nothing. The
        // same fallback `LookupQuery.keywords` has always made, one step wider: the
        // relative words come back rather than the stop words, because they are the half
        // that carries any signal.
        let significant = tokens.filter { !LookupQuery.isStopWord($0) }
        let contentWords = !kept.isEmpty ? kept : (significant.isEmpty ? tokens : significant)

        let present = Set(contentWords.map { LookupQuery.bareWord($0) })
        let owner = LookupQuery.isFirstPerson(question)
            ? LookupQuery.ownerForms(ownerName).filter { form in
                !form.split(separator: " ").allSatisfy { present.contains(LookupQuery.bareWord($0)) }
            }
            : []

        let days = Set(relatives.compactMap(\.dayOffset))
        let dates = days.sorted().flatMap { offset -> [String] in
            guard let day = clock.calendar.date(byAdding: .day, value: offset,
                                                to: clock.today) else { return [] }
            return dateForms(for: day, clock: clock)
        }
        return LookupPlan(contentWords: contentWords, ownerForms: owner,
                          dateForms: unique(dates))
    }

    /// The relative day words this question uses, longest phrase first so "this morning"
    /// is one phrase rather than a stray "morning".
    public static let relativeDayWords: [RelativeDayWord] = [
        RelativeDayWord(phrase: "this morning", dayOffset: 0),
        RelativeDayWord(phrase: "this afternoon", dayOffset: 0),
        RelativeDayWord(phrase: "this evening", dayOffset: 0),
        RelativeDayWord(phrase: "this week", dayOffset: nil),
        RelativeDayWord(phrase: "next week", dayOffset: nil),
        RelativeDayWord(phrase: "tomorrow", dayOffset: 1),
        RelativeDayWord(phrase: "yesterday", dayOffset: -1),
        RelativeDayWord(phrase: "tonight", dayOffset: 0),
        RelativeDayWord(phrase: "today", dayOffset: 0),
    ]

    /// THE DATE FORMATS A NOTE MIGHT SPELL A DAY WITH, as one list.
    ///
    /// The ISO form a frontmatter field uses, the four ways prose writes a day and a
    /// month, and each of those four again behind the weekday abbreviation an itinerary
    /// table uses (`Sun 27 Sep`). One list rather than a set of ad-hoc strings, so
    /// "which forms does this understand" has a single answer a reader can check.
    ///
    /// Each becomes an FTS5 PHRASE with a prefix on its last token, so `27 Sep` also
    /// matches `27 September` and nothing has to list both.
    public static let dateFormats = [
        "yyyy-MM-dd",
        "d MMM", "MMM d", "d MMMM", "MMMM d",
        "EEE d MMM", "EEE MMM d", "EEE d MMMM", "EEE MMMM d",
    ]

    /// One day, in every form a note might spell it with.
    ///
    /// In the device's locale AND in English. The notes in this vault are written in
    /// English on a device that may well be set to another language, and a phone in
    /// Italian looking for `27 set` would find none of them; the two lists are identical
    /// on an English device and deduplicated when they are not.
    public static func dateForms(for day: Date, clock: VaultClock) -> [String] {
        let locales = [clock.locale, Locale(identifier: "en_US_POSIX")]
        var out: [String] = []
        for locale in locales {
            let formatter = DateFormatter()
            formatter.locale = locale
            formatter.calendar = clock.calendar
            formatter.timeZone = clock.calendar.timeZone
            for format in dateFormats {
                formatter.dateFormat = format
                out.append(formatter.string(from: day))
            }
        }
        return unique(out)
    }

    // MARK: - Pure halves, asserted directly

    /// The relative day words present in a question's tokens.
    static func relativeWords(in tokens: [String]) -> [RelativeDayWord] {
        let bare = tokens.map { LookupQuery.bareWord($0) }
        var out: [RelativeDayWord] = []
        for word in relativeDayWords where contains(bare, word.words) {
            out.append(word)
        }
        return out
    }

    /// The token positions a matched phrase occupies, so its words leave the query and
    /// nothing else does.
    ///
    /// A longer phrase wins: "this morning" covers both its words, and the bare
    /// "morning" phrase is not in the list at all, so a question about the morning
    /// routine keeps its word.
    static func coveredIndices(in tokens: [String], by words: [RelativeDayWord]) -> Set<Int> {
        let bare = tokens.map { LookupQuery.bareWord($0) }
        var out = Set<Int>()
        for word in words {
            let phrase = word.words
            guard !phrase.isEmpty, bare.count >= phrase.count else { continue }
            for start in 0...(bare.count - phrase.count)
            where Array(bare[start..<(start + phrase.count)]) == phrase {
                for offset in 0..<phrase.count { out.insert(start + offset) }
            }
        }
        return out
    }

    /// Whether `phrase` occurs in `words` as consecutive whole words.
    static func contains(_ words: [String], _ phrase: [String]) -> Bool {
        guard !phrase.isEmpty, words.count >= phrase.count else { return false }
        for start in 0...(words.count - phrase.count)
        where Array(words[start..<(start + phrase.count)]) == phrase {
            return true
        }
        return false
    }

    /// Text as the index tokenizes it: runs of letters and digits, folded for case and
    /// diacritics.
    ///
    /// The same rule as the index's own `unicode61 remove_diacritics 2`, restated here
    /// because ranking by concept needs to know WHICH terms a chunk matched and FTS5
    /// answers only whether a row matched at all. Two tokenizers that disagree would
    /// rank a chunk the query did not match, so this one is deliberately the simplest
    /// possible reading of that rule and nothing more.
    public static func tokenize(_ text: String) -> [String] {
        text.folding(options: [.diacriticInsensitive, .caseInsensitive], locale: nil)
            .lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .map(String.init)
    }

    /// Whether one term matches, with FTS5's own semantics: the term's words in order,
    /// its last word matching by PREFIX.
    ///
    /// `flight` matches `flights`, `27 Sep` matches `27 September`, and `Sun 27 Sep`
    /// matches the itinerary row that opens `| Out, Sun 27 Sep |`.
    public static func matches(term: String, inTokens tokens: [String]) -> Bool {
        let wanted = tokenize(term)
        guard let last = wanted.last, tokens.count >= wanted.count else { return false }
        let head = wanted.dropLast()
        for start in 0...(tokens.count - wanted.count) {
            guard Array(tokens[start..<(start + head.count)]) == Array(head) else { continue }
            if tokens[start + head.count].hasPrefix(last) { return true }
        }
        return false
    }

    /// The same strings, first occurrence kept, compared without case.
    static func unique(_ values: [String]) -> [String] {
        var seen = Set<String>()
        return values.filter { seen.insert($0.lowercased()).inserted }
    }
}
