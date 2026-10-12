import SwiftUI

/// The Settings row for the health archive: where the copy to the Studio's archive stands,
/// and the button that runs the first backfill in the foreground. There is no toggle: the
/// archive is on whenever Apple Health is connected and the bridge answers.
struct HealthArchiveSettingsSection: View {
    @State private var archive = HealthArchiveSync.shared

    var body: some View {
        Section {
            LabeledContent("Health archive", value: stateText)
            if let last = archive.lastSync {
                LabeledContent("Last sync") {
                    Text(last, format: .relative(presentation: .named))
                }
            }
            if let error = archive.lastError {
                LabeledContent("Last error", value: error)
            }
            Button {
                Task { await archive.startBackfill() }
            } label: {
                if archive.isRunning {
                    HStack { ProgressView(); Text("Syncing…") }
                } else {
                    Label("Start backfill", systemImage: "arrow.up.doc")
                }
            }
            .disabled(archive.isRunning || archive.state == .off)
        } footer: {
            Text("Copies every Apple Health sample Jesse can read, every workout and every workout route to the archive on your Studio. The first backfill reads your whole history: start it with the phone unlocked and on a charger, and keep Jesse open until it finishes. After that, new data follows on its own.")
        }
    }

    private var stateText: String {
        switch archive.state {
        case .off: return "Off"
        case .notConfigured: return "Not configured on the bridge"
        case .upToDate: return "Up to date"
        case .backfilling(let sent, let done, let total):
            return archive.isRunning
                ? "Backfilling: \(sent) sent, \(done) of \(total) types done"
                : "Backfill incomplete: \(done) of \(total) types done"
        }
    }
}
