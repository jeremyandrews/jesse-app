import Foundation
@testable import JesseVault

// A BIGGER INVENTED VAULT, and twelve questions with the note that answers each.
//
// EVERY WORD OF IT IS MADE UP, for the reason `VaultFixture` says: this repository is
// public and not one line of the real vault may appear in it. The cast is a pottery
// studio, a bicycle, a fictional Italian supplier and an invented family, and the notes
// are shaped like real ones — frontmatter, `##` sections, wiki links, dates in prose —
// because the shape is what the retriever reads.
//
// It is bigger than `VaultFixture`'s five notes ON PURPOSE. A retrieval floor over a
// corpus small enough that every note is in the top four proves nothing; the point of
// these twenty-odd notes is that most of them are DISTRACTORS which share vocabulary
// with the questions.
//
// Two of them are under `Inbox/`, and both of them are traps: each holds a better
// keyword match for a question than the note that actually answers it. If the exclusion
// ever regresses, the floor fails rather than the behaviour quietly changing.
enum VaultRetrievalFixture {

    /// One question, and the note whose chunk must come back for it.
    struct Case {
        let question: String
        let expectedPath: String
    }

    /// The live itinerary for the day the clock is pinned to.
    static let dayTrip = "Travel/Rotterdam-Trip.md"
    /// A live itinerary of exactly the same shape on another day.
    static let otherTrip = "Travel/Vienna-Trip.md"
    /// The day's list, which names the flight without the word.
    static let todayList = "Today.md"

    /// THE DAY THIS CORPUS IS READ ON. Sunday 27 September 2026, 11:00 in Rome, which is
    /// the morning of the incident. Pinned rather than real for the reason every clock in
    /// this package is injected: a suite whose result changes overnight is not a test.
    static let clock = fixedClock(day: 27)
    /// The day before, for the "tomorrow" shape.
    static let dayBeforeClock = fixedClock(day: 26)

    /// The owner-name setting as the device holds it: every spelling his own notes use.
    static let ownerName = "Jeremy, Jeremiah, Jeremia, Andrews"

