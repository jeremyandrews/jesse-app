import XCTest
import SwiftData
import JesseCore

/// The two folds: which user turns the owner did not type (the Prompt row), and how narration
/// groups with its answer (the Thinking row). Neither decision may read the wording, so every
/// case below differs only in where a turn came from, never in what it says.
@MainActor
final class PromptFoldTests: XCTestCase {

    private func makeContext() throws -> ModelContext {
        let container = try ModelContainer(
            for: JesseThread.self, Turn.self,
            configurations: ModelConfiguration(isStoredInMemoryOnly: true))
        return ModelContext(container)
    }

    /// A thread with these turns, in this order, a second apart.
    private func thread(_ context: ModelContext, _ turns: [Turn],
                        sentFor: String? = nil, origin: ThreadOrigin = .phone) -> JesseThread {
        let thread = JesseThread(mode: .tell)
        thread.sentFor = sentFor
        thread.origin = origin.rawValue
        context.insert(thread)
        let base = Date(timeIntervalSince1970: 2_000_000)
        for (i, t) in turns.enumerated() {
            t.createdAt = base.addingTimeInterval(Double(i))
            t.thread = thread
            context.insert(t)
        }
        return thread
    }

    private func narration(_ text: String) -> Turn {
        let t = Turn(role: .jesse, text: text)
        t.isNarration = true
        return t
    }

    // MARK: - Typed versus sent for him

    func testATypedTurnIsNeverFolded() {
        XCTAssertNil(PromptFold.hint(turnSentFor: nil, displayText: nil, contextLabel: nil,
                                     isOpeningTurn: true, threadSentFor: nil,
                                     threadOrigin: .phone))
        XCTAssertNil(PromptFold.hint(turnSentFor: nil, displayText: nil, contextLabel: nil,
                                     isOpeningTurn: true, threadSentFor: nil,
                                     threadOrigin: .watch))
    }

    func testATurnThatNamesItsSenderIsFoldedUnderThatName() {
        XCTAssertEqual(PromptFold.hint(turnSentFor: PromptSender.morningRoutine, displayText: nil,
                                       contextLabel: nil, isOpeningTurn: false,
                                       threadSentFor: nil, threadOrigin: .phone),
                       "Morning routine")
        XCTAssertNil(PromptFold.hint(turnSentFor: "   ", displayText: nil, contextLabel: nil,
                                     isOpeningTurn: false, threadSentFor: nil,
                                     threadOrigin: .phone),
                     "a blank label is no label")
    }

    /// An "Ask about this" sent with nothing typed: the whole turn is the screen's context.
    /// This is how turns stored before the field existed resolve too.
    func testAContextSentWithNothingTypedIsFoldedUnderItsScope() {
        XCTAssertEqual(PromptFold.hint(turnSentFor: nil, displayText: "", contextLabel: "Lunch · Aug 22",
                                       isOpeningTurn: true, threadSentFor: nil,
                                       threadOrigin: .phone),
                       "Lunch · Aug 22")
        XCTAssertEqual(PromptFold.hint(turnSentFor: nil, displayText: "", contextLabel: nil,
                                       isOpeningTurn: false, threadSentFor: nil,
                                       threadOrigin: .phone),
                       PromptSender.askContext)
        // Something typed alongside the context: typed, and renders as today.
        XCTAssertNil(PromptFold.hint(turnSentFor: nil, displayText: "is this too much?",
                                     contextLabel: "Lunch", isOpeningTurn: true,
                                     threadSentFor: nil, threadOrigin: .phone))
    }

    /// The bridge's `sent_for` names the conversation's OPENING turn only: a follow-up the
    /// owner types into a scheduled run's conversation is his.
    func testAScheduledConversationFoldsOnlyItsOpeningTurn() throws {
        let context = try makeContext()
        let prompt = Turn(role: .user, text: "Process the archive box …")
        let answer = Turn(role: .jesse, text: "Moved 3 notes.")
        let followUp = Turn(role: .user, text: "why was the invoice skipped?")
        _ = thread(context, [prompt, answer, followUp], sentFor: "Scheduled: archive box")
        XCTAssertEqual(prompt.promptHint, "Scheduled: archive box")
        XCTAssertNil(followUp.promptHint)
        XCTAssertNil(answer.promptHint, "a Jesse turn is never a prompt")
    }

    /// Turns stored before this change carry no label. An automatic thread's opening turn
    /// still reads as sent for him; a typed thread's does not.
    func testMigratedTurnsResolveFromTheirThread() throws {
        let context = try makeContext()
        let auto = Turn(role: .user, text: "Refresh the health dashboard …")
        _ = thread(context, [auto, Turn(role: .jesse, text: "Done.")], origin: .automatic)
        XCTAssertEqual(auto.promptHint, PromptSender.automatic)

        let typed = Turn(role: .user, text: "what is on today?")
        _ = thread(context, [typed, Turn(role: .jesse, text: "Three things.")])
        XCTAssertNil(typed.promptHint)
    }

