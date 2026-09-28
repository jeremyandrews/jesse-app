import Foundation

// THE ANSWER, AND THE THREE WAYS IT IS STOPPED FROM BEING A LIE.
//
// A 3B model reading four note extracts will usually find the date that is in them. It
// will also, given a question the extracts do not answer, produce a confident sentence
// and a plausible file name to hang it on. Nothing about the shape of that reply
// distinguishes it from a correct one, so three mechanical checks stand between the
// model and the transcript, none of which needs the model's cooperation:
//
//   1. THE INSTRUCTIONS SAY ABSTAIN IS AN OPTION, and the generated struct has a field
//      for it, so "I don't know" is a value the model can return rather than a sentence
//      it has to compose against its own training.
//   2. EVERY CITATION IS CHECKED against the paths that were actually supplied. A path
//      the model invented is dropped here, not surfaced.
//   3. AN ANSWER WITH NO SURVIVING CITATION BECOMES AN ABSTAIN. This is the one that
//      matters. Step 2 alone would leave a confident answer with its evidence quietly
//      deleted, which reads exactly like a well-sourced one; turning it into a visible
//      "not found" is the difference between a bug and a lie.
//
// Plus a wall clock. Twenty seconds is not a performance target, it is the point past
// which a person has already decided the app is broken — and an abstain that arrives is
// worth more than an answer that does not.
//
// This file imports no model framework. `VaultAnswerGenerating` is the whole dependency
// surface.

/// What the model produced, before anything has been checked.
public struct VaultAnswerDraft: Equatable, Sendable {
    public let answer: String
    public let citations: [String]
    public let abstain: Bool

    public init(answer: String, citations: [String], abstain: Bool) {
        self.answer = answer
        self.citations = citations
        self.abstain = abstain
    }
}

/// One note the answer actually came from, with the line to open it at.
public struct VaultCitation: Equatable, Sendable, Hashable {
    public let path: String
    public let line: Int

    public init(path: String, line: Int) {
        self.path = path
        self.line = line
    }

    public var reference: String { "\(path):\(line)" }
}

/// A checked answer: text that survived validation, and at least one real citation.
public struct VaultAnswer: Equatable, Sendable {
    public let text: String
    /// Never empty. An answer with no citation is not a `VaultAnswer` at all.
    public let citations: [VaultCitation]

    public init(text: String, citations: [VaultCitation]) {
        self.text = text
        self.citations = citations
    }
}

/// Why there is no answer. Typed, because the composer renders a different line for
/// each and because a diagnostics row that said only "failed" would be useless.
public enum VaultAnswerFailure: Equatable, Sendable {
    /// No usable on-device model. The composer behaves exactly as it did before this
    /// path existed.
    case modelUnavailable
    /// The gate's rules said this is not a lookup, and which rule said so. The reason
    /// is CARRIED rather than looked up again by the caller, because the composer that
    /// renders "Not tried on the device: …" must say the same thing the diagnostics row
    /// says, and two readers deriving a reason from a question twice is how they drift.
    case gateRefused(LookupGate.Refusal)
    /// The index found nothing outside `Inbox/`.
    case noHits
    /// Twenty seconds went by.
    case timedOut
    /// The model said the notes do not hold the answer, OR its answer failed
    /// validation, which is the same fact from the reader's side: this device does not
    /// have it.
    case abstained
    /// The generation itself failed for a reason that is none of the above. Carried
    /// rather than folded into `abstained` because "the model refused" and "the model
    /// errored" are different things to see in a diagnostics list.
    case failed(String)

    /// The word the diagnostics list shows.
    public var label: String {
        switch self {
        case .modelUnavailable: return "no model"
        case .gateRefused: return "not a lookup"
        case .noHits: return "no hits"
        case .timedOut: return "timed out"
        case .abstained: return "abstained"
        case .failed(let why): return "failed: \(why)"
        }
    }
}