    static func fixedClock(day: Int) -> VaultClock {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "Europe/Rome") ?? .gmt
        let date = calendar.date(from: DateComponents(year: 2026, month: 9, day: day,
                                                      hour: 11, minute: 0)) ?? Date()
        return VaultClock.fixed(date, calendar: calendar,
                                locale: Locale(identifier: "en_GB"))
    }

    static func write(in root: URL) {
        func note(_ text: String, _ path: String) {
            VaultFixture.write(text, to: path, in: root)
        }

        note("""
            ---
            title: School year
            ---

            # School year

            ## Concert

            The spring concert is on Thursday 14 May at 18:30 in the school hall.
            Aurora plays second violin and needs to arrive at 17:45.

            ## Term dates

            Term ends on 26 June.
            """, "Family/School-Year.md")

        note("""
            # Family dates

            ## Birthdays

            Marta's birthday is 3 February. Alberto's is 19 September.
            Aurora's birthday is 11 March. The dog was born on 2 November.
            I was born on 4 September 1974.
            """, "Family/Birthdays.md")

        note("""
            ---
            title: Fiber contract
            ---

            # Fiber contract

            ## Decision

            We decided to stay on the twenty-four month fiber contract rather than break
            it early, because the exit fee was larger than the saving. Revisit in March.

            ## Alternatives considered

            A shorter contract at a higher monthly rate.
            """, "Projects/Fiber-Contract.md")

        note("""
            # Boiler

            The boiler service is booked for 8 October. The engineer is Nicola Fanti,
            who did last year's as well. He needs the cellar key.
            """, "House/Boiler.md")

        note("""
            ---
            title: The Kiln Rebuild
            tags: pottery, workshop
            ---

            # Kiln notes

            The old kiln's floor cracked in the spring firing.

            ## Bricks

            [[Suppliers/Terrasole]] quoted for forty soft bricks.

            ## Schedule

            - [ ] Order the bricks
            - [x] Measure the arch
            """, "Workshop/Kiln-Rebuild.md")

        note("""
            # Glazes

            ## Tenmoku

            Fired to cone ten in reduction. The recipe is forty feldspar, thirty silica,
            twenty whiting and ten red iron oxide.

            ## Shino

            Unreliable below cone eight.
            """, "Workshop/Glazes.md")

        note("""
            # Terrasole

            A brickyard outside Perugia. Ask for Alberto.

            ## Prices

            Soft brick, per pallet: quoted twice a year.
            """, "Suppliers/Terrasole.md")

        note("""
            # Clay orders

            The last clay order was six hundred kilograms of white stoneware, delivered
            on a pallet. The next one is due when the shelf is down to two bags.
            """, "Suppliers/Clay.md")

        note("""
            # Alberto Neri

            Runs the yard at [[Suppliers/Terrasole]]. His mobile is 0555 0102 0304.
            Speaks no English; write in Italian.
            """, "People/Alberto Neri.md")

        note("""
            # Marta Ruggeri

            Runs the burner workshop. Knows [[Workshop/Kiln-Rebuild]] inside out.
            She is usually at the studio on Tuesdays.
            """, "People/Marta Ruggeri.md")

        note("""
            # Winter bike

            ## Bottom bracket

            The bottom bracket is a 68 mm threaded English shell. The last one lasted
            two winters.

            ## Chain

            Replaced in November.
            """, "Bicycle/Winter-Bike.md")

        note("""
            # Perugia trip

            ## Train

            The 07:12 from Terontola gets in at 08:05. Buy the ticket the night before.

            ## Hotel

            Two nights at the Brufani, booked under Ruggeri.
            """, "Travel/Perugia-Trip.md")

        note("""
            # Studio rent

            The studio rent is 340 euro a month, paid on the first. The lease renews in
            September and the landlord is Signora Pini.
            """, "Workshop/Studio-Rent.md")

        note("""
            # Wheel maintenance

            The wheel bearing whines under load. Grease it before the next throwing day.
            """, "Workshop/Wheel.md")

        note("""
            # Firing log

            ## 12 March

            Bisque to cone 06, twelve hours, no cracks.

            ## 4 April

            Glaze firing to cone ten. Two pots lost to the shino.
            """, "Workshop/Firing-Log.md")

        note("""
            # Insurance

            The studio insurance renews on 30 November through Ferri Assicurazioni. The
            policy number is PT-884120.
            """, "House/Insurance.md")

        note("""
            # Car service

            The car is due its service at 120,000 km. The garage is Officina Bini in
            Cortona, who also did the timing belt.
            """, "House/Car.md")

        note("""
            # Reading

            Finished the book about Japanese wood firing. The chapter on anagama kilns is
            the one worth rereading.
            """, "Personal/Reading.md")

        // ── THE DAY'S TRAVEL, and the four notes the 2026-09-27 incident needs.
        //
        //    THE ANSWER CHUNK IS DELIBERATELY POOR. `## Flight Details` carries the date
        //    in the form an itinerary table writes it, a passenger name in capitals that
        //    is not the owner-name setting, a flight code and two airport codes — and NO
        //    city name, no airline name, and not the word "today". Every word the two
        //    questions actually contain is therefore missing from it except "flight",
        //    which is the whole shape of the incident: the note that answers the question
        //    shares almost none of the question's words.
        note("""
            ---
            title: Rotterdam trip
            ---

            # Rotterdam trip

            ## Flight Details

            **Booking code:** QXRTVB · **Passenger:** JEREMIAH KIRSTEN ANDREWS

            | Leg | Route | Flight | Dep | Arr |
            |---|---|---|---|---|
            | Out, Sun 27 Sep | FLR to AMS | KL1654 | 12:40 | 14:50 |
            | Back, Fri 2 Oct | AMS to FLR | KL1657 | 17:05 | 19:00 |

            ## Room

            Four nights in a junior suite, booked in the legal name.
            """, dayTrip)

        // The same shape on another date. Nothing separates it from the note above
        // except WHICH day it is, which is the point.
        note("""
            ---
            title: Vienna trip
            ---

            # Vienna trip

            ## Flight Details

            **Booking code:** BWQ2LM · **Passenger:** JEREMIAH KIRSTEN ANDREWS

            | Leg | Route | Flight | Dep | Arr |
            |---|---|---|---|---|
            | Out, Fri 16 Oct | FLR to VIE | OS512 | 09:15 | 11:05 |
            | Back, Tue 20 Oct | VIE to FLR | OS511 | 12:30 | 14:10 |
            """, otherTrip)

        // The day's list. It names the flight by number and time and never uses the
        // word "flight" — the note a person would expect to answer first and the one a
        // required-keyword query can never reach.
        note("""
            # Today

            - Travel day. KL 1654 leaves Florence 12:40, lands 14:50.
            - Ask [[Suppliers/Terrasole]] about the pallet before the bank closes.
            """, todayList)

        // Live notes that say "flight" and answer nothing. Without them the corpus
        // would have two flight notes and the questions could not go wrong.
        note("""
            # Stairs

            The flight of stairs down to the cellar has a loose tread, third from the
            bottom. The carpenter wants to see it before he quotes.
            """, "House/Stairs.md")

        note("""
            # Chicago notes

            The flight over was delayed four hours and the hotel held the room anyway.
            Next time, take the earlier flight and eat at the airport.
            """, "Travel/Chicago-Notes.md")

        note("""
            # Freight

            Air freight was quoted per flight and refused: the bricks go by road on a
            pallet, which is slower and a third of the money.
            """, "Suppliers/Freight.md")

        note("""
            # Birds

            A heron in flight over the valley at dusk, every evening this month, always
            downstream and never back.
            """, "Personal/Birds.md")

        note("""
            # Attic

            The attic flight is steeper than the cellar flight and the top step of the
            attic flight is loose.
            """, "House/Attic.md")

        note("""
            # Lost luggage

            The flight was fine; the bag took a later flight and arrived two days after
            the flight it was booked on.
            """, "Travel/Lost-Luggage.md")

        note("""
            # Model aeroplanes

            Arlo's glider flight lasted nine seconds. The next flight went into the
            olives and the flight after that into the road.
            """, "Personal/Gliders.md")

        // ── THE OWNER'S OWN NAME, EVERYWHERE, WHICH IS WHAT A PERSONAL VAULT IS LIKE.
        //
        //    Six short notes that name him and answer nothing about a flight. They are
        //    here because the pass they defeat was REAL: the retriever used to fall back
        //    to a query per keyword, each returning its own top twenty, and in a vault
        //    where the owner's name is in hundreds of notes that fallback fills the whole
        //    prompt with notes that merely say who he is. A corpus where his name appears
        //    only on the booking would prove the opposite of what it looks like.
        note("""
            # Passport

            The passport is in the legal name, JEREMIAH KIRSTEN ANDREWS, and so is the
            residence permit. JEREMIAH is what the questura prints; ANDREWS alone is what
            the old card said.
            """, "Personal/Passport.md")

        note("""
            # Dottor Bellini

            The file is under ANDREWS, JEREMIAH. Jeremy goes every spring and Jeremy's
            notes are still on paper.
            """, "People/Bellini.md")

        note("""
            # Tax

            The comune writes JEREMIA ANDREWS, the accountant writes Jeremiah Andrews, and
            the bank writes JEREMIAH K ANDREWS. All three are the same person.
            """, "House/Tax.md")

        note("""
            # Running log

            Jeremy ran the valley loop on Tuesday and again on Friday. Jeremy's shoes are
            done at 700 km and these are at 680.
            """, "Personal/Running-Log.md")

        note("""
            # Bank

            The account is JEREMIAH KIRSTEN ANDREWS. The card reads ANDREWS JEREMIAH K,
            which is why the name on a receipt never matches.
            """, "Personal/Bank.md")

        note("""
            # Library

            The card says ANDREWS, JEREMIAH. Jeremy has had it since the year the library
            reopened and Jeremy renews it every January.
            """, "Personal/Library.md")

        // ── THE TWO TRAPS. Both are under `Inbox/`, both are better keyword matches for
        //    a question than the note that answers it, and neither may ever be retrieved.
        note("""
            # Pasted mail

            From the school office: "the concert has been moved to Thursday 21 May at
            19:00 in the church" — concert concert concert school school hall.
            """, "Inbox/2026-09-01-pasted-mail.md")

        note("""
            # Scanned

            fiber contract fiber contract decided decision exit fee twenty-four month
            — scan of a letter about the fiber contract decision.
            """, "Inbox/archive/2026-08-11-scan.md")
    }

    /// The twelve. Each expected path must be among the chunks the retriever keeps.
    static let cases: [Case] = [
        Case(question: "when is the school concert",
             expectedPath: "Family/School-Year.md"),
        Case(question: "when is Marta's birthday",
             expectedPath: "Family/Birthdays.md"),
        // The two questions the on-device run measured against this corpus, and the
        // reason the birthday note carries an Aurora and a birth year: a gate the model
        // no longer votes on has to be shown letting BOTH of them through to the note
        // that answers them, not just the one it happened to like.
        Case(question: "what is Aurora's birthday",
             expectedPath: "Family/Birthdays.md"),
        Case(question: "when was I born",
             expectedPath: "Family/Birthdays.md"),
        Case(question: "what did we decide about the fiber contract",
             expectedPath: "Projects/Fiber-Contract.md"),
        Case(question: "when is the boiler service booked",
             expectedPath: "House/Boiler.md"),
        Case(question: "how many soft bricks did Terrasole quote for",
             expectedPath: "Workshop/Kiln-Rebuild.md"),
        Case(question: "what is the tenmoku glaze recipe",
             expectedPath: "Workshop/Glazes.md"),
        Case(question: "what is Alberto's mobile number",
             expectedPath: "People/Alberto Neri.md"),
        Case(question: "how much was the last clay order",
             expectedPath: "Suppliers/Clay.md"),
        Case(question: "what size is the winter bike bottom bracket",
             expectedPath: "Bicycle/Winter-Bike.md"),
        Case(question: "what train do we take to Perugia",
             expectedPath: "Travel/Perugia-Trip.md"),
        Case(question: "how much is the studio rent",
             expectedPath: "Workshop/Studio-Rent.md"),
        Case(question: "when does the studio insurance renew",
             expectedPath: "House/Insurance.md"),
        // ── THE FOUR THE 2026-09-27 INCIDENT ADDED. Every one of them asks with a word
        //    the answering note does not contain, which is what the old every-token rule
        //    could not survive: "today" and "this evening" appear in no note at all,
        //    "delivery" and "weigh" appear in none of the notes about clay.
        Case(question: "When is my flight today?", expectedPath: dayTrip),
        Case(question: "When is the KLM flight to Amsterdam today?", expectedPath: dayTrip),
        Case(question: "when is the school concert this evening",
             expectedPath: "Family/School-Year.md"),
        Case(question: "what did the last clay delivery weigh",
             expectedPath: "Suppliers/Clay.md"),
    ]
}

