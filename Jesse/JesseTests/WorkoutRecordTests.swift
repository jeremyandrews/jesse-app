import XCTest
import JesseNetworking
@testable import Jesse

/// The structured `WorkoutRecord` is built from the SAME `WorkoutSummary` as the feed line,
/// and must carry every figure the line renders. These tests pin that parity for a fully
/// populated run, walk and swim, the basis markers, the wire keys, and the backfill paging.
@MainActor
final class WorkoutRecordTests: XCTestCase {

    private let utc = TimeZone(identifier: "UTC")!
    private func date(_ y: Int, _ mo: Int, _ d: Int, _ h: Int, _ mi: Int) -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utc
        return cal.date(from: DateComponents(year: y, month: mo, day: d, hour: h, minute: mi))!
    }

    // MARK: Fixtures (every field the line can render)

    private func fullRun() -> WorkoutSummary {
        WorkoutSummary(activityName: "Run", start: date(2026, 10, 7, 5, 12), duration: 2894,
                       elapsed: 2950, distanceMeters: 7500, activeEnergyKcal: 500,
                       averageHeartRateBPM: 150, maxHeartRateBPM: 172,
                       source: "Runna", brandName: "7.5km Long Run",
                       averageRunningPowerW: 245, groundContactTimeMs: 240,
                       verticalOscillationCm: 8.1, strideLengthM: 1.15,
                       productType: "Watch7,5", isIndoor: false, averageMETs: 9.4,
                       effortScore: 8, effortScoreIsUserRated: false,
                       elevationAscendedM: 84, elevationDescendedM: nil, stepCount: 7910,
                       weatherTemperatureC: 14, weatherHumidityPercent: 72,
                       weatherConditionRawValue: 3, lapCount: 5,
                       routeElevation: RouteElevation(ascentM: 90, descentM: 80,
                                                      minAltitudeM: 12, maxAltitudeM: 61),
                       splitSecondsPerKm: [358, 361, 364, 359, 360, 362, 357],
                       workoutUUID: "6F1C2A4E-0000-4000-8000-000000000001",
                       timeZoneIdentifier: "Europe/Rome", sourceBundle: "com.runna.app")
    }

    private func fullWalk() -> WorkoutSummary {
        WorkoutSummary(activityName: "Walk", start: date(2026, 10, 6, 16, 0), duration: 3600,
                       elapsed: 3700, distanceMeters: 4800, activeEnergyKcal: 250,
                       averageHeartRateBPM: 101, maxHeartRateBPM: 120,
                       source: "Apple Watch", productType: "Watch7,5", isIndoor: false,
                       averageMETs: 3.9, effortScore: 3, effortScoreIsUserRated: true,
                       elevationAscendedM: 40, elevationDescendedM: 38, stepCount: 6100,
                       weatherTemperatureC: 20, weatherHumidityPercent: 55,
                       weatherConditionRawValue: 1, lapCount: 2,
                       routeElevation: RouteElevation(ascentM: 41, descentM: 37,
                                                      minAltitudeM: 3, maxAltitudeM: 30),
                       splitSecondsPerKm: [740, 745, 738, 750],
                       workoutUUID: "6F1C2A4E-0000-4000-8000-000000000002",
                       timeZoneIdentifier: "Europe/London",
                       sourceBundle: "com.apple.health.ABC")
    }

    private func fullSwim() -> WorkoutSummary {
        WorkoutSummary(activityName: "Swim", start: date(2026, 7, 4, 6, 30), duration: 3300,
                       elapsed: 3500, distanceMeters: 1650, activeEnergyKcal: 430,
                       averageHeartRateBPM: 132, maxHeartRateBPM: 158,
                       source: "Apple Watch", brandName: "Masters set",
                       productType: "Watch7,5", isIndoor: true, averageMETs: 7.2,
                       effortScore: 6, effortScoreIsUserRated: true,
                       weatherTemperatureC: 18, weatherHumidityPercent: 60,
                       weatherConditionRawValue: 5,
                       swim: SwimDetail(lapLengthM: 25, location: .pool, lapCount: 66,
                                        strokeCount: 1840, swimSeconds: 2890,
                                        averageSWOLF: 52, waterTemperatureC: 27.5,
                                        lapsByStroke: ["freestyle": 60, "breaststroke": 6]),
                       workoutUUID: "6F1C2A4E-0000-4000-8000-000000000003",
                       timeZoneIdentifier: "Europe/Rome", sourceBundle: "com.apple.health.ABC")
    }

    /// One rendered segment: how to see it in the line, and the record field(s) that must
    /// then be non-nil.
    private struct Segment {
        let name: String
        let inLine: (String, WorkoutSummary) -> Bool
        let inRecord: (WorkoutRecord) -> Bool
    }

    private var segments: [Segment] {
        func has(_ token: String) -> (String, WorkoutSummary) -> Bool { { l, _ in l.contains(token) } }
        return [
            Segment(name: "activity", inLine: { l, s in l.hasPrefix(s.activityName + " — ") },
                    inRecord: { !$0.activity.isEmpty }),
            Segment(name: "start", inLine: { _, _ in true }, inRecord: { !$0.start.isEmpty }),
            Segment(name: "duration", inLine: { _, _ in true }, inRecord: { $0.durationSeconds > 0 }),
            Segment(name: "distance", inLine: { l, s in s.distanceMeters != nil
                        && (l.contains(" km, ") || l.contains(" m, ")) },
                    inRecord: { $0.distanceMetres != nil }),
            Segment(name: "kcal", inLine: has(" kcal"), inRecord: { $0.activeEnergyKcal != nil }),
            Segment(name: "avg HR", inLine: has("avg HR "), inRecord: { $0.avgHeartRateBpm != nil }),
            Segment(name: "max HR", inLine: has("max HR "), inRecord: { $0.maxHeartRateBpm != nil }),
            Segment(name: "source", inLine: { l, s in s.source.map { l.contains("(\($0)") } ?? false },
                    inRecord: { $0.sourceName != nil }),
            Segment(name: "brand", inLine: { l, s in s.brandName.map { l.contains(", \($0)") } ?? false },
                    inRecord: { $0.workoutName != nil }),
            Segment(name: "product", inLine: { l, s in s.productType.map { l.contains($0) } ?? false },
                    inRecord: { $0.deviceProductType != nil }),
            Segment(name: "power", inLine: has("power "), inRecord: { $0.avgRunningPowerWatts != nil }),
            Segment(name: "GCT", inLine: has("GCT "), inRecord: { $0.groundContactTimeMs != nil }),
            Segment(name: "vert osc", inLine: has("vert osc "), inRecord: { $0.verticalOscillationCm != nil }),
            Segment(name: "stride", inLine: has("stride "), inRecord: { $0.strideLengthMetres != nil }),
            Segment(name: "indoor", inLine: { l, _ in l.contains(", indoor") || l.contains(", outdoor") },
                    inRecord: { $0.indoor != nil }),
            Segment(name: "elapsed", inLine: has("elapsed "), inRecord: { $0.elapsedSeconds != nil }),
            Segment(name: "effort", inLine: has("effort "),
                    inRecord: { $0.effortScore != nil && $0.effortBasis != nil }),
            Segment(name: "METs", inLine: has("avg METs "), inRecord: { $0.avgMETs != nil }),
            Segment(name: "temp", inLine: has("temp "), inRecord: { $0.weatherTemperatureC != nil }),
            Segment(name: "condition", inLine: { l, s in
                        s.weatherConditionRawValue.flatMap(WorkoutContextFormatter.weatherConditionName)
                            .map { l.contains(", \($0),") } ?? false },
                    inRecord: { $0.weatherCondition != nil }),
            Segment(name: "humidity", inLine: has("humidity "), inRecord: { $0.weatherHumidityPercent != nil }),
            Segment(name: "pace/km", inLine: has("/km (computed)"),
                    inRecord: { $0.paceSecondsPerKm != nil && $0.paceBasis == "computed" }),
            Segment(name: "cadence", inLine: has("cadence "),
                    inRecord: { $0.cadenceSpm != nil && $0.cadenceBasis == "computed" && $0.stepCount != nil }),
            Segment(name: "foot laps", inLine: { l, s in !s.isSwim && (l.contains(" laps") || l.contains(" lap,")) },
                    inRecord: { $0.lapCount != nil }),
            Segment(name: "ascent", inLine: has("ascent "),
                    inRecord: { $0.ascentMetres != nil && $0.ascentBasis != nil }),
            Segment(name: "descent", inLine: has("descent "),
                    inRecord: { $0.descentMetres != nil && $0.descentBasis != nil }),
            Segment(name: "elevation range", inLine: has("elevation "),
                    inRecord: { $0.routeAltitudeMinMetres != nil && $0.routeAltitudeMaxMetres != nil }),
            Segment(name: "splits", inLine: has("splits/km "),
                    inRecord: { ($0.splits?.isEmpty == false) }),
            Segment(name: "pool", inLine: { l, _ in l.contains(", pool ") || l.contains(", open water") },
                    inRecord: { $0.swimLocation != nil }),
            Segment(name: "pool length", inLine: has(", pool "), inRecord: { $0.swimPoolLengthMetres != nil }),
            Segment(name: "swim laps", inLine: { l, s in s.isSwim && l.contains(" laps") },
                    inRecord: { $0.swimLapCount != nil }),
            Segment(name: "swim strokes", inLine: { l, _ in l.range(of: #"\d strokes"#, options: .regularExpression) != nil },
                    inRecord: { $0.swimStrokeCount != nil }),
            Segment(name: "swim time", inLine: has("swim time "), inRecord: { $0.swimTimeSeconds != nil }),
            Segment(name: "swim pace", inLine: has("/100m"),
                    inRecord: { $0.swimPaceSecondsPer100m != nil && $0.swimPaceBasis != nil }),
            Segment(name: "SWOLF", inLine: has("SWOLF "), inRecord: { $0.swimAvgSWOLF != nil }),
            Segment(name: "water", inLine: has("water "), inRecord: { $0.swimWaterTemperatureC != nil }),
            Segment(name: "stroke styles", inLine: has("strokes: "),
                    inRecord: { ($0.swimStrokeStyles?.isEmpty == false) }),
        ]
    }

    /// Every segment the line renders has a non-nil counterpart in the record.
    private func assertParity(_ s: WorkoutSummary, expectRendered: Set<String>,
                              file: StaticString = #filePath, line: UInt = #line) throws {
        let rendered = WorkoutContextFormatter.line(for: s, timeZone: utc)
        let record = try XCTUnwrap(WorkoutRecordBuilder.record(for: s, timeZone: utc), file: file, line: line)
        var seen: Set<String> = []
        for seg in segments where seg.inLine(rendered, s) {
            seen.insert(seg.name)
            XCTAssertTrue(seg.inRecord(record),
                          "\(s.activityName): '\(seg.name)' is in the line but not the record\n\(rendered)",
                          file: file, line: line)
        }
        // The fixture really is fully populated: every segment we expect actually rendered.
        XCTAssertTrue(expectRendered.isSubset(of: seen),
                      "\(s.activityName): did not render \(expectRendered.subtracting(seen).sorted())\n\(rendered)",
                      file: file, line: line)
    }

    private let common: Set<String> = ["activity", "start", "duration", "distance", "kcal",
                                       "avg HR", "max HR", "source", "product", "indoor",
                                       "elapsed", "effort", "METs", "temp", "condition", "humidity"]

    func testEveryFigureInARunLineIsInItsRecord() throws {
        try assertParity(fullRun(), expectRendered: common.union([
            "brand", "power", "GCT", "vert osc", "stride", "pace/km", "cadence", "foot laps",
            "ascent", "descent", "elevation range", "splits"]))
    }

    func testEveryFigureInAWalkLineIsInItsRecord() throws {
        try assertParity(fullWalk(), expectRendered: common.union([
            "pace/km", "cadence", "foot laps", "ascent", "descent", "elevation range", "splits"]))
    }

    func testEveryFigureInASwimLineIsInItsRecord() throws {
        try assertParity(fullSwim(), expectRendered: common.union([
            "brand", "pool", "pool length", "swim laps", "swim strokes", "swim time",
            "swim pace", "SWOLF", "water", "stroke styles"]))
    }

    // MARK: Values and basis markers

    func testRunRecordValuesAndBasisMarkers() throws {
        let r = try XCTUnwrap(WorkoutRecordBuilder.record(for: fullRun(), timeZone: utc,
                                                          capturedAt: date(2026, 10, 7, 9, 0)))
        XCTAssertEqual(r.schemaVersion, WorkoutRecord.currentSchemaVersion)
        XCTAssertEqual(r.workoutUUID, "6F1C2A4E-0000-4000-8000-000000000001")
        // The record's own zone wins over the fallback, and the start carries its offset.
        XCTAssertEqual(r.timeZone, "Europe/Rome")
        XCTAssertEqual(r.start, "2026-10-07T07:12:00+02:00")
        XCTAssertEqual(r.capturedAt, "2026-10-07T11:00:00+02:00")
        XCTAssertEqual(r.workoutName, "7.5km Long Run")
        XCTAssertEqual(r.sourceName, "Runna")
        XCTAssertEqual(r.sourceBundle, "com.runna.app")
        XCTAssertEqual(r.paceSecondsPerKm!, 2894 / 7.5, accuracy: 1e-9)
        XCTAssertEqual(r.cadenceSpm!, 7910 / (2894 / 60), accuracy: 1e-9)
        XCTAssertEqual(r.effortBasis, "estimated")
        // Metadata ascent wins; the route fills the missing descent and says so.
        XCTAssertEqual(r.ascentMetres, 84); XCTAssertEqual(r.ascentBasis, "metadata")
        XCTAssertEqual(r.descentMetres, 80); XCTAssertEqual(r.descentBasis, "route")
        XCTAssertEqual(r.weatherCondition, "partly cloudy")
        XCTAssertEqual(r.splits?.count, 7)
        XCTAssertEqual(r.splits?.first, WorkoutRecord.Split(kind: "km", index: 1,
                                                             distanceMetres: 1000,
                                                             durationSeconds: 358))
        XCTAssertNil(r.swimLapCount)
    }

    func testSwimPaceBasisSaysWhichTimeItUsed() throws {
        var s = fullSwim()
        XCTAssertEqual(WorkoutRecordBuilder.record(for: s)?.swimPaceBasis, "computed_swim_time")
        s.swim?.swimSeconds = nil
        XCTAssertEqual(WorkoutRecordBuilder.record(for: s)?.swimPaceBasis, "computed")
        XCTAssertEqual(WorkoutRecordBuilder.record(for: s)?.swimLocation, "pool")
    }

    func testAbsentFiguresStayAbsentNeverZero() throws {
        let bare = WorkoutSummary(activityName: "Workout", start: date(2026, 7, 4, 6, 30),
                                  duration: 600, workoutUUID: "X")
        let r = try XCTUnwrap(WorkoutRecordBuilder.record(for: bare, timeZone: utc))
        XCTAssertEqual(r.start, "2026-07-04T06:30:00Z")
        XCTAssertNil(r.distanceMetres)
        XCTAssertNil(r.paceSecondsPerKm)
        XCTAssertNil(r.splits)
        let json = String(data: try JesseBridgeClient.encodeBody(r), encoding: .utf8)!
        XCTAssertFalse(json.contains("distance_m"), "a nil figure is left out of the JSON")
        XCTAssertFalse(json.contains("null"))
    }

    func testASummaryWithoutAUUIDHasNoRecord() {
        var s = fullRun()
        s.workoutUUID = nil
        XCTAssertNil(WorkoutRecordBuilder.record(for: s))
        XCTAssertEqual(WorkoutRecordBuilder.records(for: [s, fullWalk()]).count, 1)
    }

    /// No coordinate, latitude, longitude or raw series key exists on the wire.
    func testTheWireCarriesNoPositionOrRawSeries() throws {
        let json = String(data: try JesseBridgeClient.encodeBody(
            WorkoutRecordBuilder.record(for: fullRun())!), encoding: .utf8)!.lowercased()
        for banned in ["latitude", "longitude", "coordinate", "\"lat", "\"lon", "altitudes",
                       "heart_rate_series", "samples"] {
            XCTAssertFalse(json.contains(banned), banned)
        }
        XCTAssertTrue(json.contains("\"workout_uuid\""))
        XCTAssertTrue(json.contains("\"avg_power_w\":245"))
    }

    // MARK: The request field

    func testRecordsRideOnTheRequestOnlyWhenPresent() throws {
        let rec = WorkoutRecordBuilder.record(for: fullRun())!
        let with = JesseClient.makeRequest(mode: .ask, text: "log my run", sessionId: nil,
                                           conversationId: "c", voice: false,
                                           instructions: nil, floorOverride: nil,
                                           attachments: [], healthContext: "block",
                                           workoutRecords: [rec])
        let body = String(data: try JesseClient.encodeBody(with), encoding: .utf8)!
        XCTAssertTrue(body.contains("\"workout_records\":[{"))
        let without = JesseClient.makeRequest(mode: .ask, text: "hi", sessionId: nil,
                                              conversationId: "c", voice: false,
                                              instructions: nil, floorOverride: nil,
                                              attachments: [], workoutRecords: [])
        XCTAssertFalse(String(data: try JesseClient.encodeBody(without), encoding: .utf8)!
            .contains("workout_records"))
    }

    /// The records describe exactly the workouts whose lines made it into the block.
    func testRenderedWorkoutsAreExactlyTheLinesInTheBlock() {
        let now = date(2026, 10, 7, 12, 0)
        var stale = fullWalk()
        stale.start = date(2026, 10, 1, 8, 0)   // outside the 48 h window
        let out = HealthContextFormatter.rendered(daily: .empty,
                                                  workouts: [fullWalk(), fullRun(), stale],
                                                  now: now, timeZone: utc)
        XCTAssertNotNil(out.block)
        XCTAssertEqual(out.workouts.map(\.workoutUUID), [fullRun().workoutUUID, fullWalk().workoutUUID],
                       "newest first, the stale one dropped")
        XCTAssertEqual(out.block, HealthContextFormatter.block(daily: .empty,
                                                           workouts: [fullWalk(), fullRun(), stale],
                                                           now: now, timeZone: utc),
                       "the block itself is unchanged")
        XCTAssertEqual(WorkoutRecordBuilder.records(for: out.workouts).count, 2)
    }

    // MARK: Backfill paging

    func testBackfillPagesAreAtMostFifty() {
        let recs = (0..<123).map { i -> WorkoutRecord in
            WorkoutRecord(workoutUUID: "U\(i)", start: "2026-01-01T00:00:00Z",
                          activity: "Run", durationSeconds: 60)
        }
        let pages = WorkoutDetailBackfill.pages(recs)
        XCTAssertEqual(JesseBridgeClient.workoutRecordPageSize, 50)
        XCTAssertEqual(pages.map(\.count), [50, 50, 23])
        XCTAssertEqual(pages.flatMap { $0 }.map(\.workoutUUID), recs.map(\.workoutUUID))
        XCTAssertTrue(WorkoutDetailBackfill.pages([]).isEmpty)
    }
}
