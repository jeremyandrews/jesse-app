import Foundation

// The health archive's wire contract: the body of `POST /jesse/health/ingest` (bridge 0.169.0
// and later), which writes every HealthKit sample the phone sends into Jeremy's own Postgres
// archive. The bridge validates exactly this shape and refuses the whole batch on any
// mismatch, so the types here are the contract, not a convenience.
//
// Pure Foundation: the app lifts each HealthKit object into these values, and everything
// that decides what a request looks like (batching, series numbering, metadata, timestamps,
// route thinning) is here, where it is tested without HealthKit.
//
// What this module never does is log. A request carries values, and on a route request
// coordinates, which go to the ingest endpoint and nowhere else.

/// The four kinds of request.
public enum HealthArchiveKind: String, Codable, Sendable {
    case quantity, category, workout, route
}

/// A JSON-safe metadata value. Anything HealthKit stores that is not one of these is
/// converted (`HealthArchiveMetadata`) or dropped.
public enum HealthArchiveValue: Equatable, Sendable, Encodable {
    case number(Double)
    case string(String)
    case bool(Bool)
    case object([String: HealthArchiveValue])

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .bool(let b): try c.encode(b)
        case .object(let o): try c.encode(o)
        }
    }
}

/// RFC 3339 timestamps, always with a numeric offset (`+00:00`, never `Z`) and milliseconds,
/// in the device's time zone. HealthKit dates carry sub-second precision, and a series of
/// heart-rate points a second apart must not collapse onto one instant.
public enum HealthArchiveTime {
    public static func string(_ date: Date, timeZone: TimeZone = .current) -> String {
        Formatter(timeZone: timeZone).string(date)
    }

    /// A reusable formatter for the hot loops (a long route is 100000 points). Unchecked
    /// Sendable because `DateFormatter` is documented thread-safe for formatting, and the
    /// formatter is never mutated after `init`.
    public final class Formatter: @unchecked Sendable {
        private let formatter: DateFormatter
        public init(timeZone: TimeZone = .current) {
            let f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.calendar = Calendar(identifier: .gregorian)
            f.timeZone = timeZone
            // `xxx` is always a numeric offset; `XXX` would write `Z` at UTC.
            f.dateFormat = "yyyy-MM-dd'T'HH:mm:ss.SSSxxx"
            formatter = f
        }
        public func string(_ date: Date) -> String { formatter.string(from: date) }
    }
}

/// One quantity sample, or one point of a quantity series (`seq` 0, 1, 2… under the series
/// sample's UUID; an ordinary sample is `seq` 0).
public struct HealthArchiveQuantitySample: Encodable, Equatable, Sendable {
    public var uuid: String
    public var seq: Int
    public var start: String
    public var end: String
    public var value: Double
    public var unit: String
    public var source: String
    public var sourceName: String
    public var device: String?
    public var metadata: [String: HealthArchiveValue]

    public init(uuid: String, seq: Int = 0, start: String, end: String, value: Double,
                unit: String, source: String, sourceName: String, device: String?,
                metadata: [String: HealthArchiveValue] = [:]) {
        self.uuid = uuid; self.seq = seq; self.start = start; self.end = end
        self.value = value; self.unit = unit; self.source = source
        self.sourceName = sourceName; self.device = device; self.metadata = metadata
    }

    enum CodingKeys: String, CodingKey {
        case uuid, seq, start, end, value, unit, source, device, metadata
        case sourceName = "source_name"
    }
}

/// One category sample: an integer value and no unit.
public struct HealthArchiveCategorySample: Encodable, Equatable, Sendable {
    public var uuid: String
    public var start: String
    public var end: String
    public var value: Int
    public var source: String
    public var sourceName: String
    public var device: String?
    public var metadata: [String: HealthArchiveValue]

    public init(uuid: String, start: String, end: String, value: Int, source: String,
                sourceName: String, device: String?, metadata: [String: HealthArchiveValue] = [:]) {
        self.uuid = uuid; self.start = start; self.end = end; self.value = value
        self.source = source; self.sourceName = sourceName; self.device = device
        self.metadata = metadata
    }

    enum CodingKeys: String, CodingKey {
        case uuid, start, end, value, source, device, metadata
        case sourceName = "source_name"
    }
}

/// One workout event. `type` is one of the six words the bridge accepts.
public struct HealthArchiveWorkoutEvent: Encodable, Equatable, Sendable {
    public enum Kind: String, Encodable, Sendable {
        case lap, segment, pause, resume, marker, other
    }
    public var type: Kind
    public var start: String
    public var end: String
    public var metadata: [String: HealthArchiveValue]

