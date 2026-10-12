import Foundation
import HealthKit
import JesseNetworking
import Observation
import UIKit

// The health archive: every HealthKit sample the app may read, every workout with its events,
// and every workout route, copied to the bridge's `POST /jesse/health/ingest`, which keeps
// them in Jeremy's own Postgres archive.
//
// The engine here decides nothing about HealthKit objects. A source (the live one is
// `HealthArchiveHealthKitSource`) turns "what changed since this anchor" into encoded request
// bodies and the anchor to store once they are written; the engine posts them in order and
// owns the one rule that keeps the archive whole:
//
//   AN ANCHOR ADVANCES ONLY AFTER EVERY BATCH OF ITS PAGE CAME BACK 200.
//
// Anything else (a 503, a 400, a thrown transport error, a failed read) leaves the anchor
// where it was, so the same page is read and sent again on the next run, which the bridge's
// upserts make harmless. The first run for a type has no anchor and reads the type's whole
// history, a page at a time; since the anchor advances per page, an interrupted backfill
// resumes from the last page that was written.
//
// A locked phone is not "no new data". HealthKit's store is encrypted while the phone is
// locked and every query throws `errorDatabaseInaccessible`; that ends the run quietly with no
// anchor touched.
//
// The anchors live under `healthArchive.anchor.`, never the `healthAnchor.*` keys the
// weigh-in and workout chat triggers use, so the two never move each other's position.
//
// Logging: type names and counts only. Never a value, a coordinate or a place name.

/// One stream of the archive: a catalogue type, workouts, or workout routes.
nonisolated enum HealthArchiveStream: Hashable, Sendable {
    case quantity(identifier: String, unit: String)
    case category(identifier: String)
    case workouts
    case routes

    /// The HealthKit identifier the stream's requests carry as `type`.
    var typeIdentifier: String {
        switch self {
        case .quantity(let id, _), .category(let id): return id
        case .workouts: return HealthArchiveCatalogue.workoutType
        case .routes: return HealthArchiveCatalogue.routeType
        }
    }

    static let anchorPrefix = "healthArchive.anchor."
    var anchorKey: String { Self.anchorPrefix + typeIdentifier }
    /// Set once a run has read this stream to its end: what the Settings row counts as done.
    var caughtUpKey: String { "healthArchive.caughtUp." + typeIdentifier }

    /// Every stream, in the order a run walks them: the catalogue's quantity and category
    /// types, then workouts, then routes (after their workouts, so a route's workout is
    /// normally already in the archive when the route arrives).
    static var all: [HealthArchiveStream] {
        HealthArchiveCatalogue.quantities.map { .quantity(identifier: $0.identifier, unit: $0.unit) }
            + HealthArchiveCatalogue.categories.map { .category(identifier: $0) }
            + [.workouts, .routes]
    }
}

/// One page read from HealthKit, already encoded.
nonisolated struct HealthArchivePage: Sendable {
    /// The request bodies, in the order they must be posted.
    var bodies: [Data]
    /// Wire samples across the bodies (series points counted one each), for progress.
    var sampleCount: Int
    /// Deletions across the bodies.
    var deletedCount: Int
    /// The archived `HKQueryAnchor` to store once every body is written.
    var newAnchor: Data?
    /// True when HealthKit returned nothing added and nothing deleted: the stream is caught up.
    var isEnd: Bool
    /// Samples the source could not encode and left out (unsupported metadata values are
    /// counted separately by the source's own log line; this is whole objects).
    var skipped: Int = 0

    static let end = HealthArchivePage(bodies: [], sampleCount: 0, deletedCount: 0,
                                       newAnchor: nil, isEnd: true)
}

/// Reads one page of one stream after the given anchor.
protocol HealthArchiveSource: Sendable {
    func page(_ stream: HealthArchiveStream, after anchor: Data?) async throws -> HealthArchivePage
}

/// Posts one encoded body. Throws only on a transport failure.
typealias HealthArchiveTransport = @Sendable (JesseConfig, Data) async throws -> HealthIngestOutcome

