import Foundation

// Recent-workouts rendering — the workouts SUBSECTION of the two-section health
// context (see `HealthContext.swift` for the daily-summary subsection and the
// `HealthContextFormatter` that composes both under the single byte cap).
//
// Everything here is `nonisolated` and deterministic so it is fully unit-tested;
// the ONLY code that touches HealthKit lives behind `HealthContextProviding` in
// `HealthContextProvider.swift`. `WorkoutSummary` is the value type the
// provider fills; `WorkoutContextFormatter` renders one line per workout (a base
// line plus three droppable segments — dynamics, detail, splits) and the
// subsection header. The window/cap/composition live in `HealthContextFormatter`.
//
// The two reducers here (`SwimLapReducer`, `SplitReducer`) turn the plain values
// the provider lifts out of HealthKit into the aggregates the line renders, the
// same way `SleepReducer` is fed: the provider maps HealthKit's shapes into
// Foundation values and decides nothing.

// MARK: - Swim detail

/// Per-workout swim aggregates: what the watch recorded about a pool or open-water
/// session beyond distance and heart rate. Every field is optional — a watch that
/// does not write water temperature simply has no `water` segment, and a swim with
/// no lap events has no lap fields. Rendered inside the droppable detail suffix.
nonisolated struct SwimDetail: Equatable, Sendable {
    /// Pool or open water, from the workout's swimming-location metadata.
    nonisolated enum Location: String, Equatable, Sendable {
        case pool, openWater
        var label: String { self == .pool ? "pool" : "open water" }
    }

    /// Pool length in meters (pool swims only).
    var lapLengthM: Double?
    var location: Location?
    /// Number of lap events the workout carries.
    var lapCount: Int?
    /// Total strokes over the workout.
    var strokeCount: Double?
    /// Summed duration of the lap intervals — swimming time, excluding rest.
    var swimSeconds: TimeInterval?
    /// Mean SWOLF over the laps that carry a score.
    var averageSWOLF: Double?
    /// Water temperature in °C (hardware dependent).
    var waterTemperatureC: Double?
    /// Lap count per stroke name, e.g. `["freestyle": 60, "breaststroke": 6]`.
    /// Rendered sorted by lap count (descending) then name, so the bytes are stable.
    var lapsByStroke: [String: Int]

    init(lapLengthM: Double? = nil, location: Location? = nil, lapCount: Int? = nil,
         strokeCount: Double? = nil, swimSeconds: TimeInterval? = nil,
         averageSWOLF: Double? = nil, waterTemperatureC: Double? = nil,
         lapsByStroke: [String: Int] = [:]) {
        self.lapLengthM = lapLengthM
        self.location = location
        self.lapCount = lapCount
        self.strokeCount = strokeCount
        self.swimSeconds = swimSeconds
        self.averageSWOLF = averageSWOLF
        self.waterTemperatureC = waterTemperatureC
        self.lapsByStroke = lapsByStroke
    }

    /// True when nothing at all was recorded — the provider leaves the whole value
    /// nil in that case so the formatter has nothing to render.
    var isEmpty: Bool {
        lapLengthM == nil && location == nil && lapCount == nil && strokeCount == nil
            && swimSeconds == nil && averageSWOLF == nil && waterTemperatureC == nil
            && lapsByStroke.isEmpty
    }
}

/// One lap of a swim, as plain Foundation values. `HealthContextProvider` maps an
/// `HKWorkoutEvent` of type `.lap` into this and does nothing else; every rule
/// about what the laps MEAN lives in `SwimLapReducer`.
nonisolated struct SwimLap: Equatable, Sendable {
    var start: Date
    var end: Date
    /// `HKSwimmingStrokeStyle` raw value (0…6), or nil when the lap carries none.
    var strokeStyleRawValue: Int?
    /// `HKMetadataKeySWOLFScore` for this lap, or nil when absent.
    var swolf: Double?

    init(start: Date, end: Date, strokeStyleRawValue: Int? = nil, swolf: Double? = nil) {
        self.start = start
        self.end = end
        self.strokeStyleRawValue = strokeStyleRawValue
        self.swolf = swolf
    }
}

