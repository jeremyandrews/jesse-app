import XCTest
@testable import Jesse
import JesseNetworking

/// The day aggregation behind the Studio's vitals ledger, over fixed samples in a fixed
/// zone: a night is its wake date's, unknown stays nil, and a day with nothing known is
/// not a row.
@MainActor
final class DailyVitalsTests: XCTestCase {
    private let rome = TimeZone(identifier: "Europe/Rome")!

    private func at(_ d: Int, _ h: Int, _ m: Int = 0) -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = rome
        return cal.date(from: DateComponents(year: 2026, month: 9, day: d, hour: h, minute: m))!
    }

    private func sample(_ start: Date, _ end: Date, _ stage: Int,
                        source: String = "com.apple.health.watch") -> SleepSample {
        SleepSample(start: start, end: end, value: stage, sourceID: source)
    }

    func testANightThatCrossesMidnightBelongsToTheWakeDate() {
        // 23:10 on the 26th to 06:40 on the 27th: core, deep, awake, REM.
        let samples = [
            sample(at(26, 23, 10), at(27, 1, 0), SleepStage.asleepCore),
            sample(at(27, 1, 0), at(27, 2, 0), SleepStage.asleepDeep),
            sample(at(27, 2, 0), at(27, 2, 20), SleepStage.awake),
            sample(at(27, 2, 20), at(27, 6, 40), SleepStage.asleepREM),
        ]
        let nights = SleepReducer.nights(samples, timeZone: rome)
        XCTAssertEqual(Array(nights.keys), ["2026-09-27"], "the wake date, never the 26th")
        let night = nights["2026-09-27"]
        XCTAssertEqual(night?.totalMinutes, 110 + 60 + 260)
        XCTAssertEqual(night?.deepMinutes, 60)
        XCTAssertEqual(night?.remMinutes, 260)
        XCTAssertEqual(night?.awakeMinutes, 20)
    }

    func testTwoNightsAreTwoDatesAndANapIsNeither() {
        let samples = [
            sample(at(26, 23, 0), at(27, 7, 0), SleepStage.asleepCore),
            sample(at(27, 14, 0), at(27, 14, 40), SleepStage.asleepCore),   // a nap
            sample(at(27, 23, 30), at(28, 6, 30), SleepStage.asleepCore),
        ]
        let nights = SleepReducer.nights(samples, timeZone: rome)
        XCTAssertEqual(nights.keys.sorted(), ["2026-09-27", "2026-09-28"])
        XCTAssertEqual(nights["2026-09-27"]?.totalMinutes, 480, "the nap is not added to the night")
        XCTAssertEqual(nights["2026-09-28"]?.totalMinutes, 420)
    }

    func testTwoWritersForOneNightAreUnionedNotAdded() {
        let samples = [
            sample(at(26, 23, 0), at(27, 7, 0), SleepStage.asleepCore),
            sample(at(26, 23, 0), at(27, 7, 0), SleepStage.asleepUnspecified, source: "com.other.sleep"),
        ]
        XCTAssertEqual(SleepReducer.nights(samples, timeZone: rome)["2026-09-27"]?.totalMinutes, 480)
    }

    func testOvernightReadingsAverageByTheDayTheyEnd() {
        let readings: [(end: Date, value: Double)] = [
            (end: at(27, 3, 0), value: 16), (end: at(27, 5, 0), value: 18),
            (end: at(26, 23, 50), value: 20),
        ]
        let byDay = DailyVitals.averageByEndDay(readings, timeZone: rome)
        XCTAssertEqual(byDay["2026-09-27"], 17)
        XCTAssertEqual(byDay["2026-09-26"], 20)
    }

    func testUnknownStaysNilAndAnEmptyDayIsNoRow() {
        let nights = ["2026-09-27": SleepSummary(totalMinutes: 452, deepMinutes: 72, remMinutes: nil,
                                                 coreMinutes: 278, awakeMinutes: nil, isNap: false)]
        var q = DailyVitals.Quantities()
        q.restingHr = ["2026-09-26": 53, "2026-09-27": 57]
        let rows = DailyVitals.assemble(dates: ["2026-09-25", "2026-09-26", "2026-09-27"],
                                        nights: nights, quantities: q)
        XCTAssertEqual(rows.map(\.date), ["2026-09-26", "2026-09-27"], "the 25th knew nothing")
        XCTAssertNil(rows[0].sleepMin, "no night recorded is nil, not 0")
        XCTAssertEqual(rows[1], VitalsDay(date: "2026-09-27", sleepMin: 452, deepMin: 72, restingHr: 57))
    }

    func testDayKeysEndOnTodayOldestFirst() {
        XCTAssertEqual(DailyVitals.dayKeys(endingAt: at(28, 0, 30), days: 3, timeZone: rome),
                       ["2026-09-26", "2026-09-27", "2026-09-28"])
    }

    func testTheBackfillIsSentOnceThenOnlyTheResendSpan() async {
        let suite = "DailyVitalsTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let spans = SpanLog()
        let cfg = JesseConfig(host: "studio.example", port: 8765, token: "t")
        let sync = VitalsSync(
            defaults: defaults,
            configProvider: { cfg },
            read: { span in
                await spans.record(span)
                return [VitalsDay(date: "2026-09-27", sleepMin: 400)]
            },
            post: { _, _ in },
            protectedDataAvailable: { true })
        let first = await sync.sync()
        let second = await sync.sync()
        XCTAssertTrue(first && second)
        let seen = await spans.spans
        XCTAssertEqual(seen, [DailyVitals.backfillDays, DailyVitals.resendDays])
    }

    func testAFailedUploadLeavesTheBackfillOwed() async {
        let suite = "DailyVitalsTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let spans = SpanLog()
        let cfg = JesseConfig(host: "studio.example", port: 8765, token: "t")
        let failing = VitalsSync(
            defaults: defaults, configProvider: { cfg },
            read: { span in await spans.record(span); return [VitalsDay(date: "2026-09-27", hrv: 70)] },
            post: { _, _ in throw URLError(.notConnectedToInternet) },
            protectedDataAvailable: { true })
        let sent = await failing.sync()
        XCTAssertFalse(sent)
        _ = await failing.sync()
        let seen = await spans.spans
        XCTAssertEqual(seen, [DailyVitals.backfillDays, DailyVitals.backfillDays])
    }

    func testALockedPhoneReadsNothing() async {
        let spans = SpanLog()
        let cfg = JesseConfig(host: "studio.example", port: 8765, token: "t")
        let sync = VitalsSync(
            defaults: UserDefaults(suiteName: "DailyVitalsTests.locked.\(UUID().uuidString)")!,
            configProvider: { cfg },
            read: { span in await spans.record(span); return [] },
            post: { _, _ in }, protectedDataAvailable: { false })
        let sent = await sync.sync()
        XCTAssertFalse(sent)
        let seen = await spans.spans
        XCTAssertTrue(seen.isEmpty)
    }
}

private actor SpanLog {
    var spans: [Int] = []
    func record(_ span: Int) { spans.append(span) }
}
