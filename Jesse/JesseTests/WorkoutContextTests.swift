import XCTest
import HealthKit
@testable import Jesse

/// Pure-logic tests for the recent-workouts subsection renderer: the per-workout
/// line (base fields), the three droppable suffixes (running dynamics, workout
/// detail, per-km splits), the two reducers that feed them, and the subsection
/// header. The window/cap/ordering/composition live in `HealthContextFormatter` and
/// are covered by `HealthContextTests`. Everything here is deterministic — a fixed
/// UTC calendar — so the rendered bytes are pinned.
@MainActor
final class WorkoutContextTests: XCTestCase {

    // Fixed UTC calendar so date rendering is deterministic regardless of host TZ.
    private let utc = TimeZone(identifier: "UTC")!
    private func date(_ y: Int, _ mo: Int, _ d: Int, _ h: Int, _ mi: Int) -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utc
        return cal.date(from: DateComponents(year: y, month: mo, day: d, hour: h, minute: mi))!
    }

    private func swim(start: Date, source: String = "Apple Watch") -> WorkoutSummary {
        WorkoutSummary(activityName: "Swim", start: start, duration: 1800,
                       distanceMeters: 1500, activeEnergyKcal: 420,
                       averageHeartRateBPM: 132, maxHeartRateBPM: 158, source: source)
    }

    // MARK: - Base line

    func testBaseLineRendersExactFormat() {
        let line = WorkoutContextFormatter.baseLine(for: swim(start: date(2026, 7, 4, 6, 30)),
                                                    timeZone: utc)
        XCTAssertEqual(line,
            "Swim — 2026-07-04 06:30, 30m00s, 1500 m, 420 kcal, avg HR 132, max HR 158 (Apple Watch)")
    }

    func testBaseLineOmitsNilFieldsAndSource() {
        let bare = WorkoutSummary(activityName: "Walk", start: date(2026, 7, 4, 8, 0),
                                  duration: 3660, distanceMeters: nil, activeEnergyKcal: nil,
                                  averageHeartRateBPM: nil, maxHeartRateBPM: nil, source: nil)
        let line = WorkoutContextFormatter.baseLine(for: bare, timeZone: utc)
        XCTAssertEqual(line, "Walk — 2026-07-04 08:00, 1h01m00s")
        XCTAssertFalse(line.contains("("), "no source paren when source is nil")
    }

    func testHeaderSingularAndPlural() {
        XCTAssertEqual(WorkoutContextFormatter.header(count: 1),
                       "1 recent workout from Apple Health (last 48h, newest first):")
        XCTAssertEqual(WorkoutContextFormatter.header(count: 3),
                       "3 recent workouts from Apple Health (last 48h, newest first):")
    }

    // MARK: - Running-dynamics suffix

    private func run(dynamics: Bool) -> WorkoutSummary {
        WorkoutSummary(activityName: "Run", start: date(2026, 7, 4, 7, 0), duration: 2700,
                       distanceMeters: 8000, activeEnergyKcal: 500,
                       averageHeartRateBPM: 150, maxHeartRateBPM: 172, source: "Apple Watch",
                       averageRunningPowerW: dynamics ? 245 : nil,
                       groundContactTimeMs: dynamics ? 240 : nil,
                       verticalOscillationCm: dynamics ? 8.1 : nil,
                       strideLengthM: dynamics ? 1.15 : nil)
    }

    func testDynamicsSuffixRendersAllFields() {
        XCTAssertEqual(WorkoutContextFormatter.dynamicsSuffix(for: run(dynamics: true)),
                       ", power 245 W, GCT 240 ms, vert osc 8.1 cm, stride 1.15 m")
    }

    func testDynamicsSuffixEmptyWhenNoDynamics() {
        XCTAssertEqual(WorkoutContextFormatter.dynamicsSuffix(for: run(dynamics: false)), "")
        XCTAssertFalse(run(dynamics: false).hasRunningDynamics)
        XCTAssertTrue(run(dynamics: true).hasRunningDynamics)
    }

    func testDynamicsSuffixOmitsIndividualNilFields() {
        var r = run(dynamics: true)
        r.groundContactTimeMs = nil
        r.strideLengthM = nil
        XCTAssertEqual(WorkoutContextFormatter.dynamicsSuffix(for: r),
                       ", power 245 W, vert osc 8.1 cm")
    }

    /// The segments concatenate in the order the byte cap sheds them from the right:
    /// base, dynamics, detail, splits. This run has no splits, and the only detail
    /// it can produce is the pace its distance and duration imply.
    func testFullLineAppendsDynamicsThenDetailAfterBase() {
        XCTAssertEqual(WorkoutContextFormatter.line(for: run(dynamics: true), timeZone: utc),
            "Run — 2026-07-04 07:00, 45m00s, 8.00 km, 500 kcal, avg HR 150, max HR 172 (Apple Watch)"
            + ", power 245 W, GCT 240 ms, vert osc 8.1 cm, stride 1.15 m"
            + ", pace 5:38/km (computed)")
    }

    // MARK: - Distance format (the rounding that used to eat a swim's pace)

    /// A swim prints whole meters at any length. `1650 m` rounded to `1.6 km` put a
    /// 14-second band on any per-100m pace computed from it — the reason for the
    /// split format rather than one rule for everything.
    func testSwimDistanceIsWholeMetersAtAnyLength() {
        var s = swim(start: date(2026, 7, 4, 6, 30))
        s.distanceMeters = 1650
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: s, timeZone: utc)
            .contains(", 1650 m,"))
        s.distanceMeters = 800
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: s, timeZone: utc)
            .contains(", 800 m,"))
    }

    /// Everything else keeps kilometers, but to two decimals — 10 m of resolution,
    /// enough that a per-km pace is exact to the second.
    func testNonSwimDistanceIsTwoDecimalKilometers() {
        var r = run(dynamics: false)
        r.distanceMeters = 7840
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: r, timeZone: utc)
            .contains(", 7.84 km,"))
        r.distanceMeters = 850
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: r, timeZone: utc)
            .contains(", 850 m,"), "under a kilometer stays whole meters")
    }

    /// The recording device's model rides inside the existing source parentheses,
    /// so a hardware change is visible in the data.
    func testProductTypeJoinsTheSourceParenthetical() {
        var r = run(dynamics: false)
        r.productType = "Watch7,5"
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: r, timeZone: utc)
            .hasSuffix(" (Apple Watch, Watch7,5)"))
        r.source = nil
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: r, timeZone: utc)
            .hasSuffix(" (Watch7,5)"), "product type alone still parenthesizes")
    }

    // MARK: - Swim lap reducer

    private func lap(_ minute: Int, seconds: Double, stroke: Int?, swolf: Double?) -> SwimLap {
        let start = date(2026, 7, 4, 6, 30).addingTimeInterval(Double(minute) * 60)
        return SwimLap(start: start, end: start.addingTimeInterval(seconds),
                       strokeStyleRawValue: stroke, swolf: swolf)
    }

    func testLapReducerMixedStrokes() {
        let laps = [
            lap(0, seconds: 45, stroke: 2, swolf: 52),   // freestyle
            lap(1, seconds: 44, stroke: 2, swolf: 50),
            lap(2, seconds: 60, stroke: 4, swolf: 66),   // breaststroke
            lap(3, seconds: 46, stroke: 2, swolf: 54),
        ]
        let r = SwimLapReducer.reduce(laps)!
        XCTAssertEqual(r.lapCount, 4)
        XCTAssertEqual(r.swimSeconds, 195, accuracy: 0.001)
        XCTAssertEqual(r.averageSWOLF!, 55.5, accuracy: 0.001)
        XCTAssertEqual(r.lapsByStroke, ["freestyle": 3, "breaststroke": 1])
    }

    /// A lap with no SWOLF still counts toward the lap count and the swim time; it
    /// only narrows the average's base.
    func testLapReducerMissingSWOLFOnSomeLaps() {
        let laps = [
            lap(0, seconds: 45, stroke: 2, swolf: 52),
            lap(1, seconds: 45, stroke: 2, swolf: nil),
            lap(2, seconds: 45, stroke: 2, swolf: 58),
        ]
        let r = SwimLapReducer.reduce(laps)!
        XCTAssertEqual(r.lapCount, 3, "the scoreless lap is still a lap")
        XCTAssertEqual(r.swimSeconds, 135, accuracy: 0.001)
        XCTAssertEqual(r.averageSWOLF!, 55, accuracy: 0.001, "averaged over the two scores")
        XCTAssertEqual(r.lapsByStroke, ["freestyle": 3])
    }

    func testLapReducerNoSWOLFAtAllAndUnknownStrokes() {
        let laps = [lap(0, seconds: 45, stroke: nil, swolf: nil),
                    lap(1, seconds: 45, stroke: 99, swolf: nil)]
        let r = SwimLapReducer.reduce(laps)!
        XCTAssertEqual(r.lapCount, 2)
        XCTAssertNil(r.averageSWOLF, "no score anywhere → no average, not a zero")
        XCTAssertTrue(r.lapsByStroke.isEmpty, "an unmapped raw value is uncounted")
    }

    func testLapReducerZeroLapsIsNil() {
        XCTAssertNil(SwimLapReducer.reduce([]))
    }

    /// The stroke names are pinned to HealthKit's `HKSwimmingStrokeStyle` raw
    /// values 0…6; out-of-range values map to nothing.
    func testStrokeNameRawValueMapping() {
        XCTAssertEqual((0...6).map { SwimLapReducer.strokeName($0) },
                       ["unknown", "mixed", "freestyle", "backstroke",
                        "breaststroke", "butterfly", "kickboard"])
        XCTAssertNil(SwimLapReducer.strokeName(7))
        XCTAssertNil(SwimLapReducer.strokeName(-1))
    }

    // MARK: - Split reducer

    private func sample(_ offset: Double, _ length: Double, _ meters: Double) -> DistanceSample {
        let base = date(2026, 7, 4, 7, 0)
        return DistanceSample(start: base.addingTimeInterval(offset),
                              end: base.addingTimeInterval(offset + length), meters: meters)
    }

    /// A km boundary inside a sample is interpolated, not attributed to the whole
    /// sample: 600 m in 60 s then 800 m in 80 s crosses 1000 m 40 s into the second.
    func testSplitReducerInterpolatesABoundaryInsideASample() {
        let splits = SplitReducer.splitSeconds([sample(0, 60, 600), sample(60, 80, 800)])
        XCTAssertEqual(splits.count, 1)
        XCTAssertEqual(splits[0], 100, accuracy: 0.001)
    }

    /// The last, incomplete kilometer produces no split at all.
    func testSplitReducerPartialFinalKmProducesNoSplit() {
        // 1400 m in 7 even 200 m samples of 60 s each.
        let samples = (0..<7).map { sample(Double($0) * 60, 60, 200) }
        let splits = SplitReducer.splitSeconds(samples)
        XCTAssertEqual(splits.count, 1, "only the completed kilometer")
        XCTAssertEqual(splits[0], 300, accuracy: 0.001)
    }

    func testSplitReducerEmptyInput() {
        XCTAssertEqual(SplitReducer.splitSeconds([]), [])
        XCTAssertEqual(SplitReducer.splitSeconds([sample(0, 60, 0)]), [],
                       "a zero-distance sample contributes nothing")
    }

    /// Several boundaries inside ONE sample all come out, and a gap between samples
    /// (a pause) lands in the split that contains it — an elapsed pace, by design.
    func testSplitReducerMultipleBoundariesAndAGap() {
        XCTAssertEqual(SplitReducer.splitSeconds([sample(0, 900, 3000)]),
                       [300, 300, 300])
        // 1000 m in 300 s, a 60 s gap, then 1000 m in 300 s.
        let gapped = SplitReducer.splitSeconds([sample(0, 300, 1000), sample(360, 300, 1000)])
        XCTAssertEqual(gapped.count, 2)
        XCTAssertEqual(gapped[0], 300, accuracy: 0.001)
        XCTAssertEqual(gapped[1], 360, accuracy: 0.001, "the pause is inside the second km")
    }

    // MARK: - Detail suffix

    /// Every new field nil → the line is byte-identical to the base line and
    /// nothing more. This is the guarantee that absent data renders nothing.
    func testAllNewFieldsNilRendersExactlyTheBaseLine() {
        let bare = WorkoutSummary(activityName: "Workout", start: date(2026, 7, 4, 9, 0),
                                  duration: 1200, distanceMeters: nil,
                                  activeEnergyKcal: 130, source: "iPhone")
        let base = WorkoutContextFormatter.baseLine(for: bare, timeZone: utc)
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: bare), "")
        XCTAssertEqual(WorkoutContextFormatter.splitsSuffix(for: bare), "")
        XCTAssertEqual(WorkoutContextFormatter.dynamicsSuffix(for: bare), "")
        XCTAssertEqual(WorkoutContextFormatter.line(for: bare, timeZone: utc), base)
        XCTAssertEqual(base, "Workout — 2026-07-04 09:00, 20m00s, 130 kcal (iPhone)")
    }

    func testDetailSuffixCommonFields() {
        var w = run(dynamics: false)
        w.isIndoor = true
        w.effortScore = 6
        w.effortScoreIsUserRated = true
        w.averageMETs = 7.2
        w.weatherTemperatureC = 18
        w.weatherHumidityPercent = 60
        w.stepCount = nil
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: w),
                       ", indoor, effort 6/10 (rated), avg METs 7.2, temp 18 C, humidity 60%"
                       + ", pace 5:38/km (computed)")
        w.isIndoor = false
        w.effortScoreIsUserRated = false
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: w)
            .hasPrefix(", outdoor, effort 6/10 (est), "))
    }

    /// The fixture swim: every `SwimDetail` field set, asserted verbatim.
    private func fullSwim() -> WorkoutSummary {
        WorkoutSummary(activityName: "Swim", start: date(2026, 7, 4, 6, 30), duration: 3300,
                       distanceMeters: 1650, activeEnergyKcal: 430,
                       averageHeartRateBPM: 132, maxHeartRateBPM: 158,
                       source: "Apple Watch", productType: "Watch7,5",
                       isIndoor: true, averageMETs: 7.2,
                       effortScore: 6, effortScoreIsUserRated: true,
                       weatherTemperatureC: 18, weatherHumidityPercent: 60,
                       swim: SwimDetail(lapLengthM: 25, location: .pool, lapCount: 66,
                                        strokeCount: 1840, swimSeconds: 2890,
                                        averageSWOLF: 52, waterTemperatureC: 27.5,
                                        lapsByStroke: ["freestyle": 60, "breaststroke": 6]))
    }

    func testFullSwimLineRendersExactly() {
        XCTAssertEqual(WorkoutContextFormatter.line(for: fullSwim(), timeZone: utc),
            "Swim — 2026-07-04 06:30, 55m00s, 1650 m, 430 kcal, avg HR 132, max HR 158"
            + " (Apple Watch, Watch7,5)"
            + ", indoor, effort 6/10 (rated), avg METs 7.2, temp 18 C, humidity 60%"
            + ", pool 25 m, 66 laps, 1840 strokes, swim time 48m10s"
            + ", pace 2:55/100m (computed, swim time), SWOLF 52, water 27.5 C"
            + ", strokes: freestyle 60, breaststroke 6")
    }

    /// Without lap times the swim pace falls back to the elapsed duration and says
    /// so, and open water has no pool length.
    func testSwimPaceFallsBackToElapsedAndOpenWaterHasNoLength() {
        var w = fullSwim()
        w.swim?.swimSeconds = nil
        w.swim?.location = .openWater
        w.swim?.lapLengthM = nil
        let detail = WorkoutContextFormatter.detailSuffix(for: w)
        XCTAssertTrue(detail.contains(", open water, "))
        XCTAssertTrue(detail.contains("pace 3:20/100m (computed)"),
                      "elapsed 3300s over 16.5 hundreds, and marked plain (computed)")
        XCTAssertFalse(detail.contains("swim time"))
    }

    /// Stroke counts are ordered by lap count then name, so the bytes never depend
    /// on dictionary ordering.
    func testStrokeBreakdownIsSortedByCountThenName() {
        var w = fullSwim()
        w.swim?.lapsByStroke = ["butterfly": 4, "backstroke": 4, "freestyle": 20]
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: w)
            .contains("strokes: freestyle 20, backstroke 4, butterfly 4"))
    }

    /// The fixture run: dynamics, detail and eight splits, asserted verbatim.
    private func fullRun() -> WorkoutSummary {
        WorkoutSummary(activityName: "Run", start: date(2026, 7, 4, 7, 0), duration: 2894,
                       distanceMeters: 8000, activeEnergyKcal: 500,
                       averageHeartRateBPM: 150, maxHeartRateBPM: 172,
                       source: "Apple Watch", averageRunningPowerW: 245,
                       groundContactTimeMs: 240, verticalOscillationCm: 8.1,
                       strideLengthM: 1.15, productType: "Watch7,5", isIndoor: false,
                       averageMETs: 9.4, effortScore: 8, effortScoreIsUserRated: false,
                       elevationAscendedM: 84, elevationDescendedM: 80, stepCount: 7910,
                       weatherTemperatureC: 14, weatherHumidityPercent: 72,
                       splitSecondsPerKm: [358, 361, 364, 359, 360, 362, 357, 363])
    }

    func testFullRunLineRendersExactly() {
        XCTAssertEqual(WorkoutContextFormatter.line(for: fullRun(), timeZone: utc),
            "Run — 2026-07-04 07:00, 48m14s, 8.00 km, 500 kcal, avg HR 150, max HR 172"
            + " (Apple Watch, Watch7,5)"
            + ", power 245 W, GCT 240 ms, vert osc 8.1 cm, stride 1.15 m"
            + ", outdoor, effort 8/10 (est), avg METs 9.4, temp 14 C, humidity 72%"
            + ", pace 6:02/km (computed), cadence 164 spm (computed)"
            + ", ascent 84 m, descent 80 m"
            + ", splits/km 5:58 6:01 6:04 5:59 6:00 6:02 5:57 6:03")
    }

    // MARK: - Splits suffix

    func testSplitsSuffixEmptyAndCapped() {
        var w = fullRun()
        w.splitSecondsPerKm = nil
        XCTAssertEqual(WorkoutContextFormatter.splitsSuffix(for: w), "")
        w.splitSecondsPerKm = []
        XCTAssertEqual(WorkoutContextFormatter.splitsSuffix(for: w), "")
        w.splitSecondsPerKm = Array(repeating: 361, count: 42)
        let shown = WorkoutContextFormatter.splitsSuffix(for: w)
            .replacingOccurrences(of: ", splits/km ", with: "")
            .split(separator: " ")
        XCTAssertEqual(shown.count, WorkoutContextFormatter.maxSplits, "capped at 30")
    }

    /// Pace and cadence are arithmetic done here, not device readings, and the
    /// block says so — the marker is what lets an agent tell the two apart.
    func testComputedFieldsAreMarkedComputed() {
        let detail = WorkoutContextFormatter.detailSuffix(for: fullRun())
        XCTAssertTrue(detail.contains("pace 6:02/km (computed)"))
        XCTAssertTrue(detail.contains("cadence 164 spm (computed)"))
        XCTAssertFalse(detail.contains("ascent 84 m (computed)"), "a read value is unmarked")
    }

    // MARK: - Humidity normalization

    /// The provider assumed `HKUnit.percent()` humidity metadata was the documented
    /// 0…1 fraction and multiplied it by 100, so Apple's Workout app — which stores
    /// it as 0…100 — rendered `humidity 6700%` on a real device. The normalizer takes
    /// both conventions, with 1.0 as the boundary, and rejects what cannot be a
    /// humidity so the segment is omitted rather than printed wrong.
    func testHumidityAcceptsBothPercentAndFractionConventionsAndRejectsTheRest() {
        // Apple's Workout app: already a percent.
        XCTAssertEqual(WorkoutContextFormatter.humidityPercent(fromRaw: 67), 67)
        // The documented unit: a fraction.
        XCTAssertEqual(WorkoutContextFormatter.humidityPercent(fromRaw: 0.67), 67)
        // The boundary itself reads as a saturated fraction, not 1%.
        XCTAssertEqual(WorkoutContextFormatter.humidityPercent(fromRaw: 1.0), 100)
        // Both ends of the range survive.
        XCTAssertEqual(WorkoutContextFormatter.humidityPercent(fromRaw: 0), 0)
        XCTAssertEqual(WorkoutContextFormatter.humidityPercent(fromRaw: 100), 100)
        // The bug's own output: a percent that was multiplied by 100 again.
        XCTAssertNil(WorkoutContextFormatter.humidityPercent(fromRaw: 6700))
        XCTAssertNil(WorkoutContextFormatter.humidityPercent(fromRaw: -1))
        XCTAssertNil(WorkoutContextFormatter.humidityPercent(fromRaw: .nan))
        XCTAssertNil(WorkoutContextFormatter.humidityPercent(fromRaw: .infinity))
    }

    /// End to end through the renderer: whichever convention the recording app used,
    /// the workout line carries the same real humidity, in the existing format.
    func testNormalizedHumidityRendersTheSameFromEitherConvention() {
        var asPercent = run(dynamics: false)
        asPercent.stepCount = nil
        asPercent.weatherTemperatureC = 21
        asPercent.weatherHumidityPercent =
            WorkoutContextFormatter.humidityPercent(fromRaw: 67)
        var asFraction = asPercent
        asFraction.weatherHumidityPercent =
            WorkoutContextFormatter.humidityPercent(fromRaw: 0.67)

        let detail = WorkoutContextFormatter.detailSuffix(for: asPercent)
        XCTAssertEqual(detail, ", temp 21 C, humidity 67%, pace 5:38/km (computed)")
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: asFraction), detail)

        // Implausible data renders nothing at all, never a placeholder.
        var rejected = asPercent
        rejected.weatherHumidityPercent =
            WorkoutContextFormatter.humidityPercent(fromRaw: 6700)
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: rejected),
                       ", temp 21 C, pace 5:38/km (computed)")
    }

    /// A cycle is neither a swim nor a foot distance, so it gets no pace, no cadence
    /// and — deliberately, per the rendering spec, which scopes ascent/descent to
    /// runs, walks and hikes — no elevation either, even when HealthKit recorded it.
    /// Only the conditions common to every workout render.
    func testNonFootNonSwimActivityGetsOnlyTheCommonSegments() {
        var c = WorkoutSummary(activityName: "Cycle", start: date(2026, 7, 4, 7, 0),
                               duration: 3600, distanceMeters: 30000, source: "Apple Watch")
        c.stepCount = 500
        c.elevationAscendedM = 320
        c.isIndoor = false
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: c), ", outdoor")
    }

    // MARK: - Route elevation reducer

    /// Deterministic jitter in [-amplitude, +amplitude]: a fixed linear congruential
    /// sequence, so the profile, and the asserted band, never move between runs.
    private func jitter(count: Int, amplitude: Double, seed: UInt64 = 42) -> [Double] {
        var state = seed
        return (0..<count).map { _ in
            state = state &* 6364136223846793005 &+ 1442695040888963407
            let unit = Double(state >> 11) / Double(1 << 53)  // [0, 1)
            return (unit * 2 - 1) * amplitude
        }
    }

    private func samples(_ altitudes: [Double], accuracy: Double = 3) -> [AltitudeSample] {
        altitudes.map { AltitudeSample(altitude: $0, verticalAccuracy: accuracy) }
    }

    /// The 2026-10-05 shape: out from 284 m to 354 m and back, about one reading a
    /// second, with up to 1.5 m of noise, clamped so the true extremes are the
    /// recorded ones. The climb is 70 m each way. A confirmed leg is counted to its
    /// true peak, so the hysteresis costs nothing here; only the noise at the two
    /// ends can move the total, and by no more than its amplitude at each end.
    func testOutAndBackProfileYieldsTheClimbAndTheRange() throws {
        let up = stride(from: 284.0, through: 354.0, by: 0.1).map { $0 }
        let profile = up + up.reversed()
        let noise = jitter(count: profile.count, amplitude: 1.5)
        let noisy = zip(profile, noise).map { min(354, max(284, $0 + $1)) }
        let r = try XCTUnwrap(RouteElevationReducer.reduce(samples(noisy)))
        XCTAssertEqual(r.minAltitudeM, 284)
        XCTAssertEqual(r.maxAltitudeM, 354)
        XCTAssertEqual(r.ascentM, 70, accuracy: 3)
        XCTAssertEqual(r.descentM, 70, accuracy: 3)
    }

    /// Rolling ground climbs more than its range, as the real run did (100 m of
    /// climb inside a 70 m range): 284 up to 320, down to 300, up to 354, back to
    /// 284 is 36 + 54 = 90 m of climb and 20 + 70 = 90 m of drop.
    func testRollingProfileCountsEveryConfirmedClimb() throws {
        func leg(_ from: Double, _ to: Double) -> [Double] {
            let step = from < to ? 0.2 : -0.2
            return stride(from: from, to: to, by: step).map { $0 }
        }
        let profile = leg(284, 320) + leg(320, 300) + leg(300, 354) + leg(354, 284) + [284]
        let r = try XCTUnwrap(RouteElevationReducer.reduce(samples(profile)))
        XCTAssertEqual(r.ascentM, 90, accuracy: 0.5)
        XCTAssertEqual(r.descentM, 90, accuracy: 0.5)
        XCTAssertEqual(r.minAltitudeM, 284)
        XCTAssertEqual(r.maxAltitudeM, 354, accuracy: 0.2)
    }

    /// Flat ground with plus or minus 2 m of jitter: summed sample to sample this is
    /// hundreds of meters of phantom climb; through the dead band it is nothing.
    func testFlatGroundJitterYieldsNoClimb() throws {
        let flat = jitter(count: 1800, amplitude: 2).map { 300 + $0 }
        let r = try XCTUnwrap(RouteElevationReducer.reduce(samples(flat)))
        XCTAssertLessThan(r.ascentM, 1)
        XCTAssertLessThan(r.descentM, 1)
        var naive = 0.0
        for (a, b) in zip(flat, flat.dropFirst()) where b > a { naive += b - a }
        XCTAssertGreaterThan(naive, 100, "the jitter is real; the band is what removes it")
    }

    /// An invalid reading (negative accuracy) and one worse than the threshold are
    /// dropped before they can touch the range or the climb.
    func testInvalidAndInaccurateReadingsAreIgnored() throws {
        var s = samples(Array(repeating: 300, count: 20))
        s.insert(AltitudeSample(altitude: 900, verticalAccuracy: -1), at: 5)
        s.insert(AltitudeSample(altitude: 10, verticalAccuracy:
            RouteElevationReducer.maxVerticalAccuracyM + 0.5), at: 10)
        s.insert(AltitudeSample(altitude: .nan, verticalAccuracy: 3), at: 15)
        let r = try XCTUnwrap(RouteElevationReducer.reduce(s))
        XCTAssertEqual(r, RouteElevation(ascentM: 0, descentM: 0,
                                         minAltitudeM: 300, maxAltitudeM: 300))
    }

    /// Too few usable readings is not a profile, whatever the raw count.
    func testTooFewUsableReadingsYieldsNil() {
        let n = RouteElevationReducer.minimumUsableSamples
        XCTAssertNil(RouteElevationReducer.reduce([]))
        XCTAssertNil(RouteElevationReducer.reduce(samples(Array(repeating: 300, count: n - 1))))
        XCTAssertNil(RouteElevationReducer.reduce(samples(Array(repeating: 300, count: 50),
                                                          accuracy: -1)))
        XCTAssertNotNil(RouteElevationReducer.reduce(samples(Array(repeating: 300, count: n))))
    }

    // MARK: - Route elevation, time, laps, brand and weather on the line

    private let routeProfile = RouteElevation(ascentM: 100, descentM: 98,
                                              minAltitudeM: 284, maxAltitudeM: 354)

    /// The 2026-10-05 Runna run, as HealthKit holds it: no elevation metadata, a
    /// route, five lap events, workout time 29:26 and elapsed 29:31.
    private func runnaRun() -> WorkoutSummary {
        WorkoutSummary(activityName: "Run", start: date(2026, 10, 5, 9, 43), duration: 1766,
                       elapsed: 1771, distanceMeters: 5020, activeEnergyKcal: 380,
                       averageHeartRateBPM: 151, maxHeartRateBPM: 170, source: "Runna",
                       productType: "Watch8,1", isIndoor: false, stepCount: 4592,
                       lapCount: 5, routeElevation: routeProfile)
    }

    /// Metadata is a device reading and wins: unmarked, and the route's own climb is
    /// not printed. The range still comes from the route.
    func testMetadataAscentWinsUnmarkedOverTheRoute() {
        var w = runnaRun()
        w.elevationAscendedM = 84
        w.elevationDescendedM = 80
        let detail = WorkoutContextFormatter.detailSuffix(for: w)
        XCTAssertTrue(detail.contains(", ascent 84 m, descent 80 m, "))
        XCTAssertFalse(detail.contains("(route)"))
        XCTAssertFalse(detail.contains("ascent 100"))
        XCTAssertTrue(detail.hasSuffix(", elevation 284 to 354 m"))
    }

    /// With only the route, the climb renders with its basis marker, and the range
    /// follows it.
    func testRouteOnlyElevationRendersMarkedWithTheRange() {
        let detail = WorkoutContextFormatter.detailSuffix(for: runnaRun())
        XCTAssertTrue(detail.hasSuffix(
            ", ascent 100 m (route), descent 98 m (route), elevation 284 to 354 m"))
    }

    /// Workout time renders to the second, past the hour too.
    func testWorkoutTimeRendersToTheSecond() {
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: runnaRun(), timeZone: utc)
            .hasPrefix("Run — 2026-10-05 09:43, 29m26s, "))
        var long = runnaRun()
        long.duration = 3910
        long.elapsed = nil
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: long, timeZone: utc)
            .hasPrefix("Run — 2026-10-05 09:43, 1h05m10s, "))
    }

    /// Elapsed renders only for a real pause: five seconds or more over workout time.
    func testElapsedRendersOnlyPastTheFiveSecondThreshold() {
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: runnaRun())
            .hasPrefix(", outdoor, elapsed 29m31s, "))
        var w = runnaRun()
        w.elapsed = 1768
        XCTAssertFalse(WorkoutContextFormatter.detailSuffix(for: w).contains("elapsed"))
        w.elapsed = nil
        XCTAssertFalse(WorkoutContextFormatter.detailSuffix(for: w).contains("elapsed"))
    }

    /// A run's lap events render as a count; one lap is singular, none is nothing.
    func testRunLapEventsRenderAsACount() {
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: runnaRun()).contains(", 5 laps, "))
        var w = runnaRun()
        w.lapCount = 1
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: w).contains(", 1 lap, "))
        w.lapCount = nil
        XCTAssertFalse(WorkoutContextFormatter.detailSuffix(for: w).contains(" lap"))
    }

    /// The whole 2026-10-05 line, pinned.
    func testRunnaRunLineRendersExactly() {
        XCTAssertEqual(WorkoutContextFormatter.line(for: runnaRun(), timeZone: utc),
            "Run — 2026-10-05 09:43, 29m26s, 5.02 km, 380 kcal, avg HR 151, max HR 170"
            + " (Runna, Watch8,1)"
            + ", outdoor, elapsed 29m31s"
            + ", pace 5:52/km (computed), cadence 156 spm (computed), 5 laps"
            + ", ascent 100 m (route), descent 98 m (route), elevation 284 to 354 m")
    }

    /// The brand joins the attribution only when it says something the source name
    /// does not.
    func testBrandRendersOnlyWhenItDiffersFromTheSource() {
        var w = runnaRun()
        w.brandName = "runna"
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: w, timeZone: utc)
            .hasSuffix(" (Runna, Watch8,1)"))
        w.brandName = "Nike Run Club"
        XCTAssertTrue(WorkoutContextFormatter.baseLine(for: w, timeZone: utc)
            .hasSuffix(" (Runna, Nike Run Club, Watch8,1)"))
    }

    /// The weather condition renders as a word right after the temperature.
    func testWeatherConditionRendersAsAWordNextToTheTemperature() {
        var w = runnaRun()
        w.weatherTemperatureC = 14
        w.weatherHumidityPercent = 72
        w.weatherConditionRawValue = HKWeatherCondition.partlyCloudy.rawValue
        XCTAssertTrue(WorkoutContextFormatter.detailSuffix(for: w)
            .contains(", temp 14 C, partly cloudy, humidity 72%, "))
        w.weatherConditionRawValue = HKWeatherCondition.none.rawValue
        XCTAssertFalse(WorkoutContextFormatter.detailSuffix(for: w).contains("cloudy"))
        var unknown = w
        unknown.weatherConditionRawValue = 999
        w.weatherConditionRawValue = nil
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: unknown),
                       WorkoutContextFormatter.detailSuffix(for: w),
                       "a condition this SDK does not define renders nothing")
    }

    /// The pure table is pinned to the SDK's enum, so a reordering there fails here
    /// rather than mislabeling the weather.
    func testWeatherConditionTableMatchesTheSDK() {
        let sdk: [(HKWeatherCondition, String)] = [
            (.clear, "clear"), (.fair, "fair"), (.partlyCloudy, "partly cloudy"),
            (.mostlyCloudy, "mostly cloudy"), (.cloudy, "cloudy"), (.foggy, "foggy"),
            (.haze, "haze"), (.windy, "windy"), (.blustery, "blustery"), (.smoky, "smoky"),
            (.dust, "dust"), (.snow, "snow"), (.hail, "hail"), (.sleet, "sleet"),
            (.freezingDrizzle, "freezing drizzle"), (.freezingRain, "freezing rain"),
            (.mixedRainAndHail, "rain and hail"), (.mixedRainAndSnow, "rain and snow"),
            (.mixedRainAndSleet, "rain and sleet"), (.mixedSnowAndSleet, "snow and sleet"),
            (.drizzle, "drizzle"), (.scatteredShowers, "scattered showers"),
            (.showers, "showers"), (.thunderstorms, "thunderstorms"),
            (.tropicalStorm, "tropical storm"), (.hurricane, "hurricane"),
            (.tornado, "tornado"),
        ]
        for (condition, word) in sdk {
            XCTAssertEqual(WorkoutContextFormatter.weatherConditionName(condition.rawValue), word)
        }
        XCTAssertNil(WorkoutContextFormatter.weatherConditionName(HKWeatherCondition.none.rawValue))
        XCTAssertEqual(WorkoutContextFormatter.weatherConditionNames.count, sdk.count + 1)
    }

    /// A swim's segments are untouched by the run fields: the dynamics, detail and
    /// splits are byte for byte what `main` rendered (only the base line's duration
    /// gained its seconds), and lap events stay in the swim roll-up.
    func testSwimSegmentsAreByteIdenticalToMain() {
        let w = fullSwim()
        XCTAssertEqual(WorkoutContextFormatter.dynamicsSuffix(for: w)
                       + WorkoutContextFormatter.detailSuffix(for: w)
                       + WorkoutContextFormatter.splitsSuffix(for: w),
            ", indoor, effort 6/10 (rated), avg METs 7.2, temp 18 C, humidity 60%"
            + ", pool 25 m, 66 laps, 1840 strokes, swim time 48m10s"
            + ", pace 2:55/100m (computed, swim time), SWOLF 52, water 27.5 C"
            + ", strokes: freestyle 60, breaststroke 6")
        XCTAssertNil(w.lapCount)
    }

    /// A cycle keeps no elevation even with a route profile in hand: the rendering
    /// scope for elevation is runs, walks and hikes.
    func testCycleStillShowsNoElevationEvenWithARoute() {
        var c = WorkoutSummary(activityName: "Cycle", start: date(2026, 7, 4, 7, 0),
                               duration: 3600, distanceMeters: 30000, source: "Apple Watch")
        c.elevationAscendedM = 320
        c.routeElevation = routeProfile
        c.lapCount = 4
        c.isIndoor = false
        XCTAssertEqual(WorkoutContextFormatter.detailSuffix(for: c), ", outdoor")
    }
}