    // MARK: - Narration and its answer

    func testNarrationGroupsWithTheAnswerAfterIt() {
        XCTAssertEqual(ReplyFold.rows([.user, .narration, .narration, .answer, .user, .answer]),
                       [.user(0), .reply(answer: 3, narration: [1, 2]), .user(4),
                        .reply(answer: 5, narration: [])])
    }

    /// Narration no answer followed (a turn still running when it hydrated, or one that
    /// failed) still folds; it is never shown as an answer.
    func testNarrationWithNoAnswerFoldsOnItsOwn() {
        XCTAssertEqual(ReplyFold.rows([.user, .narration, .user, .narration]),
                       [.user(0), .reply(answer: nil, narration: [1]), .user(2),
                        .reply(answer: nil, narration: [3])])
    }

    /// A reply delivered live stores its narration; hydrated later, its narration turns come
    /// in too. It shows once.
    func testStoredNarrationWinsOverHydratedTurns() {
        XCTAssertEqual(ReplyFold.thinking(stored: "Looking.", narrationTexts: ["Looking."]),
                       "Looking.")
        XCTAssertEqual(ReplyFold.thinking(stored: nil, narrationTexts: ["One.", " ", "Two."]),
                       "One.\n\nTwo.")
        XCTAssertNil(ReplyFold.thinking(stored: "  ", narrationTexts: []),
                     "a reply with no narration renders exactly as an answer")
    }

    func testItemsResolveAScheduledRunIntoPromptThinkingAndAnswer() throws {
        let context = try makeContext()
        let prompt = Turn(role: .user, text: "Process the archive box …")
        let answer = Turn(role: .jesse, text: "Moved 3 notes.")
        let t = thread(context, [prompt, narration("Starting now."), narration("Checking."), answer],
                       sentFor: "Scheduled: archive box")
        let items = TranscriptItem.items(t.orderedTurns)
        XCTAssertEqual(items.count, 2)
        guard case .prompt(let p, let hint) = items[0] else { return XCTFail("\(items[0])") }
        XCTAssertEqual(p.id, prompt.id)
        XCTAssertEqual(hint, "Scheduled: archive box")
        guard case .reply(let a, let thinking, let id) = items[1] else { return XCTFail("\(items[1])") }
        XCTAssertEqual(a?.id, answer.id)
        XCTAssertEqual(id, answer.id, "a reply row is keyed on its answer")
        XCTAssertEqual(thinking, "Starting now.\n\nChecking.")
    }

    func testAnOrdinaryConversationRendersExactlyAsBefore() throws {
        let context = try makeContext()
        let q = Turn(role: .user, text: "hi")
        let a = Turn(role: .jesse, text: "hello")
        let t = thread(context, [q, a])
        let items = TranscriptItem.items(t.orderedTurns)
        guard case .user(let u) = items[0], case .reply(let r, let thinking, _) = items[1] else {
            return XCTFail("\(items)")
        }
        XCTAssertEqual(u.id, q.id)
        XCTAssertEqual(r?.id, a.id)
        XCTAssertNil(thinking)
    }

    // MARK: - What does not change

    /// Share and Copy carry everything the folds hide, labelled.
    func testTheSharedTranscriptCarriesPromptThinkingAndAnswer() throws {
        let context = try makeContext()
        let answer = Turn(role: .jesse, text: "Moved 3 notes.")
        answer.thinkingText = "Checking the footer."
        let t = thread(context, [Turn(role: .user, text: "Process the archive box."), answer,
                                 Turn(role: .user, text: "thanks")],
                       sentFor: "Scheduled: archive box")
        XCTAssertEqual(t.sharedTranscript, """
            **Prompt (Scheduled: archive box):** Process the archive box.

            **Jesse (thinking):** Checking the footer.

            **Jesse:** Moved 3 notes.

            **You:** thanks
            """)
    }

    /// A list preview or notification shows the answer, never a prompt or narration.
    func testTheLastAnswerSkipsPromptsAndNarration() throws {
        let context = try makeContext()
        let t = thread(context, [Turn(role: .jesse, text: "Moved 3 notes."),
                                 Turn(role: .user, text: "Process again."),
                                 narration("Starting now.")])
        XCTAssertEqual(t.lastAnswerText, "Moved 3 notes.")
    }

    func testSearchTextsIncludeTheFoldedNarration() {
        let t = Turn(role: .jesse, text: "Moved 3 notes.")
        XCTAssertEqual(t.searchableTexts, ["Moved 3 notes."])
        t.thinkingText = "Checking the footer."
        XCTAssertEqual(t.searchableTexts, ["Moved 3 notes.", "Checking the footer."])
    }

    func testEachAutomaticHealthTurnNamesItself() {
        XCTAssertEqual(HealthAutoTurn.morningRefresh.sentFor, PromptSender.healthNewDay)
        XCTAssertEqual(HealthAutoTurn.workoutLog.sentFor, PromptSender.healthWorkoutLog)
    }
}
