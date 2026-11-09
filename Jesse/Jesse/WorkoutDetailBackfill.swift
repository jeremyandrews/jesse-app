import Foundation
import JesseNetworking
import Observation
import UIKit

/// Reads every workout since a start date and posts each one's structured `WorkoutRecord`
/// to the bridge's workout detail ledger (`POST /jesse/diet/workouts`), in pages of at most
/// `JesseBridgeClient.workoutRecordPageSize`. The bridge upserts by workout UUID and starts
/// no model turn, so running it twice changes nothing the second time.
///
/// The per-workout reads are the feed's own (`HealthContextProvider.backfillWorkouts`) and
/// the records are built by the same `WorkoutRecordBuilder` the per-turn path uses, so a
/// backfilled row and a row written by a turn are the same row.
@MainActor
@Observable
final class WorkoutDetailBackfill {
    nonisolated deinit {}

    static let shared = WorkoutDetailBackfill()

    /// Reads summaries since a date, delivering pages: (page, read so far, total).
    typealias Reader = @Sendable (Date, Int, @escaping @Sendable ([WorkoutSummary], Int, Int) async throws -> Void) async throws -> Void
    /// Posts one page of records; returns what the bridge did with it.
    typealias Poster = @Sendable (JesseConfig, [WorkoutRecord]) async throws -> WorkoutRecordUpsertResult

    /// The default start: this many days before today.
    static let defaultDays = 365

    enum Phase: Equatable {
        case idle
        case running(read: Int, total: Int)
        case finished(Counts)
        case failed(String)
    }

    struct Counts: Equatable, Sendable {
        var sent = 0
        var written = 0
        var unchanged = 0
    }

    private(set) var phase: Phase = .idle
    private(set) var counts = Counts()

    var isRunning: Bool {
        if case .running = phase { return true }
        return false
    }

    @ObservationIgnored private let configProvider: @MainActor () -> JesseConfig
    @ObservationIgnored private let reader: Reader
    @ObservationIgnored private let poster: Poster
    @ObservationIgnored private let protectedDataAvailable: @MainActor () -> Bool
    @ObservationIgnored private let timeZone: TimeZone

    init(configProvider: @escaping @MainActor () -> JesseConfig = { ConfigStore.load() },
         reader: @escaping Reader = { since, size, onPage in
             try await HealthContextProvider.backfillWorkouts(since: since, pageSize: size,
                                                              onPage: onPage)
         },
         poster: @escaping Poster = { cfg, records in
             try await JesseBridgeClient(config: cfg).postWorkoutRecords(records)
         },
         protectedDataAvailable: @escaping @MainActor () -> Bool = {
             UIApplication.shared.isProtectedDataAvailable
         },
         timeZone: TimeZone = .current) {
        self.configProvider = configProvider
        self.reader = reader
        self.poster = poster
        self.protectedDataAvailable = protectedDataAvailable
        self.timeZone = timeZone
    }

    /// The default start date: `defaultDays` before the start of today.
    static func defaultStart(now: Date = Date(), calendar: Calendar = .current) -> Date {
        calendar.date(byAdding: .day, value: -defaultDays, to: calendar.startOfDay(for: now))
            ?? now.addingTimeInterval(-Double(defaultDays) * 86_400)
    }

    /// Split records into upload pages of at most `size` (pure).
    nonisolated static func pages(_ records: [WorkoutRecord],
                                  size: Int = JesseBridgeClient.workoutRecordPageSize)
        -> [[WorkoutRecord]] {
        let n = max(1, size)
        return stride(from: 0, to: records.count, by: n).map {
            Array(records[$0..<min(records.count, $0 + n)])
        }
    }

    /// Run the backfill from `since`, in the foreground with the screen kept awake (a locked
    /// phone cannot read Health). A second tap while one runs does nothing.
    func run(since: Date) async {
        guard !isRunning else { return }
        let cfg = configProvider()
        guard cfg.isConfigured else { phase = .failed("No bridge is configured."); return }
        guard protectedDataAvailable() else {
            phase = .failed("Unlock the phone first: Apple Health cannot be read while it is locked.")
            return
        }
        UIApplication.shared.isIdleTimerDisabled = true
        defer { UIApplication.shared.isIdleTimerDisabled = false }

        counts = Counts()
        phase = .running(read: 0, total: 0)
        let started = Date()
        let zone = timeZone
        let poster = self.poster
        do {
            try await reader(since, JesseBridgeClient.workoutRecordPageSize) { [weak self] summaries, read, total in
                let records = WorkoutRecordBuilder.records(for: summaries, timeZone: zone)
                var add = Counts()
                for page in Self.pages(records) {
                    let result = try await poster(cfg, page)
                    add.sent += page.count
                    add.written += result.written
                    add.unchanged += result.unchanged
                }
                await self?.progress(add, read: read, total: total)
            }
            phase = .finished(counts)
            let secs = Int(Date().timeIntervalSince(started).rounded())
            Log.health.notice("workout backfill: \(self.counts.sent) sent, \(self.counts.written) written, \(self.counts.unchanged) unchanged in \(secs) s")
        } catch {
            phase = .failed(error.localizedDescription)
            Log.health.error("workout backfill stopped after \(self.counts.sent) sent: \(error.localizedDescription)")
        }
    }

    private func progress(_ add: Counts, read: Int, total: Int) {
        counts.sent += add.sent
        counts.written += add.written
        counts.unchanged += add.unchanged
        phase = .running(read: read, total: total)
    }
}