/// Reduces a swim's lap events to the aggregates the line renders. Pure and
/// Foundation-only. A lap with no SWOLF still counts toward `lapCount` and
/// `swimSeconds` — a missing score narrows the average's base, it never drops the lap.
nonisolated enum SwimLapReducer {
    /// `HKSwimmingStrokeStyle` raw values 0…6, in the header's order.
    static let strokeNames = ["unknown", "mixed", "freestyle", "backstroke",
                              "breaststroke", "butterfly", "kickboard"]

    /// The stroke name for a raw value, or nil for a value this SDK does not define
    /// (a newer watch writing a style we do not know about is simply uncounted).
    static func strokeName(_ raw: Int) -> String? {
        guard raw >= 0, raw < strokeNames.count else { return nil }
        return strokeNames[raw]
    }

    /// The lap aggregates, or nil when there are no laps (so nothing renders).
    /// `averageSWOLF` is nil unless at least one lap carries a score.
    static func reduce(_ laps: [SwimLap])
        -> (lapCount: Int, swimSeconds: TimeInterval,
            averageSWOLF: Double?, lapsByStroke: [String: Int])? {
        guard !laps.isEmpty else { return nil }

        let seconds = laps.reduce(0.0) { $0 + max(0, $1.end.timeIntervalSince($1.start)) }

        let scores = laps.compactMap(\.swolf)
        let avgSWOLF = scores.isEmpty ? nil : scores.reduce(0, +) / Double(scores.count)

        var byStroke: [String: Int] = [:]
        for lap in laps {
            guard let raw = lap.strokeStyleRawValue, let name = strokeName(raw) else { continue }
            byStroke[name, default: 0] += 1
        }

        return (laps.count, seconds, avgSWOLF, byStroke)
    }
}

// MARK: - Per-km splits

/// One distance sample: meters accrued over a half-open wall-clock interval.
/// `HealthContextProvider` maps the `distanceWalkingRunning` samples associated
/// with a workout into these, in start order, and decides nothing.
nonisolated struct DistanceSample: Equatable, Sendable {
    var start: Date
    var end: Date
    var meters: Double
}

/// Turns an ordered list of distance samples into per-kilometer split times. Pure
/// and Foundation-only.
///
/// Distance is assumed to accrue linearly inside a sample, so a km boundary that
/// falls mid-sample is interpolated rather than attributed to the whole sample.
/// Splits are measured on the WALL CLOCK between boundary instants, so a pause
/// between samples lands in the split that contains it — which is what an elapsed
/// pace means. A final partial kilometer produces no split.
nonisolated enum SplitReducer {
    static let metersPerKm: Double = 1000

    /// Seconds for each completed kilometer, in order. `[]` for empty input or for
    /// a workout that never completes one kilometer.
    static func splitSeconds(_ samples: [DistanceSample]) -> [Double] {
        var splits: [Double] = []
        var cumulative: Double = 0
        var lastBoundary: Date?

        for s in samples {
            let meters = max(0, s.meters)
            guard meters > 0 else { continue }
            let span = max(0, s.end.timeIntervalSince(s.start))
            if lastBoundary == nil { lastBoundary = s.start }

            // Every km boundary strictly inside this sample, in order.
            var consumed: Double = 0
            while true {
                // From the distance reached SO FAR, including what this sample has
                // already contributed — reading it off `cumulative` alone leaves
                // `needed` at zero after the first crossing and never terminates.
                let reached = cumulative + consumed
                let nextBoundary = (floor(reached / metersPerKm) + 1) * metersPerKm
                let needed = nextBoundary - reached
                guard needed <= meters - consumed else { break }
                consumed += needed
                let at = s.start.addingTimeInterval(span * (consumed / meters))
                if let previous = lastBoundary {
                    splits.append(max(0, at.timeIntervalSince(previous)))
                }
                lastBoundary = at
            }
            cumulative += meters
        }
        return splits
    }
}

// MARK: - Route elevation

/// One altitude reading from a workout route, as plain Foundation values.
/// `HealthContextProvider` lifts ONLY these two numbers out of each `CLLocation`;
/// the coordinates never leave the HealthKit boundary, so nothing downstream of
/// this type can log or send a position.
nonisolated struct AltitudeSample: Equatable, Sendable {
    /// Meters above sea level.
    var altitude: Double
    /// `CLLocation.verticalAccuracy` in meters. Negative means the altitude is
    /// invalid (Core Location's own convention).
    var verticalAccuracy: Double
}

