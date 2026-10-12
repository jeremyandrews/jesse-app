import CoreLocation
import Foundation
import HealthKit
import JesseNetworking

// The HealthKit half of the health archive: one anchored page of one stream, lifted into the
// wire values `JesseNetworking` defines and encoded, plus the observer queries that wake the
// engine when anything new lands. Deliberately thin: every rule about the request shape
// (batching, series numbering, metadata, timestamps, route thinning) is in JesseKit and tested
// there, and every rule about anchors is `HealthArchiveSync`'s.
//
// Coordinates. A workout route's locations are read here and travel to the bridge's ingest
// endpoint, which stores them in Jeremy's own database, and NOWHERE ELSE: never into the
// per-turn health block, a prompt, or a log line. The log lines below carry type names and
// counts only.

nonisolated struct HealthArchiveHealthKitSource: HealthArchiveSource {
    /// HealthKit objects per anchored page. A sample page is posted in requests of at most
    /// 2000 wire samples (a series expands into one per point); workouts carry events and
    /// are sent 100 to a request to stay well inside the bridge's 8 MiB body; a route is one
    /// request on its own, so it is one per page too, and its anchor moves route by route.
    static let samplePageLimit = 2000
    static let workoutPageLimit = 100
    static let workoutBatchLimit = 100
    static let routePageLimit = 1

    func page(_ stream: HealthArchiveStream, after anchor: Data?) async throws -> HealthArchivePage {
        let previous = anchor.flatMap {
            try? NSKeyedUnarchiver.unarchivedObject(ofClass: HKQueryAnchor.self, from: $0)
        }
        let store = HKHealthStore()
        let time = HealthArchiveTime.Formatter()
        switch stream {
        case .quantity(let id, let unitString):
            let type = HKQuantityType(HKQuantityTypeIdentifier(rawValue: id))
            let unit = HKUnit(from: unitString)
            let result = try await HKAnchoredObjectQueryDescriptor(
                predicates: [.quantitySample(type: type)], anchor: previous,
                limit: Self.samplePageLimit).result(for: store)
            guard !(result.addedSamples.isEmpty && result.deletedObjects.isEmpty) else { return .end }
            var samples: [HealthArchiveQuantitySample] = []
            var dropped = 0
            for s in result.addedSamples {
                let (base, d) = Self.wire(s, unit: unit, unitString: unitString, time: time)
                dropped += d
                if s.count > 1 {
                    let points = try await Self.seriesPoints(s, unit: unit, store: store)
                    samples += points.isEmpty ? [base]
                        : HealthArchiveSeries.expand(points, template: base, time: time)
                } else {
                    samples.append(base)
                }
            }
            Self.logDropped(dropped, id)
            return try Self.page(kind: .quantity, type: id, samples: samples,
                                 deleted: result.deletedObjects, anchor: result.newAnchor)

        case .category(let id):
            let type = HKCategoryType(HKCategoryTypeIdentifier(rawValue: id))
            let result = try await HKAnchoredObjectQueryDescriptor(
                predicates: [.categorySample(type: type)], anchor: previous,
                limit: Self.samplePageLimit).result(for: store)
            guard !(result.addedSamples.isEmpty && result.deletedObjects.isEmpty) else { return .end }
            var dropped = 0
            let samples = result.addedSamples.map { s -> HealthArchiveCategorySample in
                let meta = Self.metadata(s.metadata, time: time)
                dropped += meta.dropped
                return HealthArchiveCategorySample(
                    uuid: s.uuid.uuidString, start: time.string(s.startDate),
                    end: time.string(s.endDate), value: s.value,
                    source: s.sourceRevision.source.bundleIdentifier,
                    sourceName: s.sourceRevision.source.name, device: Self.device(s),
                    metadata: meta.values)
            }
            Self.logDropped(dropped, id)
            return try Self.page(kind: .category, type: id, samples: samples,
                                 deleted: result.deletedObjects, anchor: result.newAnchor)

        case .workouts:
            let result = try await HKAnchoredObjectQueryDescriptor(
                predicates: [.workout()], anchor: previous,
                limit: Self.workoutPageLimit).result(for: store)
            guard !(result.addedSamples.isEmpty && result.deletedObjects.isEmpty) else { return .end }
            var dropped = 0
            var workouts: [HealthArchiveWorkout] = []
            for w in result.addedSamples {
                let (workout, d) = await Self.wire(w, time: time)
                dropped += d
                workouts.append(workout)
            }
            Self.logDropped(dropped, HealthArchiveCatalogue.workoutType)
            return try Self.page(kind: .workout, type: HealthArchiveCatalogue.workoutType,
                                 samples: workouts, deleted: result.deletedObjects,
                                 anchor: result.newAnchor, batch: Self.workoutBatchLimit)

        case .routes:
            let result = try await HKAnchoredObjectQueryDescriptor(
                predicates: [.sample(type: HKSeriesType.workoutRoute())], anchor: previous,
                limit: Self.routePageLimit).result(for: store)
            guard !(result.addedSamples.isEmpty && result.deletedObjects.isEmpty) else { return .end }
            let deleted = result.deletedObjects.map(\.uuid.uuidString)
            var bodies: [Data] = []
            var sent = 0
            var skipped = 0
            for case let route as HKWorkoutRoute in result.addedSamples {
                guard let workout = try await Self.workout(owning: route, store: store) else {
                    // A route with no workout cannot be stored (the archive keys it by its
                    // workout). Counted and passed over, so one orphan cannot stall every
                    // route after it.
                    skipped += 1
                    continue
                }
                var points: [HealthArchiveRoutePoint] = []
                try await HealthContextProvider.readRouteLocations(route, store: store) { loc in
                    if let p = HealthArchiveRoutePoint(
                        t: time.string(loc.timestamp), lat: loc.coordinate.latitude,
                        lon: loc.coordinate.longitude, altitude: loc.altitude,
                        horizontalAccuracy: loc.horizontalAccuracy,
                        verticalAccuracy: loc.verticalAccuracy, speed: loc.speed,
                        course: loc.course) {
                        points.append(p)
                    }
                }
                let wireRoute = HealthArchiveRoute(
                    uuid: route.uuid.uuidString, workoutUUID: workout.uuidString,
                    source: route.sourceRevision.source.bundleIdentifier,
                    sourceName: route.sourceRevision.source.name, points: points)
                bodies.append(try HealthArchiveRequest(
                    kind: .route, type: HealthArchiveCatalogue.routeType,
                    samples: [wireRoute], deleted: bodies.isEmpty ? deleted : []).encoded())
                sent += 1
            }
            if bodies.isEmpty, !deleted.isEmpty {
                bodies.append(try HealthArchiveRequest<HealthArchiveRoute>(
                    kind: .route, type: HealthArchiveCatalogue.routeType,
                    samples: [], deleted: deleted).encoded())
            }
            if skipped > 0 {
                Log.health.error("archive: \(skipped) route(s) with no workout, not sent")
            }
            return HealthArchivePage(bodies: bodies, sampleCount: sent,
                                     deletedCount: deleted.count,
                                     newAnchor: Self.archive(result.newAnchor),
                                     isEnd: false, skipped: skipped)
        }
    }

    // MARK: Lifting samples

    private static func wire(_ s: HKQuantitySample, unit: HKUnit, unitString: String,
                             time: HealthArchiveTime.Formatter)
        -> (HealthArchiveQuantitySample, dropped: Int) {
        let meta = metadata(s.metadata, time: time)
        // The catalogue's unit is compatible with its type by construction (and tested), so
        // `doubleValue(for:)` cannot raise here.
        return (HealthArchiveQuantitySample(
            uuid: s.uuid.uuidString, seq: 0, start: time.string(s.startDate),
            end: time.string(s.endDate), value: s.quantity.doubleValue(for: unit),
            unit: unitString, source: s.sourceRevision.source.bundleIdentifier,
            sourceName: s.sourceRevision.source.name, device: device(s),
            metadata: meta.values), meta.dropped)
    }

    /// Every point of a quantity series sample, in HealthKit's order.
    private static func seriesPoints(_ s: HKQuantitySample, unit: HKUnit,
                                     store: HKHealthStore) async throws -> [HealthArchiveSeries.Point] {
        let descriptor = HKQuantitySeriesSampleQueryDescriptor(
            predicate: .quantitySample(type: s.quantityType,
                                       predicate: HKQuery.predicateForObject(with: s.uuid)),
            options: [])
        var points: [HealthArchiveSeries.Point] = []
        for try await r in descriptor.results(for: store) {
            points.append(.init(start: r.dateInterval.start, end: r.dateInterval.end,
                                value: r.quantity.doubleValue(for: unit)))
        }
        return points
    }

    private static func wire(_ w: HKWorkout, time: HealthArchiveTime.Formatter) async
        -> (HealthArchiveWorkout, dropped: Int) {
        let meta = metadata(w.metadata, time: time)
        var dropped = meta.dropped
        let bpm = HKUnit.count().unitDivided(by: .minute())
        let hr = w.statistics(for: HKQuantityType(.heartRate))
        let events = (w.workoutEvents ?? []).map { e -> HealthArchiveWorkoutEvent in
            var em = metadata(e.metadata, time: time)
            dropped += em.dropped
            // The six wire words fold motion pauses into pause/resume; the raw type keeps
            // the distinction (and the pause-or-resume request's) in the archive.
            em.values["HKWorkoutEventType"] = .number(Double(e.type.rawValue))
            return HealthArchiveWorkoutEvent(type: eventKind(e.type),
                                             start: time.string(e.dateInterval.start),
                                             end: time.string(e.dateInterval.end),
                                             metadata: em.values)
        }
        let workout = HealthArchiveWorkout(
            uuid: w.uuid.uuidString, start: time.string(w.startDate), end: time.string(w.endDate),
            durationS: w.duration, activityType: Int(w.workoutActivityType.rawValue),
            activityName: HealthContextProvider.activityName(w.workoutActivityType),
            distanceM: HealthContextProvider.distanceMeters(for: w),
            energyKcal: w.statistics(for: HKQuantityType(.activeEnergyBurned))?
                .sumQuantity()?.doubleValue(for: .kilocalorie()),
            avgHr: hr?.averageQuantity()?.doubleValue(for: bpm),
            maxHr: hr?.maximumQuantity()?.doubleValue(for: bpm),
            effort: await HealthContextProvider.effortScore(for: w)?.score,
            indoor: (w.metadata?[HKMetadataKeyIndoorWorkout] as? NSNumber)?.boolValue,
            source: w.sourceRevision.source.bundleIdentifier,
            sourceName: w.sourceRevision.source.name, device: device(w),
            metadata: meta.values, events: events)
        return (workout, dropped)
    }

    static func eventKind(_ t: HKWorkoutEventType) -> HealthArchiveWorkoutEvent.Kind {
        switch t {
        case .lap: return .lap
        case .segment: return .segment
        case .pause, .motionPaused: return .pause
        case .resume, .motionResumed: return .resume
        case .marker: return .marker
        default: return .other
        }
    }

    /// The workout a route belongs to. First the association itself: the workouts whose
    /// time overlaps the route's are asked which routes they own, and the one owning this
    /// route wins. Only when no workout claims it does the largest time overlap decide.
    private static func workout(owning route: HKWorkoutRoute, store: HKHealthStore) async throws -> UUID? {
        let overlapping = HKQuery.predicateForSamples(withStart: route.startDate.addingTimeInterval(-60),
                                                      end: route.endDate.addingTimeInterval(60),
                                                      options: [])
        let candidates = try await HKSampleQueryDescriptor(
            predicates: [.workout(overlapping)], sortDescriptors: []).result(for: store)
        for w in candidates {
            let owned = try await HKAnchoredObjectQueryDescriptor(
                predicates: [.sample(type: HKSeriesType.workoutRoute(),
                                     predicate: HKQuery.predicateForObjects(from: w))],
                anchor: nil).result(for: store).addedSamples
            if owned.contains(where: { $0.uuid == route.uuid }) { return w.uuid }
        }
        func overlap(_ w: HKWorkout) -> TimeInterval {
            min(w.endDate, route.endDate).timeIntervalSince(max(w.startDate, route.startDate))
        }
        return candidates.max { overlap($0) < overlap($1) }?.uuid
    }

    // MARK: Shared pieces

    /// The recording device's model identifier (`Watch8,1`), else HealthKit's device model.
    private static func device(_ s: HKSample) -> String? {
        s.sourceRevision.productType ?? s.device?.model
    }

    /// Metadata made JSON-safe. An `HKQuantity` value becomes a number in the first of these
    /// units it is compatible with; `doubleValue(for:)` raises an uncatchable ObjC exception
    /// on an incompatible unit, so compatibility is always checked first.
    static func metadata(_ m: [String: Any]?, time: HealthArchiveTime.Formatter)
        -> (values: [String: HealthArchiveValue], dropped: Int) {
        HealthArchiveMetadata.convert(m, time: time) { raw in
            guard let q = raw as? HKQuantity else { return nil }
            for unit in metadataUnits where q.is(compatibleWith: HKUnit(from: unit)) {
                return (q.doubleValue(for: HKUnit(from: unit)), unit)
            }
            return nil
        }
    }

    static let metadataUnits = ["m", "s", "kcal", "degC", "%", "count/min", "kg", "m/s", "W",
                                "kcal/(kg*hr)", "mL/(kg*min)", "dBASPL", "count", "appleEffortScore"]

    private static func page<S: Encodable & Sendable>(kind: HealthArchiveKind, type: String,
                                                      samples: [S], deleted: [HKDeletedObject],
                                                      anchor: HKQueryAnchor,
                                                      batch: Int = HealthArchiveBatcher.maxSamples) throws
        -> HealthArchivePage {
        let deletedIDs = deleted.map(\.uuid.uuidString)
        return HealthArchivePage(
            bodies: try HealthArchiveBatcher.requests(kind: kind, type: type, samples: samples,
                                                      deleted: deletedIDs, max: batch),
            sampleCount: samples.count, deletedCount: deletedIDs.count,
            newAnchor: archive(anchor), isEnd: false)
    }

    private static func archive(_ anchor: HKQueryAnchor) -> Data? {
        try? NSKeyedArchiver.archivedData(withRootObject: anchor, requiringSecureCoding: true)
    }

    private static func logDropped(_ n: Int, _ type: String) {
        if n > 0 { Log.health.notice("archive: \(type): \(n) metadata value(s) not JSON-safe, dropped") }
    }
}

