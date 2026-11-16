import SwiftUI

/// The Settings row that backfills the Studio's workout detail ledger
/// (`diet-logs/workout-detail.csv` and `workout-splits.csv`) from Apple Health.
struct WorkoutDetailBackfillSection: View {
    @State private var backfill = WorkoutDetailBackfill.shared
    @State private var since = WorkoutDetailBackfill.defaultStart()

    var body: some View {
        Section {
            DatePicker("From", selection: $since, in: ...Date(), displayedComponents: .date)
                .disabled(backfill.isRunning)
            Button {
                Task { await backfill.run(since: since) }
            } label: {
                if backfill.isRunning {
                    HStack { ProgressView(); Text("Backfilling…") }
                } else {
                    Label("Backfill workout details", systemImage: "figure.run")
                }
            }
            .disabled(backfill.isRunning)
            if let status {
                Text(status)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        } footer: {
            Text("Sends every workout since the date above, with every figure Jesse reads for it (heart rate, power, splits, elevation, swim detail), to the workout detail log on your Studio. Safe to run again: a workout already there is only rewritten when something changed. Keep Jesse open and the phone unlocked until it finishes.")
        }
    }

    private var status: String? {
        switch backfill.phase {
        case .idle:
            return nil
        case .running(let read, let total):
            let c = backfill.counts
            return total == 0
                ? "Reading workouts…"
                : "Read \(read) of \(total). Sent \(c.sent), written \(c.written), unchanged \(c.unchanged)."
        case .finished(let c):
            return "Done. Sent \(c.sent), written \(c.written), unchanged \(c.unchanged)."
        case .failed(let why):
            let c = backfill.counts
            return "Stopped: \(why) Sent \(c.sent), written \(c.written), unchanged \(c.unchanged)."
        }
    }
}
