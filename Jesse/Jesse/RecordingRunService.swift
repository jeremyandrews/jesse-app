import BackgroundTasks
import SwiftData
import UIKit
import UserNotifications
import JesseCore
import JesseSpeech

// The app's half of a recording run that keeps going when Jesse leaves the screen.
//
// `RecordingRuns` (JesseKit) owns the runs and decides where a transcript goes; this file
// connects it to what only the app has: the background upload session, the foreground
// clock, the conversation's saved draft, notifications, and the system's continued
// processing task that keeps the process running, visibly, while a run is doing work.
//
// It is a singleton for the same reason `AppDelegate.delivery` is: iOS relaunches the app
// straight into the BACKGROUND to hand over an upload's answer or a completion push, and
// there no scene, no view and no environment exists. Everything here must work from the
// app delegate alone.

@MainActor
final class RecordingRunService {
    static let shared = RecordingRunService()

    let runs: RecordingRuns
    let uploader = BackgroundStudioUploader()
    private let continued = ContinuedTranscriptionTasks()
    private var restored = false

    private init() {
        let transport = URLSessionStudioTransport(endpoint: {
            let config = ConfigStore.load()
            return StudioEndpoint(baseURL: config.endpoint("/"), token: config.token)
        }, uploader: uploader)
        let studio = StudioFirstTranscriber(studio: transport, foreground: ForegroundClock.shared)
        runs = RecordingRuns(store: .standard(), studio: studio, foreground: ForegroundClock.shared) { durable in
            // The simulator has no on-device speech locales, so a seam-armed launch names
            // the fixture's own. Nil in every ordinary launch.
            if let locale = RecordingRunUITestSeam.supportedLocale {
                return RecordingAttachment(supportedLocales: { [locale] }, durable: durable)
            }
            return RecordingAttachment(durable: durable)
        }
        runs.deliverToDraft = { Self.deliverToDraft($0, $1) }
        runs.notify = { Self.post($0) }
        runs.runStarted = { [continued] record in
            Self.keepConversation(record.conversationID)
            continued.submit(for: record)
        }
        runs.runProgressed = { [continued] record, update in continued.progress(record, update) }
        runs.runEnded = { [continued] record in continued.finish(record) }
        continued.onExpired = { [runs] in runs.backgroundTimeExpired() }
    }

    /// Launch: reconnect the upload session to transfers a previous process started, then
    /// resume every run still written down. Once per process, whether the launch is to the
    /// foreground or into the background.
    func start() {
        guard !restored else { return }
        restored = true
        uploader.connect()
        runs.restore()
    }

    // MARK: - Delivery

    /// Put a finished transcript into the conversation's saved composer draft, after
    /// whatever was already there, exactly as the composer would have. Never sent.
    private static func deliverToDraft(_ conversationID: UUID, _ done: CompletedRecording) -> Bool {
        let context = AppModelContainer.shared.container.mainContext
        let descriptor = FetchDescriptor<JesseThread>(predicate: #Predicate { $0.id == conversationID })
        guard let found = try? context.fetch(descriptor), !found.isEmpty else {
            Log.run.notice("transcript for a conversation that no longer exists: dropped")
            return false
        }
        let store = ComposerDraftStore.shared
        let saved = store.snapshot(for: conversationID)
        store.capture(ComposerDraftCapture(text: done.messageBody(typed: saved.text),
                                           files: saved.files,
                                           pendingRecording: nil,
                                           contextLabel: saved.contextLabel),
                      for: conversationID)
        // The draft write is asynchronous. Finishing it is a few milliseconds of work the
        // process may be suspended in the middle of, so ask for the time to finish it.
        let assertion = DraftWriteAssertion()
        assertion.begin()
        Task { @MainActor in
            await store.settle()
            assertion.end()
        }
        return true
    }

    /// A run started in a conversation that may never have been saved (a share opens a
    /// fresh one). Save it now, so the transcript has a conversation to land in even if
    /// the process is terminated before the composer is ever left.
    private static func keepConversation(_ conversationID: UUID) {
        let context = AppModelContainer.shared.container.mainContext
        guard context.hasChanges else { return }
        do {
            try context.save()
        } catch {
            Log.run.error("saving the recording's conversation failed: \(error.localizedDescription)")
        }
    }

    // MARK: - Notifications

    /// The wire key the notification carries, the same one a push uses, so a tap opens the
    /// conversation through `PushTap` either way.
    private static func post(_ notice: RecordingRunNotice) {
        let content = UNMutableNotificationContent()
        content.sound = .default
        let conversationID: UUID
        switch notice {
        case .ready(let id, let name):
            conversationID = id
            content.title = "Transcript ready"
            content.body = "“\(name)” is in the conversation’s composer, not sent."
        case .failed(let id, let message):
            conversationID = id
            content.title = "Transcription failed"
            content.body = message
        case .needsApp(let id, let name):
            conversationID = id
            content.title = "Open Jesse to finish “\(name)”"
            content.body = "The Studio couldn’t transcribe it, and this iPhone can only read it with Jesse open."
        }
        content.userInfo = [BackgroundDelivery.PayloadKey.conversationId: conversationID.uuidString]
        let request = UNNotificationRequest(identifier: "recording-\(conversationID.uuidString)",
                                            content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if let error { Log.push.error("recording notification failed: \(error.localizedDescription)") }
        }
    }
}

// MARK: - The continued processing task

/// One `BGContinuedProcessingTask` per run in flight: the system's visible "Jesse is
/// transcribing" progress, and the time it grants the process to keep uploading and
/// following while the owner is elsewhere.
///
/// Expiry is not failure. When the system takes the time back, the run stays written down,
/// the Studio keeps working, and the completion push (or the next launch) finishes it.
@MainActor
final class ContinuedTranscriptionTasks {
    /// The wildcard `BGTaskSchedulerPermittedIdentifiers` lists as `<prefix>.*`.
    nonisolated static let prefix = "com.tag1.Jesse.transcription"

