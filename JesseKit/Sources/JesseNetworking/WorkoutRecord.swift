import Foundation

/// One workout as STRUCTURED DATA: every figure the phone reads for the per-turn workout
/// line, as its own field with its unit in its name, so the bridge can write it into its own
/// CSV column (`diet-logs/workout-detail.csv`, one row per workout, and
/// `diet-logs/workout-splits.csv`, one row per split) instead of a language model copying a
/// handful of numbers out of prose.
///
/// It travels two ways: as the optional `workout_records` array on `POST /jesse` (beside the
/// prose line, never instead of it; the bridge never puts it in a prompt), and as the body of
/// the backfill endpoint `POST /jesse/diet/workouts`.
///
/// **Unknown is not zero.** Every figure is optional and a nil one is left out of the JSON,
/// so the bridge writes a blank cell, never a 0.
///
/// **Only reduced numbers.** No coordinate, no raw altitude series, no raw heart-rate series:
/// exactly the reduced figures the feed line already shows (route elevation is the four
/// numbers `RouteElevationReducer` keeps).
///
/// **Basis markers.** Wherever the APP computes a value rather than reading it, a `basis`
/// field says so: pace and cadence are `computed`; effort is `rated` (the owner's own) or
/// `estimated` (the watch's); ascent and descent are `metadata` (the recorder wrote them) or
/// `route` (reduced from the GPS route here).
///
/// New fields go at the END of the wire and of the bridge's CSV header, and bump
/// `currentSchemaVersion`.
public struct WorkoutRecord: Codable, Equatable, Sendable {
    /// The version of this shape. 1 is the first.
    public static let currentSchemaVersion = 1

    /// One timed split. `kind` is `km` (per-kilometre splits) today; `lap` is reserved.
    public struct Split: Codable, Equatable, Sendable {
        public var kind: String
        /// 1-based, in order.
        public var index: Int
        public var distanceMetres: Double
        public var durationSeconds: Double

        public init(kind: String = "km", index: Int, distanceMetres: Double,
                    durationSeconds: Double) {
            self.kind = kind
            self.index = index
            self.distanceMetres = distanceMetres
            self.durationSeconds = durationSeconds
        }

        enum CodingKeys: String, CodingKey {
            case kind, index
            case distanceMetres = "distance_m"
            case durationSeconds = "duration_s"
        }
    }

    public var schemaVersion: Int
    /// The HealthKit workout's UUID: the row key.
    public var workoutUUID: String
    /// Start instant, RFC 3339 WITH the offset of `timeZone` (`2026-10-07T07:12:03+02:00`).
    public var start: String
    /// IANA zone the workout was recorded in (`HKMetadataKeyTimeZone`, else the phone's).
    public var timeZone: String?
    /// When the phone read this record, RFC 3339.
    public var capturedAt: String?

    /// Short activity name: `Run`, `Walk`, `Swim`, `Hike`, `Workout`, ...
    public var activity: String
    /// The workout name the recorder wrote (`HKMetadataKeyWorkoutBrandName`), e.g. Runna's
    /// "7.5km Long Run".
    public var workoutName: String?
    /// The recording app's display name and bundle identifier.
    public var sourceName: String?
    public var sourceBundle: String?
    /// The recording device's model identifier, e.g. `Watch7,5`.
    public var deviceProductType: String?
    /// True indoor, false outdoor, nil when the recorder did not say.
    public var indoor: Bool?

    /// Workout time (pauses excluded) and elapsed wall-clock time.
    public var durationSeconds: Double
    public var elapsedSeconds: Double?
    public var distanceMetres: Double?
    public var activeEnergyKcal: Double?
    public var avgHeartRateBpm: Double?
    public var maxHeartRateBpm: Double?

    /// Pace for foot activities, seconds per km (always `computed`).
    public var paceSecondsPerKm: Double?
    public var paceBasis: String?

    // Running dynamics (runs only).
    public var avgRunningPowerWatts: Double?
    public var groundContactTimeMs: Double?
    public var verticalOscillationCm: Double?
    public var strideLengthMetres: Double?

    /// Steps during the workout, and the cadence computed from them (always `computed`).
    public var stepCount: Double?
    public var cadenceSpm: Double?
    public var cadenceBasis: String?

    public var avgMETs: Double?
    /// Effort 1...10 and whether it is `rated` (the owner's) or `estimated` (the watch's).
    public var effortScore: Double?
    public var effortBasis: String?

    /// Ascent and descent, each with its own basis (`metadata` or `route`), because the
    /// recorder may write one and not the other.
    public var ascentMetres: Double?
    public var ascentBasis: String?
    public var descentMetres: Double?
    public var descentBasis: String?
    /// The route's lowest and highest usable altitude (route reduction only).
    public var routeAltitudeMinMetres: Double?
    public var routeAltitudeMaxMetres: Double?

    /// Lap events on a run, walk or hike (swims carry theirs in `swimLapCount`).
    public var lapCount: Int?

    public var weatherTemperatureC: Double?
    public var weatherHumidityPercent: Double?
    /// The weather condition as its word (`partly cloudy`).
    public var weatherCondition: String?