// MARK: - Waking the engine

/// Observer queries with background delivery for every archive stream. Each wake runs the
/// engine (joining a run already in flight) and only then calls HealthKit's completion, which
/// must be called on every path.
///
/// Frequency: hourly, except body mass and workouts, which the weigh-in and workout chat
/// triggers (`HealthDataObserver`) already ask to be delivered immediately. Background
/// delivery is one setting per type per app, so asking hourly here would slow those triggers.
@MainActor
final class HealthArchiveObserver {
    nonisolated deinit {}

    static let shared = HealthArchiveObserver()

    private var queries: [HKObserverQuery] = []
    private let store = HKHealthStore()

    static var sampleTypes: [HKSampleType] {
        HealthArchiveStream.all.map { stream in
            switch stream {
            case .quantity(let id, _): return HKQuantityType(HKQuantityTypeIdentifier(rawValue: id))
            case .category(let id): return HKCategoryType(HKCategoryTypeIdentifier(rawValue: id))
            case .workouts: return HKObjectType.workoutType()
            case .routes: return HKSeriesType.workoutRoute()
            }
        }
    }

    nonisolated static func frequency(for type: HKSampleType) -> HKUpdateFrequency {
        type == HKQuantityType(.bodyMass) || type == HKObjectType.workoutType() ? .immediate : .hourly
    }