/// The four numbers a route's altitude profile is reduced to. Nothing else about
/// the route is kept.
nonisolated struct RouteElevation: Equatable, Sendable {
    /// Cumulative climb, meters.
    var ascentM: Double
    /// Cumulative drop, meters.
    var descentM: Double
    /// Lowest usable altitude, meters.
    var minAltitudeM: Double
    /// Highest usable altitude, meters.
    var maxAltitudeM: Double
}

/// Reduces a route's altitude readings to ascent, descent and range. Pure and
/// Foundation-only, the way `SplitReducer` and `SwimLapReducer` own their rules.
///
/// A third-party recorder (Runna, for one) saves the GPS route but never writes the
/// elevation metadata Apple's Workout app does, so for those workouts the route is
/// the only elevation HealthKit holds.
nonisolated enum RouteElevationReducer {
    /// Readings whose vertical accuracy is worse than this are dropped. An Apple
    /// Watch fuses the barometric altimeter into route altitude and reports a
    /// vertical accuracy of a few meters on a normal outdoor run; GPS-only altitude
    /// sits well above 10 m. 10 m keeps the fused readings and drops the ones that
    /// would put tens of meters of noise into the min and max, which take no
    /// smoothing.
    static let maxVerticalAccuracyM: Double = 10

    /// A change of direction counts only once altitude has come back this far from
    /// the running peak or trough (hysteresis). GPS and barometric jitter on flat
    /// ground is a few meters peak to peak; summed sample to sample it adds tens of
    /// meters of phantom climb to a flat run. 5 m sits above that jitter, and since
    /// a confirmed climb is counted to its true peak, the band costs nothing on a
    /// real hill: it only decides which wiggles are hills.
    static let hysteresisM: Double = 5

    /// Fewer usable readings than this is not a profile: nil, so nothing renders. A
    /// watch records a route point about once a second, so even a five-minute
    /// workout has hundreds; ten is a floor against a route that is mostly invalid.
    static let minimumUsableSamples = 10

    /// The reduced profile, or nil when too few usable readings remain.
    static func reduce(_ samples: [AltitudeSample]) -> RouteElevation? {
        let usable = samples.filter {
            $0.altitude.isFinite && $0.verticalAccuracy.isFinite
                && $0.verticalAccuracy >= 0 && $0.verticalAccuracy <= maxVerticalAccuracyM
        }.map(\.altitude)
        guard usable.count >= minimumUsableSamples, let first = usable.first else { return nil }

        // `anchor` is the last confirmed turning point; `extreme` is the running
        // peak (climbing) or trough (descending) since then. A leg is counted from
        // anchor to extreme once the profile turns back by `hysteresisM`, and the
        // open leg is counted at the end.
        enum Direction { case unknown, up, down }
        var direction = Direction.unknown
        var ascent = 0.0, descent = 0.0
        var anchor = first, extreme = first
        var low = first, high = first
        for a in usable {
            low = min(low, a)
            high = max(high, a)
            switch direction {
            case .unknown:
                if a - anchor >= hysteresisM {
                    direction = .up; extreme = a
                } else if anchor - a >= hysteresisM {
                    direction = .down; extreme = a
                }
            case .up:
                if a > extreme {
                    extreme = a
                } else if extreme - a >= hysteresisM {
                    ascent += extreme - anchor
                    anchor = extreme; extreme = a; direction = .down
                }
            case .down:
                if a < extreme {
                    extreme = a
                } else if a - extreme >= hysteresisM {
                    descent += anchor - extreme
                    anchor = extreme; extreme = a; direction = .up
                }
            }
        }
        switch direction {
        case .up: ascent += extreme - anchor
        case .down: descent += anchor - extreme
        case .unknown: break
        }
        return RouteElevation(ascentM: ascent, descentM: descent,
                              minAltitudeM: low, maxAltitudeM: high)
    }
}

// MARK: - Value type