    // Swim detail (swims only).
    /// `pool` or `open_water`.
    public var swimLocation: String?
    public var swimPoolLengthMetres: Double?
    public var swimLapCount: Int?
    public var swimStrokeCount: Double?
    /// Summed lap time, rest excluded.
    public var swimTimeSeconds: Double?
    /// Pace per 100 m: `computed_swim_time` when from the summed lap time, `computed` when
    /// from the workout time.
    public var swimPaceSecondsPer100m: Double?
    public var swimPaceBasis: String?
    public var swimAvgSWOLF: Double?
    public var swimWaterTemperatureC: Double?
    /// Lap count per stroke style (`freestyle`: 60).
    public var swimStrokeStyles: [String: Int]?

    /// Per-km splits, in order.
    public var splits: [Split]?

    public init(workoutUUID: String, start: String, activity: String,
                durationSeconds: Double) {
        self.schemaVersion = Self.currentSchemaVersion
        self.workoutUUID = workoutUUID
        self.start = start
        self.activity = activity
        self.durationSeconds = durationSeconds
    }

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case workoutUUID = "workout_uuid"
        case start
        case timeZone = "time_zone"
        case capturedAt = "captured_at"
        case activity
        case workoutName = "workout_name"
        case sourceName = "source_name"
        case sourceBundle = "source_bundle"
        case deviceProductType = "device_product_type"
        case indoor
        case durationSeconds = "duration_s"
        case elapsedSeconds = "elapsed_s"
        case distanceMetres = "distance_m"
        case activeEnergyKcal = "active_energy_kcal"
        case avgHeartRateBpm = "avg_hr_bpm"
        case maxHeartRateBpm = "max_hr_bpm"
        case paceSecondsPerKm = "pace_s_per_km"
        case paceBasis = "pace_basis"
        case avgRunningPowerWatts = "avg_power_w"
        case groundContactTimeMs = "ground_contact_ms"
        case verticalOscillationCm = "vertical_oscillation_cm"
        case strideLengthMetres = "stride_length_m"
        case stepCount = "step_count"
        case cadenceSpm = "cadence_spm"
        case cadenceBasis = "cadence_basis"
        case avgMETs = "avg_mets"
        case effortScore = "effort_score"
        case effortBasis = "effort_basis"
        case ascentMetres = "ascent_m"
        case ascentBasis = "ascent_basis"
        case descentMetres = "descent_m"
        case descentBasis = "descent_basis"
        case routeAltitudeMinMetres = "route_altitude_min_m"
        case routeAltitudeMaxMetres = "route_altitude_max_m"
        case lapCount = "lap_count"
        case weatherTemperatureC = "weather_temp_c"
        case weatherHumidityPercent = "weather_humidity_percent"
        case weatherCondition = "weather_condition"
        case swimLocation = "swim_location"
        case swimPoolLengthMetres = "swim_pool_length_m"
        case swimLapCount = "swim_lap_count"
        case swimStrokeCount = "swim_stroke_count"
        case swimTimeSeconds = "swim_time_s"
        case swimPaceSecondsPer100m = "swim_pace_s_per_100m"
        case swimPaceBasis = "swim_pace_basis"
        case swimAvgSWOLF = "swim_avg_swolf"
        case swimWaterTemperatureC = "swim_water_temp_c"
        case swimStrokeStyles = "swim_stroke_styles"
        case splits
    }
}

/// The body of `POST /jesse/diet/workouts` (the backfill).
public struct WorkoutRecordUpload: Encodable, Equatable, Sendable {
    public var records: [WorkoutRecord]
    public init(records: [WorkoutRecord]) { self.records = records }
}

/// What the bridge did with one backfill page.
public struct WorkoutRecordUpsertResult: Decodable, Equatable, Sendable {
    /// Workout rows added or changed.
    public var written: Int
    /// Workout rows already identical.
    public var unchanged: Int
    public init(written: Int, unchanged: Int) {
        self.written = written
        self.unchanged = unchanged
    }
}

extension JesseBridgeClient {
    /// The most records one backfill page carries. The bridge refuses more.
    public static let workoutRecordPageSize = 50

    /// `POST /jesse/diet/workouts`: upsert one page of records into the Studio's workout
    /// detail ledger. Starts no model turn. Throws on a transport, auth or HTTP failure.
    public func postWorkoutRecords(_ records: [WorkoutRecord]) async throws
        -> WorkoutRecordUpsertResult {
        guard !records.isEmpty else { return WorkoutRecordUpsertResult(written: 0, unchanged: 0) }
        guard var req = todayRequest("/jesse/diet/workouts", method: "POST") else {
            throw JesseError.notConfigured
        }
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try Self.encodeBody(WorkoutRecordUpload(records: records))
        let (data, http) = try await todaySend(req)
        guard (200..<300).contains(http.statusCode) else {
            throw JesseError.badResponse(http.statusCode, String(data: data, encoding: .utf8) ?? "")
        }
        return try JSONDecoder().decode(WorkoutRecordUpsertResult.self, from: data)
    }
}