    public init(type: Kind, start: String, end: String, metadata: [String: HealthArchiveValue] = [:]) {
        self.type = type; self.start = start; self.end = end; self.metadata = metadata
    }
}

/// One workout, with its statistics and events.
public struct HealthArchiveWorkout: Encodable, Equatable, Sendable {
    public var uuid: String
    public var start: String
    public var end: String
    public var durationS: Double
    public var activityType: Int
    public var activityName: String
    public var distanceM: Double?
    public var energyKcal: Double?
    public var avgHr: Double?
    public var maxHr: Double?
    public var effort: Double?
    public var indoor: Bool?
    public var source: String
    public var sourceName: String
    public var device: String?
    public var metadata: [String: HealthArchiveValue]
    public var events: [HealthArchiveWorkoutEvent]

    public init(uuid: String, start: String, end: String, durationS: Double, activityType: Int,
                activityName: String, distanceM: Double? = nil, energyKcal: Double? = nil,
                avgHr: Double? = nil, maxHr: Double? = nil, effort: Double? = nil,
                indoor: Bool? = nil, source: String, sourceName: String, device: String?,
                metadata: [String: HealthArchiveValue] = [:],
                events: [HealthArchiveWorkoutEvent] = []) {
        self.uuid = uuid; self.start = start; self.end = end; self.durationS = durationS
        self.activityType = activityType; self.activityName = activityName
        self.distanceM = distanceM; self.energyKcal = energyKcal; self.avgHr = avgHr
        self.maxHr = maxHr; self.effort = effort; self.indoor = indoor; self.source = source
        self.sourceName = sourceName; self.device = device; self.metadata = metadata
        self.events = events
    }

    enum CodingKeys: String, CodingKey {
        case uuid, start, end, effort, indoor, source, device, metadata, events
        case durationS = "duration_s"
        case activityType = "activity_type"
        case activityName = "activity_name"
        case distanceM = "distance_m"
        case energyKcal = "energy_kcal"
        case avgHr = "avg_hr"
        case maxHr = "max_hr"
        case sourceName = "source_name"
    }
}

/// One `CLLocation` of a route, as plain numbers. CoreLocation marks an invalid accuracy,
/// speed or course with a negative value; those travel as JSON `null`, and so does the
/// altitude when its vertical accuracy is invalid, because the altitude is then meaningless.
///
/// Values are rounded on the way in, to keep a 100000-point route well inside the bridge's
/// 16 MB route body: 7 decimals of a degree is about a centimetre, and 2 decimals of the
/// metre-scale fields is a centimetre or a hundredth of a degree.
public struct HealthArchiveRoutePoint: Encodable, Equatable, Sendable {
    public var t: String
    public var lat: Double
    public var lon: Double
    public var alt: Double?
    public var hAcc: Double?
    public var vAcc: Double?
    public var speed: Double?
    public var course: Double?

    /// nil when the coordinate itself is unusable (outside the globe or not finite): the
    /// bridge refuses the WHOLE route for one such point, so it is dropped here instead.
    public init?(t: String, lat: Double, lon: Double, altitude: Double,
                 horizontalAccuracy: Double, verticalAccuracy: Double,
                 speed: Double, course: Double) {
        guard lat.isFinite, lon.isFinite, (-90...90).contains(lat), (-180...180).contains(lon) else {
            return nil
        }
        self.t = t
        self.lat = Self.round(lat, 7)
        self.lon = Self.round(lon, 7)
        self.vAcc = Self.valid(verticalAccuracy)
        self.alt = vAcc == nil || !altitude.isFinite ? nil : Self.round(altitude, 2)
        self.hAcc = Self.valid(horizontalAccuracy)
        self.speed = Self.valid(speed)
        self.course = Self.valid(course)
    }

    private static func valid(_ v: Double) -> Double? {
        v.isFinite && v >= 0 ? round(v, 2) : nil
    }

    private static func round(_ v: Double, _ places: Int) -> Double {
        let scale = pow(10.0, Double(places))
        return (v * scale).rounded() / scale
    }

    enum CodingKeys: String, CodingKey {
        case t, lat, lon, alt, speed, course
        case hAcc = "h_acc"
        case vAcc = "v_acc"
    }