/// One device-reported workout, reduced to just the fields the block renders.
/// A value type with no HealthKit dependency, so the formatter is pure and the
/// provider seam can be faked in tests. Running-dynamics fields are populated only
/// for running workouts (nil otherwise) and render as a droppable suffix; the
/// detail fields below them are populated per activity and per hardware, and each
/// one absent simply renders nothing.
nonisolated struct WorkoutSummary: Equatable, Sendable {
    /// Short human name for the activity, e.g. "Swim", "Run", "Walk", "Workout".
    var activityName: String
    /// When the workout started (absolute instant).
    var start: Date
    /// Workout time in seconds: `HKWorkout.duration`, which EXCLUDES pauses.
    var duration: TimeInterval
    /// Elapsed wall-clock time in seconds (end minus start, pauses included), or
    /// nil when unknown. Rendered only when it exceeds `duration` by a real pause.
    var elapsed: TimeInterval?
    /// Total distance in METERS, or nil if the activity records none.
    var distanceMeters: Double?
    /// Total active energy in kcal, or nil if unavailable.
    var activeEnergyKcal: Double?
    /// Average heart rate in BPM over the workout, or nil if unavailable.
    var averageHeartRateBPM: Double?
    /// Max heart rate in BPM over the workout, or nil if unavailable.
    var maxHeartRateBPM: Double?
    /// Recording source, e.g. "Apple Watch", or nil if unknown.
    var source: String?
    /// `HKMetadataKeyWorkoutBrandName`, the brand the recorder names for the
    /// workout, or nil. Rendered in the attribution only when it differs from
    /// `source`.
    var brandName: String?

    // Running dynamics — average over the workout window, runs only, each nil when
    // the sample stream is absent. Rendered as a droppable suffix by the formatter.
    /// Average running power in watts.
    var averageRunningPowerW: Double?
    /// Average ground contact time in milliseconds.
    var groundContactTimeMs: Double?
    /// Average vertical oscillation in centimeters.
    var verticalOscillationCm: Double?
    /// Average stride length in meters.
    var strideLengthM: Double?

    // Workout detail — everything else HealthKit already holds about the session.
    // All nil by default; each renders only when present.
    /// The recording device's model identifier, e.g. `Watch7,5`. A hardware change
    /// is then visible in the data itself rather than having to be asserted.
    var productType: String?
    /// True for an indoor workout, false for outdoor, nil when the flag is absent.
    var isIndoor: Bool?
    /// Average METs (kcal/(kg·hr)).
    var averageMETs: Double?
    /// Workout effort, 1…10.
    var effortScore: Double?
    /// True when `effortScore` is the user's own rating, false/nil when it is the
    /// watch's estimate. Only meaningful alongside `effortScore`.
    var effortScoreIsUserRated: Bool?
    /// Cumulative elevation ascended, meters.
    var elevationAscendedM: Double?
    /// Cumulative elevation descended, meters.
    var elevationDescendedM: Double?
    /// Steps taken during the workout (the basis of the computed cadence).
    var stepCount: Double?
    /// Weather temperature during the workout, °C.
    var weatherTemperatureC: Double?
    /// Weather humidity during the workout, as a PERCENT (0…100).
    var weatherHumidityPercent: Double?
    /// `HKMetadataKeyWeatherCondition` as its `HKWeatherCondition` raw value; the
    /// word is chosen by `WorkoutContextFormatter.weatherConditionName`.
    var weatherConditionRawValue: Int?
    /// Lap events on a run, walk or hike (swims keep theirs in `swim`).
    var lapCount: Int?
    /// The route's reduced altitude profile, read only when the workout carries no
    /// ascent metadata. Rendered with a `(route)` basis marker.
    var routeElevation: RouteElevation?
    /// Seconds for each completed kilometer, in order. Rendered as its own
    /// droppable suffix, the first thing the byte cap sheds.
    var splitSecondsPerKm: [Double]?
    /// Swim aggregates (swims only).
    var swim: SwimDetail?

    init(activityName: String, start: Date, duration: TimeInterval,
         elapsed: TimeInterval? = nil,
         distanceMeters: Double? = nil, activeEnergyKcal: Double? = nil,
         averageHeartRateBPM: Double? = nil, maxHeartRateBPM: Double? = nil,
         source: String? = nil, brandName: String? = nil,
         averageRunningPowerW: Double? = nil, groundContactTimeMs: Double? = nil,
         verticalOscillationCm: Double? = nil, strideLengthM: Double? = nil,
         productType: String? = nil, isIndoor: Bool? = nil, averageMETs: Double? = nil,
         effortScore: Double? = nil, effortScoreIsUserRated: Bool? = nil,
         elevationAscendedM: Double? = nil, elevationDescendedM: Double? = nil,
         stepCount: Double? = nil, weatherTemperatureC: Double? = nil,
         weatherHumidityPercent: Double? = nil, weatherConditionRawValue: Int? = nil,
         lapCount: Int? = nil, routeElevation: RouteElevation? = nil,
         splitSecondsPerKm: [Double]? = nil,
         swim: SwimDetail? = nil) {
        self.activityName = activityName
        self.start = start
        self.duration = duration
        self.elapsed = elapsed
        self.distanceMeters = distanceMeters
        self.activeEnergyKcal = activeEnergyKcal
        self.averageHeartRateBPM = averageHeartRateBPM
        self.maxHeartRateBPM = maxHeartRateBPM
        self.source = source
        self.brandName = brandName
        self.averageRunningPowerW = averageRunningPowerW
        self.groundContactTimeMs = groundContactTimeMs
        self.verticalOscillationCm = verticalOscillationCm
        self.strideLengthM = strideLengthM
        self.productType = productType
        self.isIndoor = isIndoor
        self.averageMETs = averageMETs
        self.effortScore = effortScore
        self.effortScoreIsUserRated = effortScoreIsUserRated
        self.elevationAscendedM = elevationAscendedM
        self.elevationDescendedM = elevationDescendedM
        self.stepCount = stepCount
        self.weatherTemperatureC = weatherTemperatureC
        self.weatherHumidityPercent = weatherHumidityPercent
        self.weatherConditionRawValue = weatherConditionRawValue
        self.lapCount = lapCount
        self.routeElevation = routeElevation
        self.splitSecondsPerKm = splitSecondsPerKm
        self.swim = swim
    }

    /// When the workout ended: start plus the elapsed time when known, else plus
    /// the workout time (which undershoots by any pause).
    var end: Date { start.addingTimeInterval(elapsed ?? duration) }

    /// True when any running-dynamics field is present (populated only for runs).
    var hasRunningDynamics: Bool {
        averageRunningPowerW != nil || groundContactTimeMs != nil
            || verticalOscillationCm != nil || strideLengthM != nil
    }

    /// Whether this is a swim, which decides both the distance format (whole meters
    /// at any length, so a per-100m pace is computable without a rounding band) and
    /// which detail segments apply. The provider names every swimming workout
    /// exactly "Swim", so the name is the signal the pure layer has.
    var isSwim: Bool { activityName == "Swim" }

    /// Whether a per-km pace and a step cadence make sense for this activity.
    var isFootDistance: Bool {
        activityName == "Run" || activityName == "Walk" || activityName == "Hike"
    }
}

