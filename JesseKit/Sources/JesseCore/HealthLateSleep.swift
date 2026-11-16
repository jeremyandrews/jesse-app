import Foundation

// **The late sleep turn**: last night's sleep reached HealthKit after a health turn today
// already went out without it.
//
// The Watch hands a night to the phone some time after waking, often over an hour later. A
// health turn sent in that gap finds the PREVIOUS night as the newest session in the lookback.
// The health block now says so (the "not synced" sleep line), and the app records that it
// sent one. When the real night lands later, the iPhone sends exactly one automatic turn
// carrying it.
//
// Everything that decides WHEN lives here as functions of plain values, beside `DietDay` and
// `HealthAutoFire`, because the HealthKit side (observer queries, background delivery) has no
// seam and does not run on the Simulator.

public nonisolated enum HealthLateSleep {
    /// The late turn's text. A CONSTANT, never built from data: the send path's lock hold
    /// recognizes it by text, and the health block attached at send time carries the figures.
    public static let prompt =
        "Last night's sleep synced after an earlier health turn today went out without it. Record last night's sleep from the health block into today's notes."

    /// Whether a sleep session ending at `end` is the night that belongs to the diet day of
    /// `now`: the local calendar date of `end` (Gregorian, in `calendar`'s zone) equals
    /// `DietDay.stamp(for: now)`.
    ///
    /// At 08:51 a night ending 07:10 today is last night and one ending 07:10 yesterday is
    /// not. At 00:30 the diet day is still yesterday, so the night that ended yesterday
    /// morning is still last night.
    public static func endsOnDietDay(end: Date, now: Date, calendar: Calendar) -> Bool {
        var gregorian = Calendar(identifier: .gregorian)
        gregorian.timeZone = calendar.timeZone
        let c = gregorian.dateComponents([.year, .month, .day], from: end)
        let endDay = String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
        return endDay == DietDay.stamp(for: now, calendar: calendar)
    }

    /// **Whether the late sleep turn should fire.** True only when the newest session is
    /// last night, an earlier turn TODAY carried the not synced line (`notSyncedDay`), and
    /// no turn today has carried last night yet (`lastNightSentDay`). The second clause
    /// makes it at most one per diet day: staging it writes `lastNightSentDay`.
    public static func shouldFire(sessionIsLastNight: Bool, notSyncedDay: String?,
                                  lastNightSentDay: String?, today: String) -> Bool {
        sessionIsLastNight && notSyncedDay == today && lastNightSentDay != today
    }
}
