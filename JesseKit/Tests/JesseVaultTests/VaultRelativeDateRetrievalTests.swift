import XCTest
@testable import JesseVault

// WHEN IS MY FLIGHT TODAY — the 2026-09-27 incident, asserted.
//
// Two questions, both asked on the phone with the bridge unreachable, both answered
// "Not found in the vault on this device. Queued for the bridge." over a vault holding
// the answer in three live notes:
//
//   Q1  "When is my flight today?"
//   Q2  "When is the KLM flight to Amsterdam today?"
//
// THREE CAUSES, AND EACH ONE HAS ITS OWN ASSERTION HERE, because a test that only
// checked the end result would pass again the moment two of them came back together:
//
//   1. EVERY KEYWORD WAS REQUIRED and "today" was a keyword. `VaultSearchQuery`
//      joins tokens with `AND`, and no note in a vault writes "today" for a date, so
//      the precise pass could not match and the single-keyword fallback fused a top
//      twenty per word.
//   2. THE OWNER'S NAME WAS REQUIRED TOO, on a first-person question, and the booking
//      that does name him spells it `JEREMIAH` — which `"Jeremy"*` does not match.
//   3. THE MODEL WAS NEVER TOLD THE DATE, so even with the itinerary in front of it,
//      "today" could not be matched to `Sun 27 Sep` and abstaining was correct.
//
// The corpus is `VaultRetrievalFixture`'s, which now holds two itineraries of the same
// shape on different days, a Today list that names the flight without the word, and
// four live notes that say "flight" and answer nothing. Every word of it is invented.
// The index and the filesystem are real; the clock is pinned and the embedding is
// absent, for the reasons the floor suite gives.
final class VaultRelativeDateRetrievalTests: XCTestCase {

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
        VaultRetrievalFixture.write(in: root)
        let index = try VaultIndex(
            url: VaultIndex.databaseURL(forRoot: root, in: databaseDirectory))
        let file = VaultFile(root: root)
        try index.reindex(scan: VaultScanner().scan(root: root),
                          read: { try file.read(relativePath: $0) })
        return index
    }

    private func retriever(_ index: VaultIndex, clock: VaultClock) -> VaultRetriever {
        VaultRetriever(index: index, ownerName: VaultRetrievalFixture.ownerName,
                       clock: clock)
    }

    /// The day's note, or the day's list, first — and the other day's flight behind it.
    private func assertTheDayIsFirst(_ paths: [String], _ question: String,
                                     file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue([VaultRetrievalFixture.dayTrip, VaultRetrievalFixture.todayList]
                        .contains(paths.first ?? ""),
                      "\(question) retrieved \(paths)", file: file, line: line)
        if let day = paths.firstIndex(of: VaultRetrievalFixture.dayTrip),
           let other = paths.firstIndex(of: VaultRetrievalFixture.otherTrip) {
            XCTAssertLessThan(day, other, "\(question) put the other day's flight first: "
                              + "\(paths)", file: file, line: line)
        }
    }

    // MARK: - The incident

    /// THE REGRESSION TEST. Both questions, over a corpus where four live notes say
    /// "flight" and a second itinerary has the same shape on another date.
    func testAQuestionAboutTodayRetrievesTodaysNoteFirst() async throws {
        let index = try indexedCorpus()
        let retriever = retriever(index, clock: VaultRetrievalFixture.clock)

        for question in ["When is my flight today?",
                         "When is the KLM flight to Amsterdam today?"] {
            let result = await retriever.retrieve(question: question,
                                                  budget: .forMeasuredPrompt(nil))
            assertTheDayIsFirst(result.chunks.map(\.path), question)
        }
    }

    /// The same shape one day earlier: "tomorrow" on the 26th is the 27th, and the 27th
    /// is the note that has to come back.
    func testAQuestionAboutTomorrowRetrievesTomorrowsNoteFirst() async throws {
        let index = try indexedCorpus()
        let retriever = retriever(index, clock: VaultRetrievalFixture.dayBeforeClock)

        for question in ["When is my flight tomorrow?",
                         "When is the KLM flight to Amsterdam tomorrow?"] {
            let result = await retriever.retrieve(question: question,
                                                  budget: .forMeasuredPrompt(nil))
            assertTheDayIsFirst(result.chunks.map(\.path), question)
        }
    }

    /// A day nothing in the vault is about answers from the content words alone, rather
    /// than answering nothing. The date PROMOTES; it has never been able to exclude.
    ///
    /// It is also the case that pins the sub-rank: this question is first person, so the
    /// owner group is in the plan, and both itineraries name him in capitals. A chunk
    /// that matches the owner and not one word of the question has not answered it.
    func testADayNoNoteMentionsStillAnswersFromTheWords() async throws {
        let index = try indexedCorpus()
        let result = await retriever(index, clock: VaultRetrievalFixture.clock)
            .retrieve(question: "how do I get to Perugia tomorrow?",
                      budget: .forMeasuredPrompt(nil))
        XCTAssertEqual(result.chunks.first?.path, "Travel/Perugia-Trip.md",
                       "got \(result.chunks.map(\.path))")
    }

    // MARK: - The owner's name, in every spelling

    /// The setting holds four spellings; the booking names one of them, in capitals,
    /// and it is not the one a sentence calls him.
    func testTheOwnerGroupMatchesASpellingTheShortNameCannotReach() throws {
        let capitals = "**Passenger:** JEREMIAH KIRSTEN ANDREWS"
        let short = LookupPlan.make(question: "When is my flight today?",
                                    ownerName: "Jeremy",
                                    clock: VaultRetrievalFixture.clock)
        let every = LookupPlan.make(question: "When is my flight today?",
                                    ownerName: VaultRetrievalFixture.ownerName,
                                    clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(short.ownerForms, ["Jeremy"])
        XCTAssertEqual(every.ownerForms, ["Jeremy", "Jeremiah", "Jeremia", "Andrews"])
        XCTAssertFalse(LookupPlan.matches(term: "Jeremy",
                                          inTokens: LookupPlan.tokenize(capitals)),
                       "a prefix term is not a fuzzy match: `Jeremy` is not in `JEREMIAH`")
        XCTAssertTrue(LookupPlan.matches(term: "Jeremiah",
                                         inTokens: LookupPlan.tokenize(capitals)))
    }

    /// ONE CONCEPT, COUNTED ONCE. A chunk that names him three ways has not answered
    /// three more of the question than one that names him once.
    func testEverySpellingOfTheOwnerCountsOnce() {
        let plan = LookupPlan.make(question: "When is my flight today?",
                                   ownerName: VaultRetrievalFixture.ownerName,
                                   clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(plan.groups.count, 3, "flight, the owner, the date: \(plan.groups)")
        XCTAssertEqual(plan.groupMatchCount(in: "Jeremy Jeremiah Jeremia Andrews"), 1)
        XCTAssertEqual(plan.groupMatchCount(in: "JEREMIAH KIRSTEN ANDREWS"), 1)
        XCTAssertEqual(plan.groupMatchCount(in: "flight JEREMIAH"), 2)
        XCTAssertEqual(plan.groupMatchCount(in: "Out, Sun 27 Sep flight JEREMIAH"), 3)
    }

    /// AND IT PROMOTES RATHER THAN GATES. A first-person question still retrieves notes
    /// that name nobody at all, which is what a booking, a calendar line and a Today
    /// item usually are.
    func testAFirstPersonQuestionStillRetrievesNotesThatNameNobody() async throws {
        let index = try indexedCorpus()
        let result = await retriever(index, clock: VaultRetrievalFixture.clock)
            .retrieve(question: "When is my flight today?",
                      budget: .forMeasuredPrompt(nil))
        let nameless = result.chunks.filter { chunk in
            !LookupQuery.ownerForms(VaultRetrievalFixture.ownerName).contains { form in
                LookupPlan.matches(term: form, inTokens: LookupPlan.tokenize(chunk.text))
            }
        }
        XCTAssertFalse(nameless.isEmpty,
                       "every chunk named the owner: \(result.chunks.map(\.path))")
    }

    /// A device with no name set behaves exactly as it did before any of this.
    func testWithNoOwnerNameThereIsNoOwnerGroup() {
        for name in [nil, "", "  ", ",", " , "] as [String?] {
            let plan = LookupPlan.make(question: "When is my flight today?",
                                       ownerName: name,
                                       clock: VaultRetrievalFixture.clock)
            XCTAssertEqual(plan.ownerForms, [], "owner name \(String(describing: name))")
        }
    }

    // MARK: - The lexical query

    /// The words that go to the index, and the ones that never should have.
    func testARelativeDayWordLeavesTheQueryAndItsDateArrives() {
        let plan = LookupPlan.make(question: "When is the KLM flight to Amsterdam today?",
                                   ownerName: nil, clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(plan.contentWords, ["KLM", "flight", "Amsterdam"])
        XCTAssertFalse(plan.terms.contains("today"))
        XCTAssertEqual(plan.dateForms, ["2026-09-27", "27 Sep", "Sep 27", "27 September",
                                        "September 27", "Sun 27 Sep", "Sun Sep 27",
                                        "Sun 27 September", "Sun September 27"])
    }

    /// Every listed phrase, and the day each one means. `this week` and `next week` name
    /// a span rather than a day, so they leave the query and bring no date with them.
    func testEveryRelativeDayWordResolves() {
        func dates(_ question: String, clock: VaultClock = VaultRetrievalFixture.clock)
            -> [String] {
            LookupPlan.make(question: question, ownerName: nil, clock: clock).dateForms
        }
        XCTAssertEqual(dates("anything today").first, "2026-09-27")
        XCTAssertEqual(dates("anything tonight").first, "2026-09-27")
        XCTAssertEqual(dates("anything this morning").first, "2026-09-27")
        XCTAssertEqual(dates("anything this afternoon").first, "2026-09-27")
        XCTAssertEqual(dates("anything this evening").first, "2026-09-27")
        XCTAssertEqual(dates("anything tomorrow").first, "2026-09-28")
        XCTAssertEqual(dates("anything yesterday").first, "2026-09-26")
        XCTAssertEqual(dates("anything this week"), [])
        XCTAssertEqual(dates("anything next week"), [])

        for question in ["what is on the calendar this week",
                         "which invoices are due next week",
                         "what did the studio deliver this morning"] {
            let plan = LookupPlan.make(question: question, ownerName: nil,
                                       clock: VaultRetrievalFixture.clock)
            XCTAssertFalse(plan.contentWords.contains { ["week", "morning"].contains($0) },
                           "\(question) kept a relative word: \(plan.contentWords)")
        }
    }

    /// …unless dropping it would leave nothing. "what is on this week" has one word in
    /// it that is not grammar, and an empty query retrieves nothing at all.
    func testTheLastWordIsKeptWhateverItIs() {
        for question in ["what is on this week", "what is on today?"] {
            let plan = LookupPlan.make(question: question, ownerName: nil,
                                       clock: VaultRetrievalFixture.clock)
            XCTAssertFalse(plan.contentWords.isEmpty, question)
        }
    }

    /// A word that only LOOKS relative keeps its place. "this morning" is a phrase; the
    /// morning routine is a thing in the vault.
    func testAWordIsOnlyRelativeInItsPhrase() {
        let plan = LookupPlan.make(question: "what is in the morning routine",
                                   ownerName: nil, clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(plan.contentWords, ["morning", "routine"])
        XCTAssertEqual(plan.dateForms, [])
    }

    /// A question of nothing but grammar and a relative day still searches for
    /// something, because an empty query retrieves nothing at all.
    func testAQuestionWithNothingLeftKeepsItsWords() {
        let plan = LookupPlan.make(question: "what is on today?", ownerName: nil,
                                   clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(plan.contentWords, ["today"])
        XCTAssertFalse(plan.dateForms.isEmpty)
    }

    /// ONE QUERY, EVERY TERM ORED. The pass that fired a query per keyword is what
    /// flooded the fusion, and this is the shape that replaced it.
    func testThePlanBuildsOneOrQuery() {
        let plan = LookupPlan.make(question: "When is my flight today?",
                                   ownerName: "Jeremy, Jeremiah",
                                   clock: VaultRetrievalFixture.clock)
        let expression = try? XCTUnwrap(plan.expression)
        XCTAssertEqual(expression,
                       "\"flight\"* OR \"Jeremy\"* OR \"Jeremiah\"* OR \"2026-09-27\"* "
                       + "OR \"27 Sep\"* OR \"Sep 27\"* OR \"27 September\"* "
                       + "OR \"September 27\"* OR \"Sun 27 Sep\"* OR \"Sun Sep 27\"* "
                       + "OR \"Sun 27 September\"* OR \"Sun September 27\"*")
    }

    /// THE TYPED SEARCH BOX IS UNTOUCHED. Its rule is every token, and it stays every
    /// token: the person typing into a field chose those words.
    func testTheTypedSearchBoxStillRequiresEveryToken() {
        XCTAssertEqual(VaultSearchQuery.matchExpression("flight today"),
                       "\"flight\"* AND \"today\"*")
        XCTAssertEqual(VaultSearchQuery.anyMatchExpression(["flight", "today"]),
                       "\"flight\"* OR \"today\"*")
        XCTAssertNil(VaultSearchQuery.anyMatchExpression([]))
        XCTAssertNil(VaultSearchQuery.anyMatchExpression(["", " ", "--"]))
    }

    /// A multi-word term is a PHRASE, and its last word a prefix — which is how
    /// `27 Sep` finds `27 September` without either being listed twice.
    func testADateTermIsAPhraseWithAPrefixOnItsLastWord() throws {
        let index = try indexedCorpus()
        let expression = try XCTUnwrap(VaultSearchQuery.anyMatchExpression(["Sun 27 Sep"]))
        let paths = index.search(expression: expression, limit: 10).map(\.path)
        XCTAssertTrue(paths.contains(VaultRetrievalFixture.dayTrip),
                      "`Sun 27 Sep` must find the itinerary row, got \(paths)")
        XCTAssertFalse(paths.contains(VaultRetrievalFixture.otherTrip))
        XCTAssertTrue(LookupPlan.matches(term: "27 Sep",
                                         inTokens: LookupPlan.tokenize("on 27 September")))
        XCTAssertFalse(LookupPlan.matches(term: "27 Sep",
                                          inTokens: LookupPlan.tokenize("27 October")))
    }

    // MARK: - The rank

    /// The rank itself, stated over hits rather than inferred from a corpus: more groups
    /// wins, bm25 breaks a tie, and one chunk per file survives.
    func testMoreConceptsWinsAndBm25BreaksTheTie() {
        func body(_ path: String, _ line: Int, _ score: Double, _ text: String)
            -> (hit: VaultSearchHit, text: String) {
            (VaultSearchHit(path: path, title: "", heading: "", line: line, snippet: "",
                            score: score), text)
        }
        let plan = LookupPlan.make(question: "When is my flight today?",
                                   ownerName: "Jeremy",
                                   clock: VaultRetrievalFixture.clock)
        let ranked = VaultRetriever.byConcept([
            body("loud.md", 1, -90, "flight flight flight flight flight"),
            body("one.md", 1, -2, "the flight leaves on Sun 27 Sep, Jeremy travelling"),
            body("two.md", 1, -3, "the flight leaves on Sun 27 Sep"),
            body("one.md", 9, -1, "flight"),
        ], plan: plan)
        XCTAssertEqual(ranked.map { "\($0.hit.path):\($0.hit.line)" },
                       ["one.md:1", "two.md:1", "loud.md:1"],
                       "three concepts, then two, then the loudest one")
    }

    // MARK: - The date in the prompt

    /// The model is told what day it is, from the injected clock, with the weekday.
    func testThePromptStatesTodaysDate() {
        let chunk = RetrievedChunk(path: "Travel/Rotterdam-Trip.md", line: 7,
                                   title: "Rotterdam trip", heading: "Flight Details",
                                   text: "| Out, Sun 27 Sep | FLR to AMS | KL1654 | 12:40 |")
        let prompt = VaultAnswerer.prompt(question: "When is my flight today?",
                                          chunks: [chunk],
                                          clock: VaultRetrievalFixture.clock)
        XCTAssertTrue(prompt.hasPrefix("Today is Sunday, 27 September 2026.\n"), prompt)
        XCTAssertTrue(prompt.contains("When is my flight today?"))
        XCTAssertTrue(prompt.contains("NOTE Travel/Rotterdam-Trip.md:7"))

        let yesterday = VaultAnswerer.prompt(question: "When was my flight?", chunks: [chunk],
                                             clock: VaultRetrievalFixture.dayBeforeClock)
        XCTAssertTrue(yesterday.hasPrefix("Today is Saturday, 26 September 2026.\n"),
                      yesterday)
    }

    /// The instructions say what a relative date means, and are still short enough for
    /// this model.
    func testTheInstructionsSayWhatTodayMeansAndStayShort() {
        let words = VaultAnswerer.instructions.split(whereSeparator: \.isWhitespace)
        XCTAssertLessThanOrEqual(words.count, 60, VaultAnswerer.instructions)
        XCTAssertTrue(VaultAnswerer.instructions.contains("Today, tomorrow and yesterday"))
        XCTAssertTrue(VaultAnswerer.instructions.contains("Answer only from the notes given."))
    }

    /// The clock is a value, and the date it spells is a pure function of it.
    func testTheDateFormsAreAPureFunctionOfTheClock() {
        let forms = LookupPlan.dateForms(for: VaultRetrievalFixture.clock.today,
                                         clock: VaultRetrievalFixture.clock)
        XCTAssertEqual(forms, LookupPlan.dateForms(for: VaultRetrievalFixture.clock.today,
                                                   clock: VaultRetrievalFixture.clock))
        XCTAssertEqual(forms.count, LookupPlan.dateFormats.count,
                       "an English device spells each format once: \(forms)")
        XCTAssertEqual(VaultRetrievalFixture.clock.todaySentence,
                       "Sunday, 27 September 2026")
    }
}
