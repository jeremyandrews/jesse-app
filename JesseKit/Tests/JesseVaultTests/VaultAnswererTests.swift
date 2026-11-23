import XCTest
@testable import JesseVault

// The three checks that stand between a 3B model and the transcript, plus the clock.
//
// Everything here runs against a scripted generator. The real model is never called from
// a test — it is unavailable in CI and on most simulators, and a test that only passes on
// one laptop is not a test.

/// A generator that answers from a script, counts its calls, and can be told to fail.
private final class ScriptedGenerator: VaultAnswerGenerating, @unchecked Sendable {
    enum Behaviour {
        case draft(VaultAnswerDraft)
        case throwing(VaultAnswerGenerationError)
        /// Refuse the first call for size, then answer.
        case contextWindowThenDraft(VaultAnswerDraft)
        /// Never return.
        case hang
    }

    let behaviour: Behaviour
    var available = true
    private let lock = NSLock()
    private(set) var calls: [[RetrievedChunk]] = []

    init(_ behaviour: Behaviour) { self.behaviour = behaviour }

    var isAvailable: Bool { available }

    func generate(question: String, chunks: [RetrievedChunk]) async throws -> VaultAnswerDraft {
        let index: Int = lock.withLock {
            calls.append(chunks)
            return calls.count - 1
        }
        switch behaviour {
        case .draft(let draft):
            return draft
        case .throwing(let error):
            throw error
        case .contextWindowThenDraft(let draft):
            if index == 0 { throw VaultAnswerGenerationError.contextWindow }
            return draft
        case .hang:
            try await Task.sleep(for: .seconds(60))
            throw VaultAnswerGenerationError.failed("never")
        }
    }
}

final class VaultAnswererTests: XCTestCase {

    private func chunk(_ path: String, line: Int = 1, text: String = "body") -> RetrievedChunk {
        RetrievedChunk(path: path, line: line, title: path, heading: "", text: text)
    }

    /// REAL-LOOKING extract text, not a placeholder, because validation now checks that
    /// the answer shares a word with what it cites — so a fixture of `"body"` would make
    /// every grounded answer fail for the wrong reason.
    private var chunks: [RetrievedChunk] {
        [chunk("Family/School-Year.md", line: 7,
               text: "The spring concert is on Thursday 14 May at 18:30 in the school hall."),
         chunk("Family/Birthdays.md", line: 3,
               text: "Marta's birthday is 3 February. Alberto's is 19 September."),
         chunk("House/Boiler.md", line: 2,
               text: "The boiler service is booked for 8 October."),
         chunk("Workshop/Glazes.md", line: 5,
               text: "Tenmoku is fired to cone ten in reduction.")]
    }

    // MARK: - Validation

    func testAValidAnswerKeepsItsCitationAndTheLineToOpenItAt() {
        let draft = VaultAnswerDraft(answer: "Thursday 14 May at 18:30.",
                                     citations: ["Family/School-Year.md"], abstain: false)
        guard case .answered(let answer) = VaultAnswerer.validate(draft, chunks: chunks) else {
            return XCTFail("expected an answer")
        }
        XCTAssertEqual(answer.text, "Thursday 14 May at 18:30.")
        XCTAssertEqual(answer.citations,
                       [VaultCitation(path: "Family/School-Year.md", line: 7)])
    }

    /// An invented path is DROPPED. Not surfaced, not corrected, not guessed at.
    func testACitationThatWasNeverSuppliedIsDropped() {
        let draft = VaultAnswerDraft(
            answer: "Thursday.",
            citations: ["Family/School-Year.md", "Family/Concerts-2026.md"],
            abstain: false)
        guard case .answered(let answer) = VaultAnswerer.validate(draft, chunks: chunks) else {
            return XCTFail("expected an answer")
        }
        XCTAssertEqual(answer.citations.map(\.path), ["Family/School-Year.md"])
    }

