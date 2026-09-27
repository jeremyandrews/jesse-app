import Foundation

// THE ANSWER FIRST.
//
// The owner reads most replies on a phone, fast, and two things used to sit between him and
// the answer: a prompt he never typed (a scheduled run's instructions, the morning routine,
// an automatic health turn) rendered as a full bubble, and the model's working narration
// ("Checking the footer format first") rendered as if it were part of the reply.
//
// Both are now folded away, and both are decided HERE, once, so the phone and the Mac cannot
// answer "is this folded?" two ways. Neither decision ever reads the wording: a prompt is
// untyped because of where it came from, and narration is narration because the harness said
// the model stopped to call a tool after it.

/// The labels a folded prompt row shows, one per thing in the apps that composes a prompt on
/// the owner's behalf. The bridge's own senders ("Scheduled: …", "Strand tick") name
/// themselves on the wire; these are the app's.
nonisolated public enum PromptSender {
    public static let morningRoutine = "Morning routine"
    public static let healthNewDay = "Health: start new day"
    public static let healthWorkoutLog = "Health: workout log"
    /// A Today item's action (Discuss, Propagate, a wiki chip), composed from the item.
    public static let todayAction = "Today action"
    /// The Today tab's Process updates, composed from the checked items.
    public static let processUpdates = "Process updates"
    /// A request to review the annotation marks in a note, composed from the note's path.
    public static let annotationReview = "Annotation review"
    /// A thread the phone started by itself before this label existed.
    public static let automatic = "Automatic"
    /// An "Ask about this" sent with nothing typed and a context with no scope name.
    public static let askContext = "Ask about this"
}

extension HealthAutoTurn {
    /// The folded prompt row's label for this automatic health turn.
    public var sentFor: String {
        switch self {
        case .morningRefresh: return PromptSender.healthNewDay
        case .workoutLog: return PromptSender.healthWorkoutLog
        }
    }
}

nonisolated public enum PromptFold {
    /// The label of the folded prompt row for a USER turn the owner did not type, or nil when
    /// he typed it and it renders as an ordinary bubble.
    ///
    /// In order, first match wins:
    /// 1. The turn names its sender (`Turn.sentFor`, set where the prompt was composed).
    /// 2. The turn carried a screen's context and nothing was typed (`displayText` is the empty
    ///    string): the whole turn is the context. Labelled with the context's scope.
    /// 3. The conversation's OPENING turn, when the conversation says what sent it: the
    ///    bridge's `sent_for` (a schedule, a strand tick, another device's routine), else an
    ///    automatic thread from before labels existed.
    ///
    /// Everything else is typed. Pure, so the rule is tested without a store.
    public static func hint(turnSentFor: String?, displayText: String?, contextLabel: String?,
                            isOpeningTurn: Bool, threadSentFor: String?,
                            threadOrigin: ThreadOrigin) -> String? {
        if let label = nonBlank(turnSentFor) { return label }
        if let shown = displayText,
           shown.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return nonBlank(contextLabel) ?? PromptSender.askContext
        }
        guard isOpeningTurn else { return nil }
        if let label = nonBlank(threadSentFor) { return label }
        if threadOrigin == .automatic { return PromptSender.automatic }
        return nil
    }

    private static func nonBlank(_ s: String?) -> String? {
        guard let t = s?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty else {
            return nil
        }
        return t
    }
}

extension Turn {
    /// The folded prompt row's label when this user turn was not typed by the owner, or nil.
    /// Jesse turns are never prompts. See `PromptFold.hint`.
    public var promptHint: String? {
        guard isUser else { return nil }
        let opening = thread.map { t in
            t.orderedTurns.first(where: { $0.isUser })?.id == id
        } ?? false
        return promptHint(isOpeningTurn: opening)
    }

    /// The same, when the caller already knows whether this is the conversation's opening
    /// user turn (a transcript pass finds that once rather than once per turn).
    public func promptHint(isOpeningTurn: Bool) -> String? {
        guard isUser else { return nil }
        return PromptFold.hint(turnSentFor: sentFor, displayText: displayText,
                               contextLabel: contextLabel, isOpeningTurn: isOpeningTurn,
                               threadSentFor: thread?.sentFor,
                               threadOrigin: thread?.originValue ?? .phone)
    }
}

// MARK: - Replies and their narration

/// What one turn is, for grouping a transcript into what it renders as.
nonisolated public enum TranscriptTurnKind: Equatable, Sendable {
    case user
    /// A Jesse turn that is the answer (every Jesse turn not marked as narration).
    case answer
    /// A Jesse turn the model said on its way to a tool call.
    case narration
}

/// One rendered row of a transcript.
nonisolated public enum TranscriptRow: Equatable, Sendable {
    /// A user turn, at this index.
    case user(Int)
    /// A reply: the answer turn (nil when the narration has no answer after it yet, as in a
    /// turn still running when it was hydrated, or one that failed) and the narration turns
    /// before it, in order. The narration folds into one Thinking row above the answer.
    case reply(answer: Int?, narration: [Int])
}