// A SECOND, SMALLER CORPUS: WHOSE BIRTHDAY, AND FROM WHICH NOTES.
//
// Kept apart from the twenty notes above on purpose. That corpus is the retrieval FLOOR's
// corpus, and a note added to it moves fourteen unrelated expectations; this one exists to
// reproduce one incident and holds only the notes that incident needs.
//
// The incident, 2026-09-26, bridge unreachable: "What's my birthday?" was answered with the
// start date of a family trip, cited from two ARCHIVED notes. The owner's birthday is in a
// live note, under a heading that names him — and the vault never says "my", it says the
// name.
//
// So there are four notes, and three of them are distractors of three different kinds:
//
//   * a LIVE note that answers the question, under `### Jeremy's Birthday (Sep 4)`;
//   * a LIVE note that is denser in the word "birthday" than the answer is and says
//     nothing about the owner — the distractor a name-less query cannot get past;
//   * an ARCHIVED draft about a trip that begins on a birthday;
//   * an ARCHIVED research note that mentions the owner AND a birthday, so it competes
//     with the answer on the owner's own name and only the archive demotion separates
//     the two.
//
// Invented, like everything else in this file. The trip, the party, the valley and both
// archived notes are made up; the owner's first name is the one real token in it, which is
// the whole point of the test.
enum VaultOwnerRetrievalFixture {