@MainActor
@Observable
final class HealthArchiveSync {
    nonisolated deinit {}

    static let shared = HealthArchiveSync()

    /// What the Settings row shows.
    enum State: Equatable {
        /// No bridge paired, or Apple Health not connected.
        case off
        /// The bridge answered `503 not_configured`: it has no archive.
        case notConfigured
        /// Some stream has not been read to its end yet.
        case backfilling(sent: Int, typesDone: Int, typesTotal: Int)
        case upToDate
    }

    private(set) var isRunning = false
    /// Samples written during the current (or last) run, for the backfill progress line.
    private(set) var sentThisRun = 0
    /// Bumped whenever the persisted status changes, so the row redraws.
    private(set) var revision = 0

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let configProvider: @MainActor () -> JesseConfig
    @ObservationIgnored private let source: any HealthArchiveSource
    @ObservationIgnored private let transport: HealthArchiveTransport
    @ObservationIgnored private let protectedDataAvailable: @MainActor () -> Bool
    @ObservationIgnored private let isEnabled: @MainActor () -> Bool
    @ObservationIgnored private let now: @MainActor () -> Date
    @ObservationIgnored let streams: [HealthArchiveStream]
    @ObservationIgnored private var inFlight: Task<Void, Never>?

    static let lastSyncKey = "healthArchive.lastSync"
    static let lastErrorKey = "healthArchive.lastError"
    static let notConfiguredKey = "healthArchive.notConfigured"

    init(defaults: UserDefaults = .standard,
         configProvider: @escaping @MainActor () -> JesseConfig = { ConfigStore.load() },
         source: any HealthArchiveSource = HealthArchiveHealthKitSource(),
         transport: @escaping HealthArchiveTransport = { cfg, body in
             try await JesseBridgeClient(config: cfg).postHealthIngest(body)
         },
         protectedDataAvailable: @escaping @MainActor () -> Bool = {
             UIApplication.shared.isProtectedDataAvailable
         },
         isEnabled: @escaping @MainActor () -> Bool = {
             HealthContextSettings.isEnabled && HKHealthStore.isHealthDataAvailable()
         },
         now: @escaping @MainActor () -> Date = { Date() },
         streams: [HealthArchiveStream] = HealthArchiveStream.all) {
        self.defaults = defaults
        self.configProvider = configProvider
        self.source = source
        self.transport = transport
        self.protectedDataAvailable = protectedDataAvailable
        self.isEnabled = isEnabled
        self.now = now
        self.streams = streams
    }

    // MARK: Status

    var lastSync: Date? { _ = revision; return defaults.object(forKey: Self.lastSyncKey) as? Date }
    /// The kind of the last failure (`unavailable`, `rejected 400`, `unauthorized`,
    /// `transport`, `read`), nil once a run finishes clean.
    var lastError: String? { _ = revision; return defaults.string(forKey: Self.lastErrorKey) }
    var typesDone: Int { _ = revision; return streams.filter { defaults.bool(forKey: $0.caughtUpKey) }.count }

    var state: State {
        _ = revision
        guard isEnabled(), configProvider().isConfigured else { return .off }
        if defaults.bool(forKey: Self.notConfiguredKey) { return .notConfigured }
        let done = typesDone
        return done < streams.count
            ? .backfilling(sent: sentThisRun, typesDone: done, typesTotal: streams.count)
            : .upToDate
    }

    // MARK: Running

    /// Run once, or join the run already in flight: two triggers never make two runs. Called
    /// on every observer wake, the background refresh, each foreground and the Settings button.
    func sync() async {
        if let inFlight {
            await inFlight.value
            return
        }
        let task = Task { await run() }
        inFlight = task
        await task.value
        inFlight = nil
    }

    /// The Settings button: the same run, in the foreground, with the screen kept awake so a
    /// long first backfill is not cut short by auto-lock (a locked phone cannot read Health).
    func startBackfill() async {
        UIApplication.shared.isIdleTimerDisabled = true
        defer { UIApplication.shared.isIdleTimerDisabled = false }
        await sync()
    }

    private enum StreamResult { case done, failed, stopRun }