/// An answered question, or the reason it was not.
public enum VaultAnswerOutcome: Equatable, Sendable {
    case answered(VaultAnswer)
    case unanswered(VaultAnswerFailure)

    public var answer: VaultAnswer? {
        if case .answered(let value) = self { return value }
        return nil
    }

    /// The word the diagnostics list shows.
    public var label: String {
        switch self {
        case .answered(let value): return "answered (\(value.citations.count) cited)"
        case .unanswered(let failure): return failure.label
        }
    }
}

/// What the generation seam can go wrong with, as the retry needs to tell them apart.
public enum VaultAnswerGenerationError: Error, Equatable, Sendable {
    /// The prompt did not fit. The one error worth reacting to rather than reporting.
    case contextWindow
    case failed(String)
}

/// Answering one question from some chunks, as a seam.
public protocol VaultAnswerGenerating: Sendable {
    /// Whether this device has a usable model right now.
    var isAvailable: Bool { get }
    /// Throws `VaultAnswerGenerationError.contextWindow` when the prompt was refused
    /// for size, and `.failed` for anything else.
    func generate(question: String, chunks: [RetrievedChunk]) async throws -> VaultAnswerDraft

    /// The same, told what day it is.
    ///
    /// THE CLOCK IS PASSED RATHER THAN READ, for the reason `VaultClock` gives: the
    /// prompt states the date, the retrieval that chose these chunks resolved "today"
    /// against a date, and a turn in which those two disagreed would answer one day's
    /// question from another day's notes.
    ///
    /// Defaulted, and the default ignores it, because only ONE conformance builds a
    /// prompt — the rest are stubs that answer out of the chunks they are handed, and a
    /// stub should not have to mention a date it has no use for.
    func generate(question: String, chunks: [RetrievedChunk],
                  clock: VaultClock) async throws -> VaultAnswerDraft

    /// The same, told WHO IS ASKING: the app's owner-name setting, so the prompt can say
    /// whose "my" a first-person question means. Defaulted to the clock form for the same
    /// reason that one is: only the conformance that builds a prompt has a use for it.
    func generate(question: String, chunks: [RetrievedChunk],
                  clock: VaultClock, ownerName: String?) async throws -> VaultAnswerDraft
}

public extension VaultAnswerGenerating {
    func generate(question: String, chunks: [RetrievedChunk],
                  clock: VaultClock) async throws -> VaultAnswerDraft {
        try await generate(question: question, chunks: chunks)
    }

    func generate(question: String, chunks: [RetrievedChunk],
                  clock: VaultClock, ownerName: String?) async throws -> VaultAnswerDraft {
        try await generate(question: question, chunks: chunks, clock: clock)
    }
}

/// No model on this device.
public struct NoVaultAnswerGeneration: VaultAnswerGenerating {
    public init() {}
    public var isAvailable: Bool { false }
    public func generate(question: String, chunks: [RetrievedChunk]) async throws
        -> VaultAnswerDraft {
        throw VaultAnswerGenerationError.failed("no model")
    }
}

/// The orchestration: availability, the clock, the one retry, and the validation.
/// Never throws.
public struct VaultAnswerer: Sendable {

    /// The wall clock, past which the answer is not worth waiting for.
    public static let defaultTimeLimit: TimeInterval = 20

    /// The instructions the session is created with. At most sixty words, because this
    /// model does not hold long instructions — and frozen here, beside the gate's
    /// prompt, rather than in the file that owns the framework.
    public static let instructions = """
        Answer only from the notes given. If they do not contain the answer, set \
        abstain. Cite only the paths given. Today, tomorrow and yesterday mean the date \
        in the first line. My and I mean the asker; a fact about a different named \
        person is not an answer. At most 60 words.
        """

    /// How many citations the model may return.
    public static let maxCitations = 4

    private let generator: any VaultAnswerGenerating
    private let timeLimit: TimeInterval
    private let clock: VaultClock
    /// The app's owner-name setting, as `VaultRetriever` takes it. Nil on a device with no
    /// name for him, where a first-person answer is checked exactly as it always was.
    private let ownerName: String?