    /// THE CHECK THIS FILE EXISTS FOR. An answer whose every citation was invented has no
    /// evidence at all, and becomes a visible "not found" rather than a plausible lie.
    func testAnAnswerWithNoValidCitationBecomesAnAbstain() {
        let draft = VaultAnswerDraft(answer: "Thursday 14 May, definitely.",
                                     citations: ["Family/Concerts-2026.md"], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: chunks),
                       .unanswered(.abstained))
    }

    func testAnEmptyCitationListIsAnAbstain() {
        let draft = VaultAnswerDraft(answer: "Thursday.", citations: [], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: chunks),
                       .unanswered(.abstained))
    }

    func testTheAbstainFlagWinsOverAnyText() {
        let draft = VaultAnswerDraft(answer: "Possibly Thursday.",
                                     citations: ["Family/School-Year.md"], abstain: true)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: chunks),
                       .unanswered(.abstained))
    }

    func testAnEmptyAnswerIsAnAbstain() {
        let draft = VaultAnswerDraft(answer: "   \n ",
                                     citations: ["Family/School-Year.md"], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: chunks),
                       .unanswered(.abstained))
    }

    func testDuplicateCitationsCollapseAndFourIsTheCeiling() {
        let draft = VaultAnswerDraft(
            answer: "Concert Thursday, birthday February, boiler October, tenmoku cone ten.",
            citations: ["Family/School-Year.md", "Family/School-Year.md",
                        "Family/Birthdays.md", "House/Boiler.md", "Workshop/Glazes.md",
                        "Family/School-Year.md"],
            abstain: false)
        guard case .answered(let answer) = VaultAnswerer.validate(draft, chunks: chunks) else {
            return XCTFail("expected an answer")
        }
        XCTAssertEqual(answer.citations.count, 4)
        XCTAssertEqual(Set(answer.citations.map(\.path)).count, 4)
    }

    /// A model that copies the `NOTE path:line` header back has still cited a real file.
    /// Tidying that is not leniency: the result is still matched exactly.
    func testACitationCopiedBackWithItsHeaderStillMatches() {
        XCTAssertEqual(VaultAnswerer.normalizeCitation("NOTE Family/School-Year.md:7"),
                       "Family/School-Year.md")
        XCTAssertEqual(VaultAnswerer.normalizeCitation("  [Family/School-Year.md]  "),
                       "Family/School-Year.md")
        XCTAssertEqual(VaultAnswerer.normalizeCitation("\"Family/School-Year.md\""),
                       "Family/School-Year.md")
        // …and a colon that is part of a name is not a line number.
        XCTAssertEqual(VaultAnswerer.normalizeCitation("Notes/Re: bricks.md"),
                       "Notes/Re: bricks.md")
    }

    // MARK: - Grounding

    /// MEASURED, not anticipated. Asked "what colour is the studio door" over notes that
    /// never mention a door, the on-device model answered "white" and cited the studio
    /// note it had been handed — a REAL path, so the citation check passed it. This is
    /// the check that turns that into an abstain.
    func testAnAnswerSharingNoWordWithWhatItCitesIsAnAbstain() {
        let rent = chunk("Workshop/Studio-Rent.md", line: 1,
                         text: "The studio rent is 340 euro a month, paid on the first.")
        let draft = VaultAnswerDraft(answer: "white",
                                     citations: ["Workshop/Studio-Rent.md"], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: [rent]),
                       .unanswered(.abstained))
    }

    func testAnExtractiveAnswerIsGrounded() {
        let rent = chunk("Workshop/Studio-Rent.md", line: 1,
                         text: "The studio rent is 340 euro a month, paid on the first.")
        let draft = VaultAnswerDraft(answer: "340 euro a month.",
                                     citations: ["Workshop/Studio-Rent.md"], abstain: false)
        XCTAssertNotNil(VaultAnswerer.validate(draft, chunks: [rent]).answer)
    }

    /// A bare affirmation has nothing in it to check, and reads as sourced when it is not.
    func testAnAnswerWithNoSignificantWordsIsNotGrounded() {
        let rent = chunk("Workshop/Studio-Rent.md", line: 1,
                         text: "The studio rent is 340 euro a month.")
        XCTAssertFalse(VaultAnswerer.isGrounded("yes, it is", in: [rent]))
        XCTAssertFalse(VaultAnswerer.isGrounded("", in: [rent]))
        XCTAssertFalse(VaultAnswerer.isGrounded("340", in: []))
    }

    /// Grounding is checked against the CITED extracts only. A word that appears in some
    /// other retrieved note is not evidence for the note the answer names.
    func testGroundingIsCheckedAgainstTheCitedExtractOnly() {
        let rent = chunk("Workshop/Studio-Rent.md", line: 1,
                         text: "The studio rent is 340 euro a month.")
        let glazes = chunk("Workshop/Glazes.md", line: 3,
                           text: "Tenmoku is fired to cone ten in reduction.")
        let draft = VaultAnswerDraft(answer: "Tenmoku, cone ten.",
                                     citations: ["Workshop/Studio-Rent.md"], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(draft, chunks: [rent, glazes]),
                       .unanswered(.abstained))
    }

    /// The second measured failure: asked "what is the name of the kiln repair company in
    /// Florence" over a vault that has no such thing, the model answered "Kiln repair
    /// company in Florence" and cited the kiln note. Every word of it IS in the extract,
    /// so the extract check alone passed it — and it had answered nothing.
    func testAnAnswerMadeOnlyOfTheQuestionsOwnWordsIsAnAbstain() {
        let kiln = chunk("Workshop/Kiln-Rebuild.md", line: 5,
                         text: "The kiln repair company quoted for forty soft bricks.")
        let draft = VaultAnswerDraft(answer: "Kiln repair company in Florence",
                                     citations: ["Workshop/Kiln-Rebuild.md"], abstain: false)
        XCTAssertEqual(
            VaultAnswerer.validate(
                draft, chunks: [kiln],
                question: "what is the name of the kiln repair company in Florence"),
            .unanswered(.abstained))
        // …and the same answer to a question that did NOT already contain those words is
        // a perfectly good one.
        XCTAssertNotNil(
            VaultAnswerer.validate(draft, chunks: [kiln],
                                   question: "who did the work").answer)
    }

    /// Case and diacritics fold, the same way the index's own tokenizer folds them.
    func testGroundingFoldsCaseAndDiacritics() {
        let cafe = chunk("Workshop/Cafe.md", line: 1, text: "The café tiles came from Terrasole.")
        XCTAssertTrue(VaultAnswerer.isGrounded("The cafe tiles.", in: [cafe]))
        XCTAssertTrue(VaultAnswerer.isGrounded("TERRASOLE", in: [cafe]))
    }

    // MARK: - Numbers

    /// Synthetic stand-ins for the 2026-11-22 incident: a clam pasta recipe whose only
    /// "30g" is salt for soaking, and a hot sauce recipe that says "honey" but never "24g".
    private var pasta: RetrievedChunk {
        chunk("Recipes/Clam-Pasta.md", line: 4,
              text: "Soak the clams in water with about 30g of salt per litre for an hour.")
    }
    private var hotSauce: RetrievedChunk {
        chunk("Recipes/Hot-Sauce.md", line: 6,
              text: "Blend the roasted chillies with 15ml honey, 12g salt and the vinegar.")
    }
    private var loaf: RetrievedChunk {
        chunk("Recipes/Seeded-Loaf.md", line: 9,
              text: "Fold in 30g hazelnuts and 24g seed mix, then drizzle with honey.")
    }
    private let nutsQuestion = "Some small changes: 180g squash, 1 tsp salt. I want to try "
        + "walnuts this time. What's the quantities of nuts and seeds?"
    private let nutsAnswer = "Walnuts: 30g; seed mix: 24g. These quantities are from the "
        + "seeded honey hazelnut loaf recipe."

    /// THE 2026-11-22 INCIDENT. One shared word ("honey") passed the grounding check, but
    /// "24g" is in neither cited note: the value asked for was invented or carried from a
    /// note the answer did not cite.
    func testAnAnswerWhoseNumbersAreNotInWhatItCitesIsAnAbstain() {
        let draft = VaultAnswerDraft(
            answer: nutsAnswer,
            citations: ["Recipes/Clam-Pasta.md", "Recipes/Hot-Sauce.md"], abstain: false)
        XCTAssertEqual(
            VaultAnswerer.validate(draft, chunks: [pasta, hotSauce, loaf],
                                   question: nutsQuestion),
            .unanswered(.abstained))
    }

    /// The same answer citing the note that does state both quantities is an answer.
    func testTheSameAnswerCitingTheNoteThatStatesItsNumbersIsAnswered() {
        let draft = VaultAnswerDraft(answer: nutsAnswer,
                                     citations: ["Recipes/Seeded-Loaf.md"], abstain: false)
        XCTAssertNotNil(
            VaultAnswerer.validate(draft, chunks: [pasta, hotSauce, loaf],
                                   question: nutsQuestion).answer)
    }

    /// An answer with no digits in it has no numbers to check.
    func testAnAnswerWithNoDigitsIsUnaffectedByTheNumberCheck() {
        XCTAssertEqual(VaultAnswerer.numbersNotIn("Tenmoku, cone ten.", chunks: []), [])
        let glazes = chunk("Workshop/Glazes.md", line: 3,
                           text: "Tenmoku is fired to cone ten in reduction.")
        let draft = VaultAnswerDraft(answer: "Tenmoku, fired to cone ten.",
                                     citations: ["Workshop/Glazes.md"], abstain: false)
        XCTAssertNotNil(VaultAnswerer.validate(draft, chunks: [glazes]).answer)
    }

    /// A number written as a word is not a numeric token: "two" is checked, if at all, by
    /// the grounding rule, not this one.
    func testANumberWrittenAsAWordIsNotANumericToken() {
        XCTAssertEqual(VaultAnswerer.numericTokens("two eggs and a dozen walnuts"), [])
        let eggs = chunk("Recipes/Omelette.md", line: 1, text: "Whisk the eggs with chives.")
        let draft = VaultAnswerDraft(answer: "Two eggs, whisked with chives.",
                                     citations: ["Recipes/Omelette.md"], abstain: false)
        XCTAssertNotNil(VaultAnswerer.validate(draft, chunks: [eggs]).answer)
    }

    /// Every number in the answer is somewhere in what it cites, with its unit, whatever
    /// the spacing and case: `30 G` in a note is the `30g` of an answer.
    func testAnAnswerWhoseNumbersAreAllInACitedNotePasses() {
        let label = chunk("Food/Label.md", line: 2,
                          text: "Per pack: 2,040 kcal. Use 1.5 Tbsp. Add 30 G sugar.")
        let draft = VaultAnswerDraft(answer: "30g sugar and 1.5 tbsp, 2,040 kcal in all.",
                                     citations: ["Food/Label.md"], abstain: false)
        XCTAssertNotNil(VaultAnswerer.validate(draft, chunks: [label]).answer)
    }

    /// The tokenizer: a run of digits with an optional decimal part, and its unit when the
    /// letters are attached or one space away; the space and case are folded out.
    func testNumericTokensCarryTheirUnitFoldedAndUnspaced() {
        XCTAssertEqual(VaultAnswerer.numericTokens("30g, 30 G, 1.5 tbsp, 2,040 kcal"),
                       [.init(number: "30", unit: "g"), .init(number: "30", unit: "g"),
                        .init(number: "1.5", unit: "tbsp"), .init(number: "2040", unit: "kcal")])
        // Years, times and dates are numbers too.
        XCTAssertEqual(VaultAnswerer.numericTokens("2026-09-27 at 18:30"),
                       [.init(number: "2026", unit: ""), .init(number: "9", unit: ""),
                        .init(number: "27", unit: ""), .init(number: "18", unit: ""),
                        .init(number: "30", unit: "")])
    }

    /// The unit is part of the number: `30ml` is not the `30g` of a note.
    func testANumberWithTheWrongUnitIsNotInTheNote() {
        let note = chunk("Recipes/Dressing.md", line: 1, text: "Whisk 30ml olive oil.")
        XCTAssertEqual(VaultAnswerer.numbersNotIn("30g olive oil", chunks: [note]),
                       [.init(number: "30", unit: "g")])
        // …but a bare number matches the same number anywhere, whatever its unit there.
        XCTAssertEqual(VaultAnswerer.numbersNotIn("30, olive oil", chunks: [note]), [])
    }

    /// A date in an answer is checked like any other number: a year the note never
    /// states is not in the note.
    func testAYearTheCitedNoteDoesNotStateIsAnAbstain() {
        let boiler = chunk("House/Boiler.md", line: 2,
                           text: "The boiler service is booked for 8 October 2026.")
        let wrong = VaultAnswerDraft(answer: "The boiler service is on 8 October 2025.",
                                     citations: ["House/Boiler.md"], abstain: false)
        XCTAssertEqual(VaultAnswerer.validate(wrong, chunks: [boiler]),
                       .unanswered(.abstained))
        let right = VaultAnswerDraft(answer: "The boiler service is on 8 October 2026.",
                                     citations: ["House/Boiler.md"], abstain: false)
        XCTAssertNotNil(VaultAnswerer.validate(right, chunks: [boiler]).answer)
    }

    // MARK: - The prompt

    func testThePromptIsTheDateTheQuestionAndTheNotesAndNothingElse() {
        let prompt = VaultAnswerer.prompt(question: "  when is the concert  ",
                                          chunks: [chunk("A.md", line: 4, text: "on Thursday")],
                                          clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(prompt, "Today is Sunday, 27 September 2026.\nwhen is the concert"
                       + "\n\nNOTE A.md:4\non Thursday")
    }

    func testTheInstructionsAreShortEnoughForThisModel() {
        let words = VaultAnswerer.instructions.split(whereSeparator: \.isWhitespace)
        XCTAssertLessThanOrEqual(words.count, 60)
        XCTAssertTrue(VaultAnswerer.instructions.contains("Answer only from the notes given."))
        XCTAssertTrue(VaultAnswerer.instructions.contains("abstain"))
    }

    // MARK: - Orchestration

    func testAnUnavailableModelIsItsOwnOutcome() async {
        let generator = ScriptedGenerator(.draft(
            VaultAnswerDraft(answer: "x", citations: [], abstain: false)))
        generator.available = false
        let outcome = await VaultAnswerer(generator: generator)
            .answer(question: "q", chunks: chunks)
        XCTAssertEqual(outcome, .unanswered(.modelUnavailable))
        XCTAssertTrue(generator.calls.isEmpty, "an unavailable model is never called")
    }

    func testNoChunksIsNoHits() async {
        let generator = ScriptedGenerator(.draft(
            VaultAnswerDraft(answer: "x", citations: [], abstain: false)))
        let outcome = await VaultAnswerer(generator: generator)
            .answer(question: "q", chunks: [])
        XCTAssertEqual(outcome, .unanswered(.noHits))
        XCTAssertTrue(generator.calls.isEmpty)
    }

    /// The window retry: ONE more attempt, with half the chunks, and then no more.
    func testAContextWindowRefusalRetriesOnceWithHalfTheChunks() async {
        let draft = VaultAnswerDraft(answer: "Thursday.",
                                     citations: ["Family/School-Year.md"], abstain: false)
        let generator = ScriptedGenerator(.contextWindowThenDraft(draft))
        let outcome = await VaultAnswerer(generator: generator)
            .answer(question: "q", chunks: chunks)
        XCTAssertNotNil(outcome.answer)
        XCTAssertEqual(generator.calls.count, 2)
        XCTAssertEqual(generator.calls[0].count, 4)
        XCTAssertEqual(generator.calls[1].count, 2)
    }

    func testAContextWindowRefusalThatSurvivesTheRetryIsReported() async {
        let generator = ScriptedGenerator(.throwing(.contextWindow))
        let outcome = await VaultAnswerer(generator: generator)
            .answer(question: "q", chunks: chunks)
        XCTAssertEqual(outcome, .unanswered(.failed("prompt too large even halved")))
        XCTAssertEqual(generator.calls.count, 2, "one retry, not a loop")
    }

    func testAGenerationFailureCarriesItsReason() async {
        let generator = ScriptedGenerator(.throwing(.failed("the model declined")))
        let outcome = await VaultAnswerer(generator: generator)
            .answer(question: "q", chunks: chunks)
        XCTAssertEqual(outcome, .unanswered(.failed("the model declined")))
    }

    /// The clock. An abstain that arrives beats an answer that does not.
    func testAGenerationThatNeverReturnsTimesOut() async {
        let generator = ScriptedGenerator(.hang)
        let started = Date()
        let outcome = await VaultAnswerer(generator: generator, timeLimit: 0.15)
            .answer(question: "q", chunks: chunks)
        XCTAssertEqual(outcome, .unanswered(.timedOut))
        XCTAssertLessThan(Date().timeIntervalSince(started), 5,
                          "the limit is a limit, not a suggestion")
    }

    func testTheShippedTimeLimitIsTwentySeconds() {
        XCTAssertEqual(VaultAnswerer.defaultTimeLimit, 20)
    }

    // MARK: - Templates

    /// The 2026-10-01 incident, with the bridge unreachable: "What's the connection to
    /// Tailscale into my studio at home?" came back as a recovery command copied out of a
    /// runbook, `<macbook-ip>` and all, citing the runbook AND the remote access note whose
    /// first line held the address. Both paths were real, and `ping` and `tsmp` were in the
    /// runbook extract, so every other check passed it. An answer whose key value is an
    /// unfilled slot has stated no fact: it is a template, and it becomes an abstain.
    func testACopiedCommandWithAnUnfilledSlotIsAnAbstain() async {
        let runbook = chunk("Runbooks/Overlay-Wedge.md", line: 22, text: """
            If the git verb is blocked, check the path first:
            tailscale ping --tsmp <macbook-ip>
            """)
        let remote = chunk("Machines/Remote-Access.md", line: 3,
                           text: "Tailscale reaches the Studio at 192.0.2.10.")
        let draft = VaultAnswerDraft(
            answer: "tailscale ping --tsmp <macbook-ip>",
            citations: ["Runbooks/Overlay-Wedge.md", "Machines/Remote-Access.md"],
            abstain: false)
        let outcome = await VaultAnswerer(generator: ScriptedGenerator(.draft(draft)),
                                          ownerName: "Jeremy")
            .answer(question: "What's the connection to Tailscale into my studio at home?",
                    chunks: [runbook, remote])
        XCTAssertEqual(outcome, .unanswered(.abstained))
    }

    /// The rule is about SLOTS, not angle brackets: a comparison has spaces around its
    /// signs and an address has `@` and `.` in it, and neither is a template.
    func testAComparisonOrABracketedAddressIsNotASlot() {
        let kiln = chunk("Workshop/Kiln.md", line: 4, text: """
            Fire when the kiln reads < 600 and > 200. Bricks from Terrasole \
            <jeremy@example.com>.
            """)
        for answer in ["Fire it when it reads < 600 and > 200.",
                       "Terrasole, at <jeremy@example.com>."] {
            let draft = VaultAnswerDraft(answer: answer, citations: ["Workshop/Kiln.md"],
                                         abstain: false)
            XCTAssertNotNil(VaultAnswerer.validate(draft, chunks: [kiln],
                                                   question: "when do I fire the kiln").answer,
                            answer)
        }
    }
}