    /// Explicit nulls, as the contract states, rather than the synthesized omission.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(t, forKey: .t)
        try c.encode(lat, forKey: .lat)
        try c.encode(lon, forKey: .lon)
        try c.encode(alt, forKey: .alt)
        try c.encode(hAcc, forKey: .hAcc)
        try c.encode(vAcc, forKey: .vAcc)
        try c.encode(speed, forKey: .speed)
        try c.encode(course, forKey: .course)
    }
}

/// One workout route, the only sample of its request.
public struct HealthArchiveRoute: Encodable, Equatable, Sendable {
    /// The most points one route request may carry.
    public static let maxPoints = 100_000

    public var uuid: String
    public var workoutUUID: String
    public var source: String
    public var sourceName: String
    public var downsampled: Bool
    public var points: [HealthArchiveRoutePoint]

    /// Builds the route, thinning `points` to `maxPoints` when there are more.
    public init(uuid: String, workoutUUID: String, source: String, sourceName: String,
                points: [HealthArchiveRoutePoint], maxPoints: Int = HealthArchiveRoute.maxPoints) {
        self.uuid = uuid; self.workoutUUID = workoutUUID; self.source = source
        self.sourceName = sourceName
        self.downsampled = points.count > maxPoints
        self.points = Self.thin(points, to: maxPoints)
    }

    /// At most `max` elements, picked evenly across the whole sequence, always keeping the
    /// first and the last. Order is preserved; a short enough sequence is returned whole.
    public static func thin<T>(_ items: [T], to max: Int) -> [T] {
        let n = items.count
        guard n > max, max > 0 else { return items }
        guard max > 1 else { return [items[0]] }
        // Index i of max picks round(i·(n−1)/(max−1)): 0 first, n−1 last, strictly
        // increasing because n > max makes every step at least 1.
        return (0..<max).map { i in items[Int((Double(i) * Double(n - 1) / Double(max - 1)).rounded())] }
    }

    enum CodingKeys: String, CodingKey {
        case uuid, source, downsampled, points
        case workoutUUID = "workout_uuid"
        case sourceName = "source_name"
    }
}

/// One request body.
public struct HealthArchiveRequest<Sample: Encodable & Sendable>: Encodable, Sendable {
    public let schema = 1
    public var kind: HealthArchiveKind
    public var type: String
    public var samples: [Sample]
    public var deleted: [String]

    public init(kind: HealthArchiveKind, type: String, samples: [Sample], deleted: [String]) {
        self.kind = kind; self.type = type; self.samples = samples; self.deleted = deleted
    }

    enum CodingKeys: String, CodingKey { case schema, kind, type, samples, deleted }

    public func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        return try encoder.encode(self)
    }
}

/// Splits one type's new samples and deletions into requests the bridge accepts.
public enum HealthArchiveBatcher {
    /// The most samples one request may carry.
    public static let maxSamples = 2000

    /// Requests of at most `max` samples and at most `max` deletions each, both in their
    /// original order: samples chunk k rides with deletions chunk k, and whichever list is
    /// longer carries on alone. Nothing to send is no request at all.
    public static func batches<Sample>(samples: [Sample], deleted: [String],
                                       max: Int = maxSamples) -> [(samples: [Sample], deleted: [String])] {
        precondition(max > 0)
        let sampleChunks = chunks(samples, max)
        let deletedChunks = chunks(deleted, max)
        return (0..<Swift.max(sampleChunks.count, deletedChunks.count)).map { k in
            (k < sampleChunks.count ? sampleChunks[k] : [],
             k < deletedChunks.count ? deletedChunks[k] : [])
        }
    }

    /// The encoded bodies for one type, in order.
    public static func requests<Sample: Encodable & Sendable>(
        kind: HealthArchiveKind, type: String, samples: [Sample], deleted: [String],
        max: Int = maxSamples
    ) throws -> [Data] {
        try batches(samples: samples, deleted: deleted, max: max).map {
            try HealthArchiveRequest(kind: kind, type: type, samples: $0.samples,
                                     deleted: $0.deleted).encoded()
        }
    }

    private static func chunks<T>(_ items: [T], _ size: Int) -> [[T]] {
        stride(from: 0, to: items.count, by: size).map {
            Array(items[$0..<Swift.min($0 + size, items.count)])
        }
    }
}

/// A quantity series sample expanded into its points.
public enum HealthArchiveSeries {
    public struct Point: Sendable, Equatable {
        public var start: Date
        public var end: Date
        public var value: Double
        public init(start: Date, end: Date, value: Double) {
            self.start = start; self.end = end; self.value = value
        }
    }

