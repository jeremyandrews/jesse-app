import Foundation
import JesseNetworking

// The pure half of the daily vitals ledger: HealthKit's raw samples, already flattened to
// plain values by `HealthContextProvider`, reduced to one `VitalsDay` per local date. No
// HealthKit here, so every rule is reachable from a unit test with fixed samples and a
// fixed zone.
//
// UNKNOWN IS NOT ZERO. A day a metric has no sample for leaves that metric nil, and a day
// with no metric at all produces no `VitalsDay`. Nothing is interpolated or carried
// forward from a neighbouring day.

// MARK: - Sleep by night

nonisolated extension SleepReducer {
    /// Split samples into sessions: sorted by start, a new session begins wherever a
    /// sample starts more than `sessionGap` after everything before it has ended. The same
    /// gap rule `reduce` uses to find last night, applied across a whole history.
    static func sessions(_ samples: [SleepSample]) -> [[SleepSample]] {
        var out: [[SleepSample]] = []
        var current: [SleepSample] = []
        var runEnd: Date?
        for s in samples.sorted(by: { $0.start < $1.start }) {
            if let end = runEnd, s.start.timeIntervalSince(end) > sessionGap {
                out.append(current)
                current = []
                runEnd = nil
            }
            current.append(s)
            runEnd = max(runEnd ?? s.end, s.end)
        }
        if !current.isEmpty { out.append(current) }
        return out
    }

    /// One `SleepSummary` per night, keyed by the WAKE date: the local `yyyy-MM-dd` of the
    /// session's last sample end in `timeZone`. A night that starts at 23:10 on the 26th and
    /// ends at 06:40 on the 27th is the 27th's night, which is the day it is read against.
    ///
    /// Each session goes through `reduce`, so the totals are the same interval union and the
    /// stages the same single-source breakdown as last night's summary; nothing about how a
    /// night is measured is decided twice. Naps are dropped (a nap is not the night), and
    /// when two sessions still wake on the same date (a night broken by more than an hour
    /// awake) the longer one is that date's night.
    static func nights(_ samples: [SleepSample], timeZone: TimeZone) -> [String: SleepSummary] {
        var out: [String: SleepSummary] = [:]
        for session in sessions(samples) {
            guard let summary = reduce(session, timeZone: timeZone), !summary.isNap,
                  let wake = session.map(\.end).max() else { continue }
            let key = DailyVitals.dayKey(wake, timeZone: timeZone)
            if let existing = out[key], existing.totalMinutes >= summary.totalMinutes { continue }
            out[key] = summary
        }
        return out
    }
}

// MARK: - Assembly

/// Builds the `VitalsDay` rows the phone uploads.
nonisolated enum DailyVitals {
    /// How far back the first upload reaches.
    static let backfillDays = 120
    /// How far back every later upload reaches: enough to catch a late watch sync, and a
    /// today whose steps were still being counted the last time it was sent.
    static let resendDays = 3

    /// The local `yyyy-MM-dd` an instant falls on in `timeZone`.
    static func dayKey(_ date: Date, timeZone: TimeZone) -> String {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let c = cal.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
    }

    /// The `days` local dates ending on the day `now` falls on, oldest first.
    static func dayKeys(endingAt now: Date, days: Int, timeZone: TimeZone) -> [String] {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let today = cal.startOfDay(for: now)
        return (0..<max(days, 0)).reversed().compactMap { back in
            cal.date(byAdding: .day, value: -back, to: today).map { dayKey($0, timeZone: timeZone) }
        }
    }

    /// Average readings by the local day each one ENDS on. For the overnight signals
    /// (respiratory rate, wrist temperature) the end is the morning, so a reading taken
    /// across midnight lands on the wake date like the night it belongs to.
    static func averageByEndDay(_ readings: [(end: Date, value: Double)],
                                timeZone: TimeZone) -> [String: Double] {
        var sums: [String: (total: Double, count: Int)] = [:]
        for r in readings where r.value.isFinite {
            let key = dayKey(r.end, timeZone: timeZone)
            let prior = sums[key] ?? (0, 0)
            sums[key] = (prior.total + r.value, prior.count + 1)
        }
        return sums.mapValues { $0.total / Double($0.count) }
    }

    /// The per-day quantity readings `assemble` takes, each keyed by local `yyyy-MM-dd`.
    struct Quantities: Equatable, Sendable {
        var restingHr: [String: Double] = [:]
        var hrv: [String: Double] = [:]
        var steps: [String: Double] = [:]
        var activeKcal: [String: Double] = [:]
        var respRate: [String: Double] = [:]
        var wristTempC: [String: Double] = [:]
    }

    /// One `VitalsDay` per date in `dates` that knows at least one metric, oldest first. A
    /// date absent from every input produces nothing; a metric absent for a date stays nil.
    static func assemble(dates: [String], nights: [String: SleepSummary],
                         quantities q: Quantities) -> [VitalsDay] {
        dates.compactMap { date in
            let night = nights[date]
            let day = VitalsDay(
                date: date,
                sleepMin: night?.totalMinutes,
                deepMin: night?.deepMinutes,
                remMin: night?.remMinutes,
                awakeMin: night?.awakeMinutes,
                restingHr: q.restingHr[date],
                hrv: q.hrv[date],
                steps: q.steps[date],
                activeKcal: q.activeKcal[date],
                respRate: q.respRate[date],
                wristTempC: q.wristTempC[date])
            return day.isEmpty ? nil : day
        }
    }
}