    private func run() async {
        let cfg = configProvider()
        guard isEnabled(), cfg.isConfigured else { return }
        guard protectedDataAvailable() else {
            Log.health.notice("archive: phone locked, not started")
            return
        }
        isRunning = true
        sentThisRun = 0
        defer { isRunning = false; revision += 1 }

        var clean = true
        for stream in streams {
            switch await drain(stream, cfg: cfg) {
            case .done: break
            case .failed: clean = false
            case .stopRun: return
            }
        }
        if clean {
            defaults.set(now(), forKey: Self.lastSyncKey)
            defaults.removeObject(forKey: Self.lastErrorKey)
        }
        Log.health.notice("archive: run finished, \(self.sentThisRun) sample(s) sent\(clean ? "" : ", with errors")")
    }

    /// Read and send one stream page by page until HealthKit has nothing more.
    private func drain(_ stream: HealthArchiveStream, cfg: JesseConfig) async -> StreamResult {
        let name = stream.typeIdentifier
        while true {
            let anchor = defaults.data(forKey: stream.anchorKey)
            let page: HealthArchivePage
            do {
                page = try await source.page(stream, after: anchor)
            } catch {
                if Self.isDatabaseInaccessible(error) {
                    Log.health.notice("archive: phone locked during \(name), run ended")
                    return .stopRun
                }
                Log.health.error("archive: \(name) read failed: \(Self.errorKind(error))")
                record(error: "read")
                return .failed
            }
            if page.isEnd {
                // Locked between the query and now would have thrown; this check is the
                // belt to that brace, so an empty answer is never taken as "caught up"
                // from a store that could not be read.
                guard protectedDataAvailable() else { return .stopRun }
                defaults.set(true, forKey: stream.caughtUpKey)
                return .done
            }
            for body in page.bodies {
                let outcome: HealthIngestOutcome
                do {
                    outcome = try await transport(cfg, body)
                } catch {
                    Log.health.error("archive: \(name) not sent: transport")
                    record(error: "transport")
                    return .failed
                }
                switch outcome {
                case .committed:
                    continue
                case .notConfigured:
                    Log.health.notice("archive: the bridge has no archive configured; run stopped")
                    defaults.set(true, forKey: Self.notConfiguredKey)
                    revision += 1
                    return .stopRun
                case .unauthorized:
                    Log.health.error("archive: \(name) refused: unauthorized; run stopped")
                    record(error: "unauthorized")
                    return .stopRun
                case .rejected(let status):
                    Log.health.error("archive: \(name) batch rejected: \(status)")
                    record(error: "rejected \(status)")
                    return .failed
                case .unavailable(let status):
                    Log.health.error("archive: \(name) not written: unavailable \(status)")
                    record(error: "unavailable")
                    return .failed
                }
            }
            // Every body of the page is written: only now does the anchor move.
            if let newAnchor = page.newAnchor { defaults.set(newAnchor, forKey: stream.anchorKey) }
            if defaults.bool(forKey: Self.notConfiguredKey) {
                defaults.removeObject(forKey: Self.notConfiguredKey)
            }
            sentThisRun += page.sampleCount
            revision += 1
            Log.health.notice("archive: \(name): \(page.sampleCount) sent, \(page.deletedCount) deleted\(page.skipped > 0 ? ", \(page.skipped) skipped" : "")")
        }
    }

    private func record(error kind: String) {
        defaults.set(kind, forKey: Self.lastErrorKey)
        revision += 1
    }

    nonisolated static func isDatabaseInaccessible(_ error: Error) -> Bool {
        if let hk = error as? HKError { return hk.code == .errorDatabaseInaccessible }
        let ns = error as NSError
        return ns.domain == HKErrorDomain && ns.code == HKError.Code.errorDatabaseInaccessible.rawValue
    }

    /// A failure's kind for a log line: the error's domain and code, never its description,
    /// which for a HealthKit error may name a type's value.
    nonisolated static func errorKind(_ error: Error) -> String {
        let ns = error as NSError
        return "\(ns.domain) \(ns.code)"
    }
}
