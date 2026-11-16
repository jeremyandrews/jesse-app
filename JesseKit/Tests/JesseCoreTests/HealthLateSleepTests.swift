import XCTest
@testable import JesseCore

/// The late sleep turn's decisions: which night is last night, and when the one late turn
/// fires. The 2026-11-08 morning is the case: woke 08:36, a health turn at 08:51 carried the
/// previous night, and the real night reached the phone about 10:16.
final class HealthLateSleepTests: XCTestCase {

    private static let zone = TimeZone(identifier: "Europe/Rome")!

    private static var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        return c
    }

    private static func at(_ y: Int, _ mo: Int, _ d: Int, _ h: Int, _ mi: Int) -> Date {
        calendar.date(from: DateComponents(year: y, month: mo, day: d, hour: h, minute: mi))!
    }

    private static let today = "2026-11-08"

    // MARK: - endsOnDietDay

    func testANightEndingThisMorningIsLastNight() {
        XCTAssertTrue(HealthLateSleep.endsOnDietDay(end: Self.at(2026, 11, 8, 7, 10),
                                                    now: Self.at(2026, 11, 8, 8, 51),
                                                    calendar: Self.calendar))
    }

    func testANightEndingYesterdayMorningIsNotLastNight() {
        XCTAssertFalse(HealthLateSleep.endsOnDietDay(end: Self.at(2026, 11, 7, 7, 10),
                                                     now: Self.at(2026, 11, 8, 8, 51),
                                                     calendar: Self.calendar))
    }

    /// At 00:30 the diet day is still the day before, so the night that ended that morning
    /// is still last night.
    func testJustAfterMidnightTheNightThatEndedThePreviousMorningIsLastNight() {
        XCTAssertTrue(HealthLateSleep.endsOnDietDay(end: Self.at(2026, 11, 8, 7, 10),
                                                    now: Self.at(2026, 11, 9, 0, 30),
                                                    calendar: Self.calendar))
    }

    /// A session that started before midnight is dated by its END, this morning.
    func testASessionCrossingMidnightAndEndingThisMorningIsLastNight() {
        let start = Self.at(2026, 11, 7, 23, 20)
        let end = Self.at(2026, 11, 8, 6, 45)
        XCTAssertFalse(HealthLateSleep.endsOnDietDay(end: start, now: Self.at(2026, 11, 8, 8, 51),
                                                     calendar: Self.calendar),
                       "the start is yesterday's date; only the end counts")
        XCTAssertTrue(HealthLateSleep.endsOnDietDay(end: end, now: Self.at(2026, 11, 8, 8, 51),
                                                    calendar: Self.calendar))
    }

    // MARK: - shouldFire

    /// The 08:51 turn carried the not synced line; the real night lands: one late turn.
    func testTheNotSyncedCaseFires() {
        XCTAssertTrue(HealthLateSleep.shouldFire(sessionIsLastNight: true,
                                                 notSyncedDay: Self.today,
                                                 lastNightSentDay: nil, today: Self.today))
        XCTAssertTrue(HealthLateSleep.shouldFire(sessionIsLastNight: true,
                                                 notSyncedDay: Self.today,
                                                 lastNightSentDay: "2026-11-07",
                                                 today: Self.today))
    }

    /// The morning turn already had last night: nothing went out without it.
    func testAMorningTurnThatHadLastNightFiresNothing() {
        XCTAssertFalse(HealthLateSleep.shouldFire(sessionIsLastNight: true,
                                                  notSyncedDay: nil,
                                                  lastNightSentDay: Self.today,
                                                  today: Self.today))
        XCTAssertFalse(HealthLateSleep.shouldFire(sessionIsLastNight: true,
                                                  notSyncedDay: "2026-11-07",
                                                  lastNightSentDay: nil, today: Self.today),
                       "a not synced turn from an earlier day is not today's")
    }

    /// Once a turn today carried last night (the late turn itself included), never again.
    func testLastNightAlreadySentTodayFiresNothing() {
        XCTAssertFalse(HealthLateSleep.shouldFire(sessionIsLastNight: true,
                                                  notSyncedDay: Self.today,
                                                  lastNightSentDay: Self.today,
                                                  today: Self.today))
    }

    /// The arrival did not bring last night (an older session, a nap): nothing to send.
    func testAnOlderSessionFiresNothing() {
        XCTAssertFalse(HealthLateSleep.shouldFire(sessionIsLastNight: false,
                                                  notSyncedDay: Self.today,
                                                  lastNightSentDay: nil, today: Self.today))
    }

    // MARK: - The turn

    @MainActor
    func testTheLateSleepTurnSendsTheFixedPrompt() {
        XCTAssertEqual(HealthAutoTurn.lateSleep.prompt, HealthLateSleep.prompt)
        XCTAssertEqual(HealthAutoTurn.lateSleep.sentFor, PromptSender.healthLateSleep)
        XCTAssertFalse(HealthLateSleep.prompt.contains("\n"))
    }
}