    /// The live note that answers the question.
    static let answer = "Family/Key-Dates.md"
    /// The live note that is denser in "birthday" and names nobody.
    static let liveDistractor = "Family/Party-Notes.md"
    /// Finished work: a packing list for a trip that leaves on a birthday.
    static let archivedDraft = "Projects/drafts/archive/2026-06-26-Perugia-Trip-Packing-List.md"
    /// Finished work that mentions the owner and a birthday in the same sentence.
    static let archivedResearch = "Projects/Research/archive/2026-05-26-Trip-Water-Risk-Notes.md"

    /// The owner's name, as the app's setting holds it.
    static let ownerName = "Jeremy"
    /// The question exactly as it was typed on the phone.
    static let question = "What's my birthday?"
    /// The same question without the contraction, which is the shape the stop list was
    /// written for.
    static let plainQuestion = "What is my birthday?"

    static func write(in root: URL) {
        func note(_ text: String, _ path: String) {
            VaultFixture.write(text, to: path, in: root)
        }

        note("""
            ---
            title: Key dates
            ---

            # Key dates

            The ones that never move. Anything that depends on a trip lives with the trip.

            ### Jeremy's Birthday (Sep 4)

            Cake at the studio after throwing, and the candles counted wrong on purpose.
            Marta brings the lemon one and the kiln stays cold that afternoon.

            ### Marta's Birthday (Feb 3)

            Dinner at the Brufani, booked a month ahead.
            """, answer)

        note("""
            # Party notes

            ## Birthday party

            The birthday party is in the garden: birthday cake, birthday candles, and the
            paper lanterns from last year. Fifteen children and two hours of it.
            """, liveDistractor)

        note("""
            # Perugia trip packing list

            The trip leaves on 26 June, which is the birthday itself, so the presents ride
            in the top of the case. The birthday lunch is booked for the 27th.
            """, archivedDraft)

        note("""
            # Trip water risk notes

            Written before the 26 June trip. The birthday plans put Jeremy at the far end
            of the valley, where the water is trucked in, and the birthday lunch on the
            27th is at the same address.
            """, archivedResearch)
    }
}
