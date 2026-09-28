import XCTest
@testable import JesseVault

// WHOSE BIRTHDAY, A SECOND TIME — the 2026-09-27 incident, asserted.
//
// With the bridge unreachable, "What is my birthday?" was answered "Jamie's birthday is on
// Tuesday, December 9.", cited from a LIVE trip itinerary titled for somebody else's
// birthday. The fix for the day before (the owner's name in the query, archived notes last)
// was in place and could not help: the decoy is live, and it names the owner, once, in its
// list of travellers. Replayed over the real notes, the note whose heading reads
// `Jeremy's Birthday` came first only with no embedding and a one-name setting; the
// embedding lifted a decoy over it, and the four-spelling setting dropped it out of the top
// five entirely.
//
// TWO THINGS WERE WRONG AND EACH HAS ITS OWN ASSERTION:
//
//   1. RETRIEVAL could not tell a note ABOUT the owner from a note that MENTIONS him. Both
//      matched "birthday" and the name, so they tied, and bm25 and the embedding both prefer
//      the note that says "birthday" five times.
//   2. THE ANSWER passed a sentence about another named person as the answer to a question
//      about "my" birthday. Every check it had was about citations and words, none about
//      whose fact it was.
//
// The model and the embedding are fakes; the index and the filesystem are real, for the
// reason `VaultRetrieverTests` gives.

/// An embedding that puts the decoy first and the owner's own heading LAST, which is what the
/// device's real embedding did to it: over the real notes, a note that only quoted the
/// heading ranked above the note that has it. The rank has to hold against that.
private struct DecoyLovingEmbedding: ChunkEmbedding {
    var isAvailable: Bool { true }
    func similarity(_ lhs: String, _ rhs: String) -> Double? {
        if rhs.contains("Lucia") { return 0.9 }
        if rhs.contains("Jeremy's Birthday") { return 0.1 }
        return 0.3
    }
}

/// A generator that returns one fixed sentence citing the first chunk it is handed.
private struct FixedSentence: VaultAnswerGenerating {
    let sentence: String
    var isAvailable: Bool { true }
    func generate(question: String, chunks: [RetrievedChunk]) async throws -> VaultAnswerDraft {
        VaultAnswerDraft(answer: sentence, citations: chunks.prefix(1).map(\.reference),
                         abstain: false)
    }
}

final class VaultOwnerSubjectTests: XCTestCase {

    private var root: URL!
    private var databaseDirectory: URL!

    override func setUp() {
        super.setUp()
        root = VaultFixture.makeDirectory()
        databaseDirectory = VaultFixture.makeDirectory()
    }

    override func tearDown() {
        VaultFixture.cleanUp(root)
        VaultFixture.cleanUp(databaseDirectory)
        super.tearDown()
    }

    private func indexedCorpus() throws -> VaultIndex {
        VaultOwnerRetrievalFixture.write(in: root)
        VaultOwnerRetrievalFixture.writeLiveDecoy(in: root)
        let index = try VaultIndex(
            url: VaultIndex.databaseURL(forRoot: root, in: databaseDirectory))
        let file = VaultFile(root: root)
        try index.reindex(scan: VaultScanner().scan(root: root),
                          read: { try file.read(relativePath: $0) })
        return index
    }

    // MARK: - Retrieval

    /// THE REGRESSION TEST. A live note that names the owner in passing, beside the live
    /// note whose heading is the owner's birthday: the heading wins, with and without an
    /// embedding that prefers the decoy, and with one spelling or four.
    func testTheOwnersOwnHeadingOutranksALiveNoteThatOnlyMentionsHim() async throws {
        let index = try indexedCorpus()
        let embeddings: [(String, any ChunkEmbedding)] = [("none", NoChunkEmbedding()),
                                                           ("decoy", DecoyLovingEmbedding())]
        for name in ["Jeremy", "Jeremy, Jeremiah, Jeremia, Andrews"] {
            for (label, embedding) in embeddings {
                let retriever = VaultRetriever(index: index, embedding: embedding,
                                               ownerName: name)
                for question in [VaultOwnerRetrievalFixture.question,
                                 VaultOwnerRetrievalFixture.plainQuestion] {
                    let result = await retriever.retrieve(question: question,
                                                          budget: .forMeasuredPrompt(nil))
                    let first = result.chunks.first
                    XCTAssertEqual(first?.path, VaultOwnerRetrievalFixture.answer,
                                   "\(question) [\(name), embedding \(label)] retrieved "
                                   + "\(result.chunks.map(\.reference))")
                    XCTAssertEqual(first?.heading, "Jeremy's Birthday (Sep 4)",
                                   "the owner's own chunk of the note, not Marta's")
                }
            }
        }
    }

