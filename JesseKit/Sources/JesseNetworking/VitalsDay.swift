import Foundation

/// One day of what Apple Health measured, as the phone sends it to the bridge's daily vitals
/// ledger (`POST /jesse/diet/vitals`, bridge 0.161.0 and later). The bridge keeps one row
/// per `date` in `diet-logs/vitals-log.csv`, replaced in place when a day is sent again, and
/// the Patterns engine reads that history.
///
/// UNKNOWN IS NOT ZERO. Every metric is optional, and a nil one is left out of the JSON
/// entirely, so the bridge writes an empty cell rather than a 0. A night with no sleep
/// recorded, a day the watch was off the wrist: those are gaps, and a 0 would be a
/// measurement nobody took. A day with no metric at all is never sent (the bridge refuses
/// one), because the usual way to get one is a HealthKit read that failed.
///
/// Sleep is attributed to the WAKE date: the night of the 26th into the 27th is the 27th's.
public struct VitalsDay: Codable, Equatable, Sendable {
    /// `yyyy-MM-dd`, the device's local day.
    public var date: String
    /// Minutes asleep, every stage, unioned across sources.
    public var sleepMin: Double?
    public var deepMin: Double?
    public var remMin: Double?
    public var awakeMin: Double?
    /// Resting heart rate, bpm, the day's average.
    public var restingHr: Double?
    /// Heart rate variability (SDNN), ms, the day's average.
    public var hrv: Double?
    public var steps: Double?
    public var activeKcal: Double?
    /// Respiratory rate, breaths per minute, the average of samples ending that day.
    public var respRate: Double?
    /// Sleeping wrist temperature, °C as HealthKit reports it.
    public var wristTempC: Double?

    public init(date: String, sleepMin: Double? = nil, deepMin: Double? = nil,
                remMin: Double? = nil, awakeMin: Double? = nil, restingHr: Double? = nil,
                hrv: Double? = nil, steps: Double? = nil, activeKcal: Double? = nil,
                respRate: Double? = nil, wristTempC: Double? = nil) {
        self.date = date; self.sleepMin = sleepMin; self.deepMin = deepMin
        self.remMin = remMin; self.awakeMin = awakeMin; self.restingHr = restingHr
        self.hrv = hrv; self.steps = steps; self.activeKcal = activeKcal
        self.respRate = respRate; self.wristTempC = wristTempC
    }

    /// True when not one metric is known — a day that is never sent.
    public var isEmpty: Bool {
        [sleepMin, deepMin, remMin, awakeMin, restingHr, hrv, steps, activeKcal,
         respRate, wristTempC].allSatisfy { $0 == nil }
    }
}

/// The body of `POST /jesse/diet/vitals`.
public struct VitalsUpload: Encodable, Equatable, Sendable {
    public var days: [VitalsDay]
    public init(days: [VitalsDay]) { self.days = days }
}

extension JesseBridgeClient {
    /// `POST /jesse/diet/vitals`: upsert these days into the Studio's vitals ledger. Days with
    /// no metric are dropped here, before the request is built; an upload left with none is
    /// not sent at all. Throws on a transport, auth or HTTP failure so the caller can try
    /// again on the next refresh.
    public func postVitals(_ days: [VitalsDay]) async throws {
        let known = days.filter { !$0.isEmpty }
        guard !known.isEmpty else { return }
        guard var req = todayRequest("/jesse/diet/vitals", method: "POST") else {
            throw JesseError.notConfigured
        }
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try Self.encodeBody(VitalsUpload(days: known))
        let (data, http) = try await todaySend(req)
        guard (200..<300).contains(http.statusCode) else {
            throw JesseError.badResponse(http.statusCode, String(data: data, encoding: .utf8) ?? "")
        }
    }
}