    public init(generator: any VaultAnswerGenerating,
                timeLimit: TimeInterval = VaultAnswerer.defaultTimeLimit,
                clock: VaultClock = .device,
                ownerName: String? = nil) {
        self.generator = generator
        self.timeLimit = timeLimit
        self.clock = clock
        self.ownerName = ownerName
    }

    /// Answer `question` from `chunks`, or say why not.
    public func answer(question: String, chunks: [RetrievedChunk]) async -> VaultAnswerOutcome {
        guard generator.isAvailable else { return .unanswered(.modelUnavailable) }
        guard !chunks.isEmpty else { return .unanswered(.noHits) }

        let generator = self.generator
        let clock = self.clock
        let ownerName = self.ownerName
        do {
            let draft = try await Self.withTimeLimit(timeLimit) {
                do {
                    return try await generator.generate(question: question, chunks: chunks,
                                                        clock: clock, ownerName: ownerName)
                } catch VaultAnswerGenerationError.contextWindow {
                    // ONE retry, with half the chunks. Not a loop: if half of a measured
                    // budget still does not fit, the budget is wrong and the right
                    // outcome is an honest abstain rather than four more round trips.
                    let halved = Array(chunks.prefix(max(1, chunks.count / 2)))
                    return try await generator.generate(question: question, chunks: halved,
                                                        clock: clock, ownerName: ownerName)
                }
            }
            return Self.validate(draft, chunks: chunks, question: question,
                                 ownerName: ownerName)
        } catch is TimedOut {
            return .unanswered(.timedOut)
        } catch VaultAnswerGenerationError.contextWindow {
            return .unanswered(.failed("prompt too large even halved"))
        } catch VaultAnswerGenerationError.failed(let why) {
            return .unanswered(.failed(why))
        } catch {
            return .unanswered(.failed(error.localizedDescription))
        }
    }

    // MARK: - Pure halves, asserted directly

    /// The prompt: TODAY'S DATE, the question, then each chunk under its own
    /// `NOTE path:line` header, and nothing else. No preamble, no restating of the
    /// instructions, no invented framing — every character here is a character not
    /// spent on a note.
    ///
    /// THE DATE IS THE ONE LINE THAT IS NOT A NOTE, and it is here because a model that
    /// cannot place "today" can only abstain. On 2026-09-27 "When is my flight today?"
    /// had the itinerary in front of it and the itinerary says `Sun 27 Sep`; with no
    /// idea what day it was, abstaining was the correct behaviour under these
    /// instructions. Unconditional rather than only when the question says "today":
    /// thirty characters, and "is that this week?" is a question about the date that
    /// does not contain the word.
    ///
    /// THE ASKER IS THE OTHER LINE, and only for a first-person question with a name set.
    /// On 2026-09-27 "What is my birthday?" was answered "Jamie's birthday is on Tuesday,
    /// December 9." from an extract about Jamie: nothing told the model the asker was not
    /// Jamie. The setting's first spelling, because a sentence needs one name.
    public static func prompt(question: String, chunks: [RetrievedChunk],
                              clock: VaultClock = .device,
                              ownerName: String? = nil) -> String {
        var out = "Today is \(clock.todaySentence).\n"
        if LookupQuery.isFirstPerson(question),
           let asker = LookupQuery.ownerForms(ownerName).first {
            out += "The asker is \(asker).\n"
        }
        out += question.trimmingCharacters(in: .whitespacesAndNewlines)
        for chunk in chunks {
            out += "\n\nNOTE \(chunk.reference)\n\(chunk.text)"
        }
        return out
    }