    /// THE SAME QUESTION IN A VAULT FULL OF HIS NAME. Over the real notes with the
    /// four-spelling setting, the chunk under `Jeremy's Birthday` was not in the top five,
    /// because it was not in the rows the OR query was read to at all: more notes than the
    /// scan is deep say his names over and over, and "birthday" besides. No ranking fixes a row that never
    /// arrives.
    func testTheOwnersHeadingIsFoundInAVaultFullOfHisName() async throws {
        VaultOwnerRetrievalFixture.write(in: root)
        VaultOwnerRetrievalFixture.writeLiveDecoy(in: root)
        for number in 0..<(VaultRetriever.lexicalScanLimit + 30) {
            VaultFixture.write("""
                # Receipt \(number)

                JEREMIAH KIRSTEN ANDREWS, card ANDREWS JEREMIAH K. Jeremy Andrews signed;
                the comune writes JEREMIA ANDREWS. A birthday card, filed for Jeremy under Andrews.
                """, to: "House/Receipts/Receipt-\(number).md", in: root)
        }
        let index = try VaultIndex(
            url: VaultIndex.databaseURL(forRoot: root, in: databaseDirectory))
        let file = VaultFile(root: root)
        try index.reindex(scan: VaultScanner().scan(root: root),
                          read: { try file.read(relativePath: $0) })

        let retriever = VaultRetriever(index: index,
                                       ownerName: "Jeremy, Jeremiah, Jeremia, Andrews")
        let result = await retriever.retrieve(question: VaultOwnerRetrievalFixture.question,
                                              budget: .forMeasuredPrompt(nil))
        XCTAssertEqual(result.chunks.first?.path, VaultOwnerRetrievalFixture.answer,
                       "got \(result.chunks.map(\.reference))")
    }

    /// The candidate query exists only when there is an owner and a word to pair him with.
    func testTheSubjectQueryIsBuiltOnlyForAQuestionAboutTheOwner() {
        let plan = LookupPlan.make(question: "What is my birthday?",
                                   ownerName: "Jeremy, Jeremiah Kirsten Andrews")
        XCTAssertEqual(plan.subjectExpression,
                       "{heading title} : (NEAR(\"Jeremy\" \"birthday\"*, 2) OR "
                       + "NEAR(\"Jeremiah Kirsten Andrews\" \"birthday\"*, 2))")
        XCTAssertNil(LookupPlan.make(question: "What is my birthday?", ownerName: nil)
            .subjectExpression)
        XCTAssertNil(LookupPlan.make(question: "When is Lucia's birthday?", ownerName: "Jeremy")
            .subjectExpression)
    }

    /// A question about somebody else is not the owner's, and nothing is promoted for it.
    func testAQuestionAboutSomebodyElseIsNotPromotedForTheOwner() async throws {
        let index = try indexedCorpus()
        let retriever = VaultRetriever(index: index, embedding: DecoyLovingEmbedding(),
                                       ownerName: "Jeremy")
        let result = await retriever.retrieve(question: "When is Lucia's birthday?",
                                              budget: .forMeasuredPrompt(nil))
        XCTAssertEqual(result.chunks.first?.path, VaultOwnerRetrievalFixture.liveDecoy,
                       "got \(result.chunks.map(\.reference))")
    }

    // MARK: - The subject rule, on its own

    func testTheOwnerIsTheSubjectOnlyWhenHisNameStandsNextToAQuestionWord() {
        let plan = LookupPlan.make(question: "What is my birthday?", ownerName: "Jeremy")
        func subject(_ text: String) -> Bool {
            plan.namesOwnerAsSubject(inTokens: LookupPlan.tokenize(text))
        }
        XCTAssertTrue(subject("Jeremy's Birthday (Sep 4)"), "possessive")
        XCTAssertTrue(subject("Jeremy birthday dinner"), "subject position")
        XCTAssertFalse(subject("Lisbon, Lucia's Birthday"), "somebody else's")
        XCTAssertFalse(subject("Travellers: Lucia, Jeremy, Aurora, Arlo"), "no question word")
        XCTAssertFalse(subject("Birthday party with Jeremy and the children"),
                       "the name AFTER the word is a guest, not the subject")
        XCTAssertFalse(subject("Jeremy, Lucia and the birthday"), "too far apart")
    }

    /// No name, or a question not about the asker, never makes anyone the subject.
    func testNoOwnerMeansNoSubject() {
        let tokens = LookupPlan.tokenize("Jeremy's Birthday (Sep 4)")
        XCTAssertFalse(LookupPlan.make(question: "What is my birthday?", ownerName: nil)
            .namesOwnerAsSubject(inTokens: tokens))
        XCTAssertFalse(LookupPlan.make(question: "When is Lucia's birthday?", ownerName: "Jeremy")
            .namesOwnerAsSubject(inTokens: tokens))
    }

    // MARK: - The answer

    private let decoyChunk = RetrievedChunk(
        path: VaultOwnerRetrievalFixture.liveDecoy, line: 13, title: "Lisbon, Lucia's Birthday",
        heading: "Tuesday, December 9: Jamie's Birthday",
        text: "### Tuesday, December 9: Jamie's Birthday\n\nLunch by the river.")