    var onExpired: () -> Void = {}
    private var tasks: [UUID: BGContinuedProcessingTask] = [:]
    private var names: [UUID: String] = [:]
    private var registered: Set<String> = []

    nonisolated static func identifier(for record: TranscriptionRunRecord) -> String {
        "\(prefix).\(record.id.uuidString.lowercased())"
    }

    func submit(for record: TranscriptionRunRecord) {
        let identifier = Self.identifier(for: record)
        let runID = record.id
        names[runID] = record.sourceName
        if !registered.contains(identifier) {
            // Registration after launch is allowed for this kind of task, and the handler is
            // per identifier, so it is registered once, just before its only submission.
            let ok = BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: .main) { task in
                MainActor.assumeIsolated {
                    guard let task = task as? BGContinuedProcessingTask else {
                        task.setTaskCompleted(success: false)
                        return
                    }
                    self.began(task, runID: runID)
                }
            }
            guard ok else {
                Log.run.error("continued-processing registration refused for \(identifier)")
                return
            }
            registered.insert(identifier)
        }
        let request = BGContinuedProcessingTaskRequest(identifier: identifier,
                                                       title: "Transcribing “\(record.sourceName)”",
                                                       subtitle: "Sending it to the Studio")
        request.strategy = .queue
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // Not fatal: the run goes on while the app is in front, and the push and the
            // store cover the rest. Logged so a misconfigured identifier is visible.
            Log.run.error("continued-processing submit failed: \(error.localizedDescription)")
        }
    }

    private func began(_ task: BGContinuedProcessingTask, runID: UUID) {
        guard names[runID] != nil else {
            // The run already ended before the system started the task.
            task.setTaskCompleted(success: true)
            return
        }
        task.progress.totalUnitCount = 1_000
        task.expirationHandler = { [weak self] in
            MainActor.assumeIsolated {
                self?.tasks[runID] = nil
                self?.onExpired()
                task.setTaskCompleted(success: false)
            }
        }
        tasks[runID] = task
    }

    func progress(_ record: TranscriptionRunRecord, _ update: TranscriptionUpdate) {
        guard let task = tasks[record.id] else { return }
        task.progress.completedUnitCount = Int64(Self.overall(update) * 1_000)
        task.updateTitle(task.title,
                         subtitle: RecordingProgressBar.label(for: update, sourceName: record.sourceName))
    }

    func finish(_ record: TranscriptionRunRecord) {
        names[record.id] = nil
        guard let task = tasks.removeValue(forKey: record.id) else { return }
        task.progress.completedUnitCount = task.progress.totalUnitCount
        task.setTaskCompleted(success: true)
    }

    /// One bar across the whole run, from each phase's own fraction.
    nonisolated static func overall(_ update: TranscriptionUpdate) -> Double {
        let span: (Double, Double)
        switch update.phase {
        case .preparing: span = (0, 0.02)
        case .uploading: span = (0.02, 0.25)
        case .queued, .downloadingModel, .conditioning: span = (0.25, 0.3)
        case .transcribing: span = (0.3, 0.8)
        case .secondReading: span = (0.8, 0.95)
        case .reconciling: span = (0.95, 1)
        }
        return span.0 + (span.1 - span.0) * update.fraction
    }
}

/// The few milliseconds of background time a draft write needs, asked for and given back.
@MainActor
private final class DraftWriteAssertion {
    private var id = UIBackgroundTaskIdentifier.invalid

    func begin() {
        id = UIApplication.shared.beginBackgroundTask(withName: "transcript-draft") { [weak self] in
            MainActor.assumeIsolated { self?.end() }
        }
    }

    func end() {
        guard id != .invalid else { return }
        UIApplication.shared.endBackgroundTask(id)
        id = .invalid
    }
}