    /// The three checks, over a draft and the chunks it was given.
    public static func validate(_ draft: VaultAnswerDraft,
                                chunks: [RetrievedChunk],
                                question: String = "",
                                ownerName: String? = nil) -> VaultAnswerOutcome {
        // The line to open each cited path at: the highest-ranked chunk from that file,
        // which is the first one in `chunks`.
        var lineFor: [String: Int] = [:]
        for chunk in chunks where lineFor[chunk.path] == nil {
            lineFor[chunk.path] = chunk.line
        }

        var citations: [VaultCitation] = []
        for raw in draft.citations {
            let path = normalizeCitation(raw)
            guard let line = lineFor[path] else { continue }
            guard !citations.contains(where: { $0.path == path }) else { continue }
            citations.append(VaultCitation(path: path, line: line))
            if citations.count == maxCitations { break }
        }

        let text = draft.answer.trimmingCharacters(in: .whitespacesAndNewlines)
        // Abstain wins over everything: a model that set the flag AND produced a
        // sentence is a model hedging, and the flag is the half that was asked for.
        guard !draft.abstain else { return .unanswered(.abstained) }
        guard !text.isEmpty else { return .unanswered(.abstained) }
        // THE CHECK THIS FILE EXISTS FOR. An answer whose every citation was invented
        // is not an answer with a formatting problem, it is an answer with no evidence,
        // and it becomes a visible "not found" rather than a plausible sentence.
        guard !citations.isEmpty else { return .unanswered(.abstained) }
        // …and the answer has to be ABOUT what it cites, and has to SAY something the
        // question did not already say.
        let cited = Set(citations.map(\.path))
        guard isGrounded(text, in: chunks.filter { cited.contains($0.path) },
                         question: question) else {
            return .unanswered(.abstained)
        }
        // …and a question about the asker is not answered by somebody else's fact.
        if LookupQuery.isFirstPerson(question),
           isAboutSomebodyElse(text, question: question, ownerName: ownerName) {
            return .unanswered(.abstained)
        }
        return .answered(VaultAnswer(text: text, citations: citations))
    }

    /// Whether `answer` carries at least one significant word that is in an extract it
    /// cites AND was not already in the question.
    ///
    /// "Significant" is `LookupQuery`'s rule — the question tokenizer's stop list, so the
    /// vault has exactly one idea of which words carry meaning — plus a three-character
    /// floor, because a two-letter coincidence is not evidence of anything. The
    /// comparison folds case and diacritics through `localizedStandardContains`, the same
    /// way the index's own tokenizer does.
    ///
    /// BOTH HALVES WERE MEASURED, on this corpus, with the real on-device model:
    ///
    ///   * Without the extract check, "what colour is the studio door" — over notes that
    ///     never mention a door — came back "white", citing the studio note it had been
    ///     handed. A REAL path, so the citation check passed it.
    ///   * Without the question check, "what is the name of the kiln repair company in
    ///     Florence" came back "Kiln repair company in Florence", citing the kiln note:
    ///     every word of it appears in the extract, so the extract check passed it. An
    ///     answer made only of the question's own words has answered nothing.
    ///
    /// An answer with no significant words at all ("yes", "it is") fails for the same
    /// reason: there is nothing in it to check, and a bare affirmation with a citation is
    /// precisely the shape that reads as sourced and is not.
    ///
    /// It is a floor, not a proof. A wrong answer assembled out of words that ARE in the
    /// extract still gets through, which is why the badge and the citations are on every
    /// one of these replies.
    public static func isGrounded(_ answer: String, in chunks: [RetrievedChunk],
                                  question: String = "") -> Bool {
        guard !chunks.isEmpty else { return false }
        let asked = Set(LookupQuery.keywords(question).map { $0.lowercased() })
        let words = LookupQuery.keywords(answer)
            .filter { $0.count >= 3 && !asked.contains($0.lowercased()) }
        guard !words.isEmpty else { return false }
        return words.contains { word in
            chunks.contains { $0.text.localizedStandardContains(word) }
        }
    }