    /// THE INCIDENT'S OWN SENTENCE. A first-person question answered with another named
    /// person's fact is not an answer; it falls back to the ordinary not-found.
    func testAnAnswerAboutSomebodyElseIsRefusedForAFirstPersonQuestion() async {
        let outcome = await VaultAnswerer(
            generator: FixedSentence(sentence: "Jamie's birthday is on Tuesday, December 9."),
            ownerName: "Jeremy")
            .answer(question: "What is my birthday?", chunks: [decoyChunk])
        XCTAssertEqual(outcome, .unanswered(.abstained))
    }

    func testAnAnswerAboutTheOwnerOrUnnamedStillPasses() async {
        let chunk = RetrievedChunk(path: VaultOwnerRetrievalFixture.answer, line: 7,
                                   title: "Key dates", heading: "Jeremy's Birthday (Sep 4)",
                                   text: "### Jeremy's Birthday (Sep 4)\n\nCake at the studio.")
        for sentence in ["Jeremy's birthday is Sep 4.", "Your birthday is Sep 4.",
                         "Sep 4, with cake at the studio."] {
            let outcome = await VaultAnswerer(generator: FixedSentence(sentence: sentence),
                                              ownerName: "Jeremy, Jeremiah")
                .answer(question: "What is my birthday?", chunks: [chunk])
            XCTAssertNotNil(outcome.answer, "\(sentence) -> \(outcome.label)")
        }
    }

    /// Only a question about the asker is held to this. "When is Jamie's birthday" is
    /// answered by a sentence about Jamie, and that is right.
    func testAQuestionAboutSomebodyElseMayBeAnsweredAboutThem() async {
        let outcome = await VaultAnswerer(
            generator: FixedSentence(sentence: "Jamie's birthday is on Tuesday, December 9."),
            ownerName: "Jeremy")
            .answer(question: "When is Jamie's birthday?", chunks: [decoyChunk])
        XCTAssertNotNil(outcome.answer, outcome.label)
    }

    func testTheSubjectCheckReadsTheLeadingName() {
        let owner = "Jeremy, Jeremiah Kirsten Andrews"
        let asked = "What is my birthday?"
        XCTAssertTrue(VaultAnswerer.isAboutSomebodyElse(
            "Jamie's birthday is on Tuesday, December 9.", question: asked, ownerName: owner))
        XCTAssertTrue(VaultAnswerer.isAboutSomebodyElse(
            "Lucia’s birthday is December 9.", question: asked, ownerName: owner), "curly apostrophe")
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "Jeremy's birthday is September 4.", question: asked, ownerName: owner))
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "Andrews's birthday is September 4.", question: asked, ownerName: owner),
                       "any spelling of him")
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "Jeremy Andrews's birthday is September 4.", question: asked, ownerName: "Jeremy"),
                       "the rest of his name, on a one-name setting")
        XCTAssertTrue(VaultAnswerer.isAboutSomebodyElse(
            "Jamie was born on 29 June.", question: "When was I born?", ownerName: owner),
                      "a name opening the sentence is its subject")
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "On 4 September, with Jamie.", question: asked, ownerName: owner),
                       "a name that is not the subject of the fact")
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "September 4.", question: asked, ownerName: owner))
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "Your birthday is September 4.", question: asked, ownerName: owner))
        XCTAssertFalse(VaultAnswerer.isAboutSomebodyElse(
            "Jamie's birthday is December 9.", question: asked, ownerName: nil), "no name, no claim")
    }

    /// The prompt tells the model who is asking, and only when that is known and asked.
    func testThePromptNamesTheAskerForAFirstPersonQuestion() {
        let clock = VaultRetrievalFixture.clock
        let named = VaultAnswerer.prompt(question: "What is my birthday?", chunks: [decoyChunk],
                                         clock: clock, ownerName: "Jeremy, Jeremiah")
        XCTAssertTrue(named.contains("\nThe asker is Jeremy.\n"), named)
        for (question, owner) in [("What is my birthday?", nil),
                                  ("When is Jamie's birthday?", "Jeremy")] as [(String, String?)] {
            let plain = VaultAnswerer.prompt(question: question, chunks: [decoyChunk],
                                             clock: clock, ownerName: owner)
            XCTAssertFalse(plain.contains("The asker is"), plain)
        }
    }

    // MARK: - No name set

    /// A first-person question on a device with no owner name says so in the reply, rather
    /// than silently searching for the word alone.
    func testAFirstPersonQuestionWithNoOwnerNameSaysSo() {
        XCTAssertEqual(OfflineLookupReply.ownerNameNotice(question: "What is my birthday?",
                                                          ownerName: nil),
                       OfflineLookupReply.ownerNameMissing)
        XCTAssertNil(OfflineLookupReply.ownerNameNotice(question: "What is my birthday?",
                                                        ownerName: "Jeremy"))
        XCTAssertNil(OfflineLookupReply.ownerNameNotice(question: "When is Lucia's birthday?",
                                                        ownerName: nil))
        let body = OfflineLookupReply.body(.abstained, queued: false,
                                           notice: OfflineLookupReply.ownerNameMissing)
        XCTAssertEqual(body, OfflineLookupReply.badge + "\n\nNot found in the vault on this device."
                       + "\n\n" + OfflineLookupReply.ownerNameMissing)
    }
}
