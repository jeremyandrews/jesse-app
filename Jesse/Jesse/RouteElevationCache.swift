import Foundation

// The route elevation cache: why a long run's climb renders at all.
//
// Reading a route walks every location HealthKit saved for it, about one a second,
// so a marathon is ~12,600 points. That read cannot be allowed to hold up a turn (the
// whole health gather shares one bound, and an overrun empties the entire block), so
// it runs under its own bound. Before this cache, a read that missed the bound was
// thrown away and started again on the next turn, from scratch, forever; a long route
// could never render. Now the read is started once per workout, is never cancelled,
// and lands here whenever it finishes. A saved route never changes, so its four
// numbers are kept, keyed by the workout's UUID, and every later turn renders them
// without touching HealthKit.
//
// Foundation-only and HealthKit-free: the reader is injected as a closure, so the
// policy (hit, miss, late landing, dedupe, what is cached, pruning, persistence) is
// unit-tested without HealthKit, the way the reducers are.

/// What one read of a workout's route found.
nonisolated enum RouteReadResult: Equatable, Sendable {
    /// The workout owns no route series. NOT cached: a recorder may save the route
    /// after the workout, and a later read can find it.
    case noRoutes
    /// The read failed. NOT cached: the next gather tries again.
    case failed
    /// Routes were read and reduced. Cached either way, including a nil reduction
    /// (too few usable readings), which would be just as nil on every re-read.
    case reduced(RouteElevation?, seriesCount: Int, pointCount: Int)
}

/// One finished read, for the log line: sizes and timing only, never a location.
nonisolated struct RouteReadReport: Equatable, Sendable {
    var seriesCount: Int
    var pointCount: Int
    var milliseconds: Int
    /// True when the read finished inside the bound of the gather that started it,
    /// false when it landed late (its result reaches the next gather, from the cache).
    var withinBound: Bool
    /// True when the result was written to the cache.
    var cached: Bool
}

/// Reduced route elevation per workout, persisted, with at most one read in flight
/// per workout.
actor RouteElevationCache {
    /// One cached workout. `elevation` nil is a cached negative (routes, but too
    /// few usable readings), distinct from "not cached".
    nonisolated struct Entry: Codable, Equatable, Sendable {
        var elevation: RouteElevation?
        var computedAt: Date
        /// When the workout ended; the pruning clock.
        var workoutEnd: Date
    }

    /// Entries are kept while their workout ended inside the feed's 48 h window plus
    /// a 24 h margin (so a workout that slides out of the window and a clock skew
    /// around it cost no re-read), then dropped. Five workouts a window keeps the
    /// file to a few hundred bytes.
    static let retention: TimeInterval = (WorkoutContextFormatter.windowHours + 24) * 3600

    private var entries: [UUID: Entry]
    private var inFlight: [UUID: Task<RouteElevation?, Never>] = [:]
    private let fileURL: URL?
    private let now: @Sendable () -> Date
    private let report: @Sendable (RouteReadReport) -> Void

    /// `fileURL` nil keeps the cache in memory only. The file is read once here and
    /// pruned; an unreadable or corrupt file is an empty cache, never an error.
    init(fileURL: URL?,
         now: @escaping @Sendable () -> Date = { Date() },
         report: @escaping @Sendable (RouteReadReport) -> Void = { _ in }) {
        self.fileURL = fileURL
        self.now = now
        self.report = report
        var loaded: [UUID: Entry] = [:]
        if let fileURL, let data = try? Data(contentsOf: fileURL),
           let stored = try? JSONDecoder().decode([String: Entry].self, from: data) {
            for (key, entry) in stored {
                if let id = UUID(uuidString: key) { loaded[id] = entry }
            }
        }
        let cutoff = now().addingTimeInterval(-Self.retention)
        self.entries = loaded.filter { $0.value.workoutEnd >= cutoff }
    }

    /// The workout's route elevation for this gather, waiting at most `bound`.
    ///
    /// A hit returns at once with no read. A miss joins the read already in flight
    /// for this workout, or starts one; either way this gather waits up to `bound`
    /// and then moves on with nil, while the read runs to completion and lands in
    /// the cache for the next gather. The read is never cancelled: a HealthKit query
    /// ignores cancellation anyway, and cancelling would only throw the work away.
    func elevation(for id: UUID, workoutEnd: Date, within bound: Duration,
                   read: @escaping @Sendable () async -> RouteReadResult) async -> RouteElevation? {
        if let hit = entries[id] { return hit.elevation }
        let task = inFlight[id] ?? start(id, workoutEnd: workoutEnd, bound: bound, read: read)
        return await BoundedRead.orNil(within: bound) { await task.value }
    }

    private func start(_ id: UUID, workoutEnd: Date, bound: Duration,
                       read: @escaping @Sendable () async -> RouteReadResult)
        -> Task<RouteElevation?, Never> {
        let clock = ContinuousClock()
        let began = clock.now
        let task = Task<RouteElevation?, Never> {
            let result = await read()
            return self.land(id, workoutEnd: workoutEnd, result: result,
                             took: clock.now - began, bound: bound)
        }
        inFlight[id] = task
        return task
    }

    /// A finished read: cache what is worth caching, report it, clear the in-flight
    /// slot. Runs on the actor, so it never races a lookup.
    private func land(_ id: UUID, workoutEnd: Date, result: RouteReadResult,
                      took: Duration, bound: Duration) -> RouteElevation? {
        inFlight[id] = nil
        let ms = Int(took.components.seconds * 1000
                     + took.components.attoseconds / 1_000_000_000_000_000)
        let elevation: RouteElevation?
        let series: Int, points: Int, cached: Bool
        switch result {
        case .noRoutes, .failed:
            elevation = nil; series = 0; points = 0; cached = false
        case let .reduced(e, seriesCount, pointCount):
            elevation = e; series = seriesCount; points = pointCount; cached = true
            entries[id] = Entry(elevation: e, computedAt: now(), workoutEnd: workoutEnd)
            prune()
            persist()
        }
        report(RouteReadReport(seriesCount: series, pointCount: points, milliseconds: ms,
                               withinBound: took <= bound, cached: cached))
        return elevation
    }

    private func prune() {
        let cutoff = now().addingTimeInterval(-Self.retention)
        entries = entries.filter { $0.value.workoutEnd >= cutoff }
    }

    /// Best-effort, atomic, and outside every backup: the four numbers are
    /// recomputable from HealthKit, so a failed write costs one re-read.
    private func persist() {
        guard let fileURL else { return }
        let stored = Dictionary(uniqueKeysWithValues: entries.map { ($0.key.uuidString, $0.value) })
        guard let data = try? JSONEncoder().encode(stored) else { return }
        var dir = fileURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? dir.setResourceValues(values)
        try? data.write(to: fileURL, options: .atomic)
    }
}