    /// WHETHER AN ANSWER GIVES THE FACT FOR A NAMED PERSON WHO IS NOT THE OWNER.
    ///
    /// The 2026-09-27 reply, "Jamie's birthday is on Tuesday, December 9.", to "What is my
    /// birthday?": grounded (its words are in the extract), cited (a real path), and about
    /// the wrong person. Nothing else here looks at WHOSE fact an answer states.
    ///
    /// The rule the retriever ranks by (`LookupPlan.namesOwnerAsSubject`), turned around: a
    /// capitalised name in POSSESSIVE position (`Jamie's`) or opening a sentence (`Jamie
    /// was born`), followed within `LookupPlan.subjectWindow` tokens by a word of the
    /// question, states that person's fact. If the name is not a spelling of the owner's,
    /// the answer is someone else's. No list of names and no list of words: the names come
    /// from the setting, the words from the question.
    ///
    /// A stop word is never a name, so `Your birthday is…` and `The birthday…` pass, and so
    /// does an answer that names nobody. With no owner name there is nobody to compare
    /// against and this says false; the reply footer says the name is missing instead.
    public static func isAboutSomebodyElse(_ answer: String, question: String,
                                           ownerName: String?) -> Bool {
        let owner = Set(LookupQuery.ownerForms(ownerName).flatMap { LookupPlan.tokenize($0) })
        guard !owner.isEmpty else { return false }
        let asked = LookupPlan.make(question: question, ownerName: nil).contentWords
        guard !asked.isEmpty else { return false }
        let sentences = answer.replacingOccurrences(of: "\u{2019}", with: "'")
            .split(whereSeparator: { ".!?\n".contains($0) })
        for sentence in sentences {
            let words = sentence.split(whereSeparator: { !$0.isLetter && !$0.isNumber })
                .map(String.init)
            for (index, word) in words.enumerated() {
                guard word.first?.isUppercase == true, !LookupQuery.isStopWord(word),
                      let folded = LookupPlan.tokenize(word).first,
                      !owner.contains(folded) else { continue }
                // `Jeremy Andrews's birthday` on a one-name setting: a name straight after
                // one of his is the rest of HIS name, not somebody else's.
                if index > 0, let previous = LookupPlan.tokenize(words[index - 1]).first,
                   owner.contains(previous) { continue }
                let from = index + 1
                let possessive = from < words.count && words[from] == "s"
                guard possessive || index == 0 else { continue }
                let after = words[from..<min(words.count, from + LookupPlan.subjectWindow)]
                    .flatMap { LookupPlan.tokenize($0) }
                if asked.contains(where: { LookupPlan.matches(term: $0, inTokens: after) }) {
                    return true
                }
            }
        }
        return false
    }

    /// A cited path as the model is likely to have written it back.
    ///
    /// The prompt names each note `NOTE path:line`, and a model that copies the whole
    /// header, or wraps the path in quotes or brackets, has still cited a real file.
    /// Trimming those is not leniency about invented paths — the result is still
    /// matched exactly against the supplied set.
    static func normalizeCitation(_ raw: String) -> String {
        var value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.hasPrefix("NOTE ") { value = String(value.dropFirst(5)) }
        value = value.trimmingCharacters(in: CharacterSet(charactersIn: "\"'`[]()<> "))
        // `path.md:12` back to `path.md`.
        if let colon = value.lastIndex(of: ":"),
           value[value.index(after: colon)...].allSatisfy(\.isNumber),
           value.index(after: colon) < value.endIndex {
            value = String(value[value.startIndex..<colon])
        }
        return value
    }

    // MARK: - The clock

    /// Thrown by the racing task, and caught by exactly one place.
    struct TimedOut: Error {}

    /// Run `work`, or throw `TimedOut` when `seconds` go by first.
    ///
    /// A task group rather than a `Task` plus a cancel, because the group is what
    /// guarantees the loser is cancelled on every exit path including a throw — and a
    /// leaked on-device inference is minutes of the neural engine nobody is waiting for.
    static func withTimeLimit<T: Sendable>(
        _ seconds: TimeInterval,
        _ work: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await Task.sleep(for: .seconds(seconds))
                throw TimedOut()
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw TimedOut() }
            return first
        }
    }
}