    /// One wire sample per point, all under the series sample's UUID and identity, numbered
    /// `seq` 0, 1, 2… in the order given (HealthKit's own order, which is time order).
    public static func expand(_ points: [Point], template: HealthArchiveQuantitySample,
                              time: HealthArchiveTime.Formatter = .init()) -> [HealthArchiveQuantitySample] {
        points.enumerated().map { i, p in
            var s = template
            s.seq = i
            s.start = time.string(p.start)
            s.end = time.string(p.end)
            s.value = p.value
            return s
        }
    }
}

/// HealthKit metadata made JSON-safe.
public enum HealthArchiveMetadata {
    /// Convert one metadata dictionary. Strings and booleans pass through, finite numbers
    /// become numbers, dates become RFC 3339 strings, and whatever `quantity` recognizes (an
    /// `HKQuantity`, on the phone) becomes `{"value": n, "unit": "u"}`, a number in a stated
    /// unit. Everything else is dropped and counted, so the caller can log how many.
    public static func convert(_ metadata: [String: Any]?,
                               time: HealthArchiveTime.Formatter = .init(),
                               quantity: (Any) -> (value: Double, unit: String)? = { _ in nil })
        -> (values: [String: HealthArchiveValue], dropped: Int) {
        guard let metadata else { return ([:], 0) }
        var out: [String: HealthArchiveValue] = [:]
        var dropped = 0
        for (key, raw) in metadata {
            if let v = value(raw, time: time, quantity: quantity) {
                out[key] = v
            } else {
                dropped += 1
            }
        }
        return (out, dropped)
    }

    private static func value(_ raw: Any, time: HealthArchiveTime.Formatter,
                              quantity: (Any) -> (value: Double, unit: String)?) -> HealthArchiveValue? {
        switch raw {
        case let s as String:
            return .string(s)
        case let n as NSNumber:
            // A Bool bridged through NSNumber is a CFBoolean; it is a flag, not a 0 or 1.
            if CFGetTypeID(n) == CFBooleanGetTypeID() { return .bool(n.boolValue) }
            let d = n.doubleValue
            return d.isFinite ? .number(d) : nil
        case let d as Date:
            return .string(time.string(d))
        default:
            guard let q = quantity(raw), q.value.isFinite else { return nil }
            return .object(["value": .number(q.value), "unit": .string(q.unit)])
        }
    }
}

/// What one ingest request came back as. Everything but `.committed` leaves the anchor where
/// it was.
public enum HealthIngestOutcome: Equatable, Sendable {
    /// `200`: written.
    case committed
    /// `503 not_configured`: the bridge has no archive. The whole run stops.
    case notConfigured
    /// `503 unavailable`, or any other status the contract does not name: try later.
    case unavailable(Int)
    /// `400` or `413`: the batch is malformed, which is a bug on this side.
    case rejected(Int)
    /// `401`: the token is wrong.
    case unauthorized

    public static func from(status: Int, body: Data) -> HealthIngestOutcome {
        switch status {
        case 200: return .committed
        case 401: return .unauthorized
        case 400, 413: return .rejected(status)
        case 503:
            struct Reason: Decodable { var error: String? }
            let reason = (try? JSONDecoder().decode(Reason.self, from: body))?.error
            return reason == "not_configured" ? .notConfigured : .unavailable(status)
        default: return .unavailable(status)
        }
    }
}

extension JesseBridgeClient {
    /// The session the archive's uploads go through: a route body can be 15 MB, which the
    /// short-call session's 60-second ceiling does not allow for on a slow uplink.
    public static let archiveSession: URLSession = {
        let c = URLSessionConfiguration.default
        c.timeoutIntervalForRequest = 60
        c.timeoutIntervalForResource = 300
        c.waitsForConnectivity = false
        return URLSession(configuration: c)
    }()

    /// `POST /jesse/health/ingest` with one encoded request body. Throws only on a transport
    /// failure; every HTTP answer is mapped to an outcome.
    public func postHealthIngest(_ body: Data) async throws -> HealthIngestOutcome {
        guard var req = todayRequest("/jesse/health/ingest", method: "POST") else {
            throw JesseError.notConfigured
        }
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = body
        let using = session === JesseBridgeClient.boundedSession ? Self.archiveSession : session
        let data: Data, resp: URLResponse
        do {
            (data, resp) = try await using.data(for: req)
        } catch {
            throw JesseError.from(error, host: config.normalizedHost)
        }
        guard let http = resp as? HTTPURLResponse else { throw JesseError.decoding }
        return HealthIngestOutcome.from(status: http.statusCode, body: data)
    }
}