    /// Register once, if Apple Health is connected. Called from launch (so the observers
    /// exist when HealthKit relaunches the app to deliver) and when the toggle changes.
    func startIfEnabled() {
        guard HealthContextSettings.isEnabled, HKHealthStore.isHealthDataAvailable() else {
            stop()
            return
        }
        guard queries.isEmpty else { return }
        for type in Self.sampleTypes {
            let query = HKObserverQuery(sampleType: type, predicate: nil) { _, completion, error in
                let done = ObserverCompletion(completion)
                if error != nil {
                    done.call()
                    return
                }
                Task { @MainActor in
                    await HealthArchiveSync.shared.sync()
                    done.call()
                }
            }
            queries.append(query)
            store.execute(query)
        }
        reassertBackgroundDelivery()
        Log.health.notice("archive: observing \(self.queries.count) type(s)")
    }

    /// Ask for background delivery of every archive type.
    private func reassertBackgroundDelivery() {
        for type in Self.sampleTypes {
            store.enableBackgroundDelivery(for: type, frequency: Self.frequency(for: type)) { enabled, _ in
                if !enabled { Log.health.error("archive: background delivery not enabled for a type") }
            }
        }
    }

    /// The toggle went off. The chat triggers' observer stops with it (it turns off all of
    /// the app's background delivery), so nothing here outlives the other.
    func stop() {
        guard !queries.isEmpty else { return }
        for query in queries { store.stop(query) }
        queries = []
        for type in Self.sampleTypes {
            store.disableBackgroundDelivery(for: type) { _, _ in }
        }
    }
}
