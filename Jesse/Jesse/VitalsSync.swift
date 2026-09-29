import Foundation
import JesseNetworking
import UIKit

/// Keeps the Studio's daily vitals ledger fed from Apple Health.
///
/// The first successful upload to a bridge carries `DailyVitals.backfillDays` days of
/// history; every one after it carries the last `DailyVitals.resendDays`, which is how a
/// late watch sync or a today still counting its steps reaches the Studio. It runs on each
/// foreground and on the periodic background refresh the app already does, and never on a
/// timer of its own.
///
/// It asks HealthKit for nothing new: every type it reads is one the per-turn health block
/// already reads, so no permission prompt is raised by this. It never writes to Health.
@MainActor
final class VitalsSync {
    nonisolated deinit {}

    static let shared = VitalsSync()

    private let defaults: UserDefaults
    private let configProvider: @MainActor () -> JesseConfig
    private let read: @Sendable (Int) async throws -> [VitalsDay]
    private let post: @Sendable (JesseConfig, [VitalsDay]) async throws -> Void
    private let protectedDataAvailable: @MainActor () -> Bool
    private var inFlight = false

    init(defaults: UserDefaults = .standard,
         configProvider: @escaping @MainActor () -> JesseConfig = { ConfigStore.load() },
         read: @escaping @Sendable (Int) async throws -> [VitalsDay] = {
             try await HealthContextProvider.dailyVitals(days: $0)
         },
         post: @escaping @Sendable (JesseConfig, [VitalsDay]) async throws -> Void = { cfg, days in
             try await JesseBridgeClient(config: cfg).postVitals(days)
         },
         protectedDataAvailable: @escaping @MainActor () -> Bool = {
             UIApplication.shared.isProtectedDataAvailable
         }) {
        self.defaults = defaults
        self.configProvider = configProvider
        self.read = read
        self.post = post
        self.protectedDataAvailable = protectedDataAvailable
    }

    /// The backfill is remembered PER BRIDGE: pairing with a different Studio starts its
    /// ledger from nothing, so it gets the history too.
    static func backfillKey(host: String, port: Int) -> String {
        "vitals.backfilled.\(host):\(port)"
    }

    /// Read and upload, if there is a bridge to upload to. Never throws and never blocks a
    /// caller on failure: a failed read or upload is logged and simply tried again on the
    /// next foreground or refresh, and the backfill is marked done only once one succeeds.
    /// Returns whether an upload went through, for the background task's outcome.
    @discardableResult
    func sync() async -> Bool {
        let cfg = configProvider()
        guard cfg.isConfigured, !inFlight else { return false }
        // A locked phone cannot read HealthKit: every query fails, which the reader turns
        // into a throw anyway, but not starting is cheaper and logs nothing alarming.
        guard protectedDataAvailable() else { return false }
        inFlight = true
        defer { inFlight = false }

        let key = Self.backfillKey(host: cfg.normalizedHost, port: cfg.effectivePort)
        let backfilled = defaults.bool(forKey: key)
        let span = backfilled ? DailyVitals.resendDays : DailyVitals.backfillDays
        do {
            let days = try await read(span)
            guard !days.isEmpty else { return false }
            try await post(cfg, days)
            if !backfilled { defaults.set(true, forKey: key) }
            Log.health.notice("vitals: sent \(days.count) day(s)\(backfilled ? "" : " (backfill)")")
            return true
        } catch {
            Log.health.error("vitals: not sent: \(error.localizedDescription)")
            return false
        }
    }
}