nonisolated public enum ReplyFold {
    /// Group turns into rows: each run of narration attaches to the answer that follows it in
    /// the same exchange. A user turn, or the end, closes a run that no answer followed.
    public static func rows(_ kinds: [TranscriptTurnKind]) -> [TranscriptRow] {
        var rows: [TranscriptRow] = []
        var pending: [Int] = []
        for (i, kind) in kinds.enumerated() {
            switch kind {
            case .narration:
                pending.append(i)
            case .answer:
                rows.append(.reply(answer: i, narration: pending))
                pending = []
            case .user:
                if !pending.isEmpty { rows.append(.reply(answer: nil, narration: pending)) }
                pending = []
                rows.append(.user(i))
            }
        }
        if !pending.isEmpty { rows.append(.reply(answer: nil, narration: pending)) }
        return rows
    }

    /// The text the Thinking row expands to: the narration the answer arrived with, else the
    /// hydrated narration turns before it. Never both, so a reply delivered live and later
    /// hydrated shows its narration once. Nil when there is none: that reply renders exactly
    /// as an ordinary answer.
    public static func thinking(stored: String?, narrationTexts: [String]) -> String? {
        if let s = stored?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty {
            return s
        }
        let joined = narrationTexts
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: "\n\n")
        return joined.isEmpty ? nil : joined
    }
}

extension Turn {
    public var transcriptKind: TranscriptTurnKind {
        if isUser { return .user }
        return isNarration ? .narration : .answer
    }
}

/// One transcript row resolved to turns, what both thread views render.
nonisolated public enum TranscriptItem: Identifiable {
    /// A typed user turn: an ordinary bubble.
    case user(Turn)
    /// A user turn the owner did not type, folded under a Prompt row with this label.
    case prompt(Turn, hint: String)
    /// A reply: its answer (nil while none followed the narration) and the text its Thinking
    /// row folds away (nil when there is none).
    case reply(answer: Turn?, thinking: String?, id: UUID)

    public var id: UUID {
        switch self {
        case .user(let t), .prompt(let t, _): return t.id
        case .reply(_, _, let id): return id
        }
    }

    /// Resolve a thread's ordered turns into rendered rows. The ONE function both apps call.
    public static func items(_ turns: [Turn]) -> [TranscriptItem] {
        let openingUser = turns.first(where: { $0.isUser })?.id
        return ReplyFold.rows(turns.map(\.transcriptKind)).map { row in
            switch row {
            case .user(let i):
                let turn = turns[i]
                if let hint = turn.promptHint(isOpeningTurn: turn.id == openingUser) {
                    return .prompt(turn, hint: hint)
                }
                return .user(turn)
            case .reply(let a, let narration):
                let answer = a.map { turns[$0] }
                let thinking = ReplyFold.thinking(stored: answer?.thinkingText,
                                                  narrationTexts: narration.map { turns[$0].text })
                // Keyed on the answer when there is one, so a row keeps its identity (and its
                // expansion state) as narration turns hydrate in ahead of it.
                let id = answer?.id ?? turns[narration[0]].id
                return .reply(answer: answer, thinking: thinking, id: id)
            }
        }
    }
}

// MARK: - Whole-conversation text

extension JesseThread {
    /// The whole conversation as a role-labelled Markdown transcript, for Share and Copy.
    /// Every prompt, thinking and answer is in full and labelled, so nothing a fold hides is
    /// lost from what is shared. Raw text, so links and formatting survive, with a blank line
    /// between parts so it reads cleanly when pasted.
    public var sharedTranscript: String {
        TranscriptItem.items(orderedTurns).compactMap { item -> String? in
            switch item {
            case .user(let t):
                return "**You:** \(t.text)"
            case .prompt(let t, let hint):
                return "**Prompt (\(hint)):** \(t.text)"
            case .reply(let answer, let thinking, _):
                var parts: [String] = []
                if let thinking { parts.append("**Jesse (thinking):** \(thinking)") }
                if let answer { parts.append("**Jesse:** \(answer.text)") }
                return parts.isEmpty ? nil : parts.joined(separator: "\n\n")
            }
        }.joined(separator: "\n\n")
    }

    /// The most recent ANSWER in this conversation, never a prompt or narration: what a list
    /// preview or a notification may show.
    public var lastAnswerText: String? {
        orderedTurns.last(where: { !$0.isUser && !$0.isNarration })?.text
    }
}

// MARK: - What the two fold rows say

/// The words and symbols of the two fold rows, shared so the phone and the Mac label them,
/// and announce them to VoiceOver, identically. The layouts are each platform's own.
nonisolated public enum FoldCopy {
    /// The Prompt row: a document, because what it hides is instructions someone wrote.
    public static let promptSymbol = "doc.text"
    public static let promptTitle = "Prompt"
    /// The Thinking row: a different symbol on purpose, so the two never read as one kind.
    public static let thinkingSymbol = "brain"
    public static let thinkingTitle = "Thinking"
    /// While the turn is still running and narrating.
    public static let thinkingLiveTitle = "Thinking…"

    public static func promptAccessibilityLabel(hint: String) -> String {
        "Prompt, sent by \(hint)"
    }
    public static let thinkingAccessibilityLabel = "Jesse's thinking"
    public static func accessibilityValue(expanded: Bool) -> String {
        expanded ? "Expanded" : "Collapsed"
    }
    public static func promptAccessibilityHint(expanded: Bool) -> String {
        expanded ? "Hides the prompt." : "Shows the full prompt."
    }
    public static func thinkingAccessibilityHint(expanded: Bool) -> String {
        expanded ? "Hides what Jesse said while working." : "Shows what Jesse said while working."
    }
    /// The chevron: pointing at where the content will be.
    public static func chevron(expanded: Bool) -> String {
        expanded ? "chevron.down" : "chevron.right"
    }
}

extension Turn {
    /// Every text this turn holds, for search: the whole `text` (a folded prompt's full body
    /// included) and a reply's stored narration. Folding hides text from the screen, never
    /// from search.
    public var searchableTexts: [String] {
        if let thinking = thinkingText, !thinking.isEmpty { return [text, thinking] }
        return [text]
    }
}