// MARK: - Formatter (workouts subsection)

/// Renders workout summaries into the "recent workouts" subsection. Pure and
/// locale-fixed (en_US_POSIX dates, metric units, kcal) so the output is
/// byte-deterministic. The window (48h), the newest-first ordering, the 5-cap, and
/// the byte budget live in `HealthContextFormatter`; this type only formats an
/// individual line and the subsection header.
///
/// A line is a base plus three independently droppable segments, in the order the
/// byte cap sheds them from the right: `base + dynamics + detail + splits`.
///
/// Anything this type DERIVES rather than reads — pace, cadence — carries a literal
/// `(computed)` marker, and elevation reduced from the route rather than read from
/// the workout's metadata carries `(route)`, so an agent reading the block can
/// always tell a device reading from arithmetic done here.
nonisolated enum WorkoutContextFormatter {
    static let maxWorkouts = 5
    static let windowHours: Double = 48
    /// Hard cap on rendered splits, so a long run cannot dominate the block.
    static let maxSplits = 30
    /// Elapsed time renders only when it exceeds workout time by at least this many
    /// whole seconds. Below it the gap is the recorder's start/stop bookkeeping, not
    /// a pause, and repeating the duration would cost bytes for nothing.
    static let minPauseSeconds = 5

    /// The subsection header for `count` workout lines (singular/plural).
    static func header(count: Int) -> String {
        "\(count) recent workout\(count == 1 ? "" : "s") from Apple Health "
            + "(last 48h, newest first):"
    }

    /// One workout as a single line with every segment it has.
    /// `HealthContextFormatter` calls the pieces separately so it can shed them
    /// under budget.
    static func line(for s: WorkoutSummary, timeZone: TimeZone = .current) -> String {
        baseLine(for: s, timeZone: timeZone) + dynamicsSuffix(for: s)
            + detailSuffix(for: s) + splitsSuffix(for: s)
    }

    /// The workout line WITHOUT any suffix. Fields that are nil are omitted; the
    /// source, when present, is parenthesized at the end, then the workout's brand
    /// when the recorder named one other than itself, then the recording device's
    /// product type. The duration is workout time to the second.
    static func baseLine(for s: WorkoutSummary, timeZone: TimeZone = .current) -> String {
        var parts = ["\(s.activityName) — \(dateString(s.start, timeZone: timeZone))"]
        parts.append(clockString(s.duration))
        if let d = s.distanceMeters { parts.append(distanceString(d, isSwim: s.isSwim)) }
        if let k = s.activeEnergyKcal { parts.append(energyString(k)) }
        if let avg = s.averageHeartRateBPM { parts.append("avg HR \(bpmString(avg))") }
        if let mx = s.maxHeartRateBPM { parts.append("max HR \(bpmString(mx))") }
        var out = parts.joined(separator: ", ")
        var attribution: [String] = []
        let src = s.source?.trimmingCharacters(in: .whitespaces) ?? ""
        if !src.isEmpty { attribution.append(src) }
        if let brand = s.brandName?.trimmingCharacters(in: .whitespaces), !brand.isEmpty,
           brand.caseInsensitiveCompare(src) != .orderedSame {
            attribution.append(brand)
        }
        if let pt = s.productType, !pt.trimmingCharacters(in: .whitespaces).isEmpty {
            attribution.append(pt)
        }
        if !attribution.isEmpty { out += " (\(attribution.joined(separator: ", ")))" }
        return out
    }

    /// The running-dynamics segment appended to a run's line (leading ", "), or ""
    /// when the workout has no dynamics. Kept separate so the byte-cap can drop it
    /// before dropping whole lines. Each field is omitted when nil.
    static func dynamicsSuffix(for s: WorkoutSummary) -> String {
        var d: [String] = []
        if let p = s.averageRunningPowerW { d.append(String(format: "power %.0f W", p)) }
        if let g = s.groundContactTimeMs { d.append(String(format: "GCT %.0f ms", g)) }
        if let v = s.verticalOscillationCm { d.append(String(format: "vert osc %.1f cm", v)) }
        if let l = s.strideLengthM { d.append(String(format: "stride %.2f m", l)) }
        return d.isEmpty ? "" : ", " + d.joined(separator: ", ")
    }

    /// Everything else HealthKit held about the workout (leading ", "), or "" when
    /// it held nothing. Dropped by the byte cap after the splits and before the
    /// running dynamics. Order is fixed: conditions common to every workout, then
    /// the swim segments, then the foot-distance segments.
    static func detailSuffix(for s: WorkoutSummary) -> String {
        var d: [String] = []

        if let indoor = s.isIndoor { d.append(indoor ? "indoor" : "outdoor") }
        if let elapsed = s.elapsed,
           Int(elapsed.rounded()) - Int(s.duration.rounded()) >= minPauseSeconds {
            d.append("elapsed \(clockString(elapsed))")
        }
        if let e = s.effortScore {
            d.append(String(format: "effort %.0f/10 (%@)", e,
                            s.effortScoreIsUserRated == true ? "rated" : "est"))
        }
        if let m = s.averageMETs { d.append(String(format: "avg METs %.1f", m)) }
        if let t = s.weatherTemperatureC { d.append(String(format: "temp %.0f C", t)) }
        if let raw = s.weatherConditionRawValue, let word = weatherConditionName(raw) {
            d.append(word)
        }
        if let h = s.weatherHumidityPercent { d.append(String(format: "humidity %.0f%%", h)) }

        if let sw = s.swim { d.append(contentsOf: swimSegments(sw, workout: s)) }
        if s.isFootDistance { d.append(contentsOf: footSegments(s)) }

        return d.isEmpty ? "" : ", " + d.joined(separator: ", ")
    }

    /// The per-km splits segment (leading ", "), or "" when there are none. The
    /// first thing the byte cap sheds, because it is the longest and the least
    /// dense. At most `maxSplits` entries.
    static func splitsSuffix(for s: WorkoutSummary) -> String {
        guard let splits = s.splitSecondsPerKm, !splits.isEmpty else { return "" }
        let shown = splits.prefix(maxSplits).map { paceString($0) }
        return ", splits/km " + shown.joined(separator: " ")
    }

    // MARK: Detail segment builders

    private static func swimSegments(_ sw: SwimDetail, workout s: WorkoutSummary) -> [String] {
        var d: [String] = []
        if let loc = sw.location {
            if loc == .pool, let len = sw.lapLengthM {
                d.append(String(format: "pool %.0f m", len))
            } else {
                d.append(loc.label)
            }
        } else if let len = sw.lapLengthM {
            d.append(String(format: "pool %.0f m", len))
        }
        if let laps = sw.lapCount { d.append("\(laps) lap\(laps == 1 ? "" : "s")") }
        if let strokes = sw.strokeCount { d.append(String(format: "%.0f strokes", strokes)) }
        if let secs = sw.swimSeconds { d.append("swim time \(clockString(secs))") }
        // Swim pace prefers the summed lap time (swimming, excluding rest) over the
        // elapsed duration, and says which it used.
        if let meters = s.distanceMeters, meters > 0 {
            let basis = sw.swimSeconds ?? s.duration
            if basis > 0 {
                let per100 = basis / (meters / 100)
                d.append("pace \(paceString(per100))/100m (computed"
                    + (sw.swimSeconds != nil ? ", swim time" : "") + ")")
            }
        }
        if let swolf = sw.averageSWOLF { d.append(String(format: "SWOLF %.0f", swolf)) }
        if let w = sw.waterTemperatureC { d.append(String(format: "water %.1f C", w)) }
        if !sw.lapsByStroke.isEmpty {
            // Sorted by lap count (descending) then name, so the bytes never depend
            // on dictionary ordering.
            let ordered = sw.lapsByStroke
                .sorted { $0.value != $1.value ? $0.value > $1.value : $0.key < $1.key }
                .map { "\($0.key) \($0.value)" }
            d.append("strokes: " + ordered.joined(separator: ", "))
        }
        return d
    }

    private static func footSegments(_ s: WorkoutSummary) -> [String] {
        var d: [String] = []
        if let meters = s.distanceMeters, meters > 0, s.duration > 0 {
            d.append("pace \(paceString(s.duration / (meters / 1000)))/km (computed)")
        }
        if let steps = s.stepCount, s.duration > 0 {
            d.append(String(format: "cadence %.0f spm (computed)", steps / (s.duration / 60)))
        }
        if let laps = s.lapCount, laps > 0 { d.append("\(laps) lap\(laps == 1 ? "" : "s")") }
        // Metadata wins, per value, and is a device reading: unmarked. The route's
        // reduction fills a gap and says so.
        let route = s.routeElevation
        if let up = s.elevationAscendedM {
            d.append(String(format: "ascent %.0f m", up))
        } else if let up = route?.ascentM {
            d.append(String(format: "ascent %.0f m (route)", up))
        }
        if let down = s.elevationDescendedM {
            d.append(String(format: "descent %.0f m", down))
        } else if let down = route?.descentM {
            d.append(String(format: "descent %.0f m (route)", down))
        }
        if let r = route {
            d.append(String(format: "elevation %.0f to %.0f m", r.minAltitudeM, r.maxAltitudeM))
        }
        return d
    }

    // MARK: Weather condition

    /// `HKWeatherCondition` raw values 0…27, in the SDK's order. `none` (0) has no
    /// word: the segment is simply omitted.
    static let weatherConditionNames: [String?] = [
        nil, "clear", "fair", "partly cloudy", "mostly cloudy", "cloudy", "foggy", "haze",
        "windy", "blustery", "smoky", "dust", "snow", "hail", "sleet", "freezing drizzle",
        "freezing rain", "rain and hail", "rain and snow", "rain and sleet",
        "snow and sleet", "drizzle", "scattered showers", "showers", "thunderstorms",
        "tropical storm", "hurricane", "tornado",
    ]

    /// The word for a weather condition raw value, or nil for `none` and for a value
    /// this SDK does not define (a newer recorder's condition is simply omitted).
    static func weatherConditionName(_ raw: Int) -> String? {
        guard raw >= 0, raw < weatherConditionNames.count else { return nil }
        return weatherConditionNames[raw]
    }

    // MARK: Unit normalization

    /// A relative humidity read from workout metadata with `HKUnit.percent()`,
    /// normalized to a percent in 0…100, or nil when the value cannot be one.
    ///
    /// The two conventions in the wild disagree, so the number itself has to decide.
    /// `HKUnit.percent()` is documented as a 0…1 fraction, and a third-party app that
    /// follows the documentation writes `0.67` for 67% humidity; Apple's own Workout
    /// app does not, and stores the value so this read returns `67`. The boundary is
    /// therefore 1.0: at or below it the value is read as a fraction and multiplied by
    /// 100, above it the value is already a percent and passes through. The one case
    /// the rule cannot call is a raw `1.0`, which is 100% as a fraction and 1% as a
    /// percent; it renders 100%, the likelier reading of a saturated recording.
    ///
    /// Anything that cannot be a humidity — negative, NaN, infinite, or a percent
    /// above 100 (a 0…100 value that was multiplied by 100 as well, the bug this
    /// replaces) — returns nil, and the caller omits the segment rather than printing
    /// a placeholder.
    static func humidityPercent(fromRaw raw: Double) -> Double? {
        guard raw.isFinite, raw >= 0 else { return nil }
        let percent = raw <= 1 ? raw * 100 : raw
        return percent <= 100 ? percent : nil
    }

    // MARK: Field formatting (all locale-fixed / C-locale numeric)

    /// `yyyy-MM-dd HH:mm` in en_US_POSIX so the string never shifts by host locale.
    private static func dateString(_ date: Date, timeZone: TimeZone) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = timeZone
        f.dateFormat = "yyyy-MM-dd HH:mm"
        return f.string(from: date)
    }

    /// A duration to the second: `48m10s`, or `1h05m10s` past the hour. Workout
    /// time, elapsed time and swim time all render this way; rounding the workout
    /// time to the minute threw away up to 30 s that pace arithmetic needs.
    private static func clockString(_ seconds: TimeInterval) -> String {
        let total = max(0, Int(seconds.rounded()))
        let h = total / 3600, m = (total % 3600) / 60, s = total % 60
        if h > 0 { return String(format: "%dh%02dm%02ds", h, m, s) }
        return String(format: "%dm%02ds", m, s)
    }

    /// `m:ss` for a pace or split. Minutes are not capped at 60 — a very slow
    /// kilometer reads `72:30` rather than silently wrapping.
    private static func paceString(_ seconds: TimeInterval) -> String {
        let total = max(0, Int(seconds.rounded()))
        return String(format: "%d:%02d", total / 60, total % 60)
    }

    /// Metric distance. A swim prints WHOLE METERS at any length (`1650 m`) because
    /// a per-100m pace computed off a rounded kilometer carries a double-digit
    /// second error; everything else prints two decimals of a kilometer (`7.84 km`),
    /// or whole meters under 1 km. `%.Nf` is C-locale (period decimal), so no
    /// locale drift.
    private static func distanceString(_ meters: Double, isSwim: Bool) -> String {
        if isSwim { return String(format: "%.0f m", meters) }
        if meters >= 1000 { return String(format: "%.2f km", meters / 1000) }
        return String(format: "%.0f m", meters)
    }

    private static func energyString(_ kcal: Double) -> String {
        String(format: "%.0f kcal", kcal)
    }

    private static func bpmString(_ bpm: Double) -> String {
        String(format: "%.0f", bpm)
    }
}
