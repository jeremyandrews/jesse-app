import Foundation
import Observation

// Who a recording's run BELONGS to, on the iPhone: its conversation, kept by the app.
//
// It used to belong to the composer on screen, as view state, so the run lived and died
// with that view: leaving the conversation dropped the transcript on the floor, and iOS
// terminating the suspended app lost the run and its audio together. Now one owner holds
// one `RecordingAttachment` per conversation for as long as the app runs, writes every
// confirmed run down (`TranscriptionRunStore`), and resumes them at the next launch. The
// composer only OBSERVES the model for its conversation while it is on screen. Leaving,
// switching tabs, locking the phone: none of them touch the run. Only Cancel stops it.
//
// WHERE A FINISHED TRANSCRIPT GOES, which is the whole point:
//
//   * the conversation's composer is on screen and the app is in front: it lands in the
//     composer as it always has (the view takes `completed`);
//   * anywhere else: into that conversation's saved composer draft, with the same header,
//     place line and uncertainty block, never sent; plus a "Transcript ready" notification
//     unless the bridge already pushed one. A composer that is still alive behind a locked
//     screen also gets it back on return (`claim`), since its live text is what the owner
//     will see.

/// Something the owner should be told while the app is not in front of them.
public enum RecordingRunNotice: Equatable, Sendable {
    case ready(conversationID: UUID, sourceName: String)
    case failed(conversationID: UUID, message: String)
    /// The Studio could not do it and this device's engine needs the app in front.
    case needsApp(conversationID: UUID, sourceName: String)
}

@MainActor
@Observable
public final class RecordingRuns {
    @ObservationIgnored private var models: [UUID: RecordingAttachment] = [:]
    /// Conversations whose composer is on screen now.
    @ObservationIgnored private var attached: Set<UUID> = []
    /// Transcripts delivered to the draft of a composer that is still alive but was not in
    /// front when they landed, waiting for it to come back.
    @ObservationIgnored private var claims: [UUID: CompletedRecording] = [:]

    private let store: TranscriptionRunStore
    private let foreground: any ForegroundGating
    private let makeModel: @MainActor (DurableRecordingRun) -> RecordingAttachment
    private let studio: StudioFirstTranscriber

    /// Put a finished transcript into the conversation's saved draft. Returns false when
    /// the conversation no longer exists, and nothing is delivered.
    public var deliverToDraft: @MainActor (UUID, CompletedRecording) -> Bool = { _, _ in false }
    public var notify: @MainActor (RecordingRunNotice) -> Void = { _ in }
    public var runStarted: @MainActor (TranscriptionRunRecord) -> Void = { _ in }
    public var runProgressed: @MainActor (TranscriptionRunRecord, TranscriptionUpdate) -> Void = { _, _ in }
    public var runEnded: @MainActor (TranscriptionRunRecord) -> Void = { _ in }

    /// - Parameter makeModel: builds the model for one conversation around its durable run;
    ///   the default takes every other dependency's default.
    public init(store: TranscriptionRunStore,
                studio: StudioFirstTranscriber,
                foreground: any ForegroundGating,
                makeModel: @escaping @MainActor (DurableRecordingRun) -> RecordingAttachment
                    = { RecordingAttachment(durable: $0) }) {
        self.store = store
        self.studio = studio
        self.foreground = foreground
        self.makeModel = makeModel
    }

    nonisolated deinit {}

    // MARK: - The model for a conversation

    /// The one model for `conversationID`, made on first ask and kept.
    public func model(for conversationID: UUID) -> RecordingAttachment {
        if let model = models[conversationID] { return model }
        let model = makeModel(DurableRecordingRun(conversationID: conversationID, store: store, studio: studio))
        model.events = events(for: conversationID)
        models[conversationID] = model
        return model
    }

    /// Whether a recording is in hand for this conversation: its reapers must leave it be.
    public func isInFlight(_ conversationID: UUID) -> Bool {
        models[conversationID]?.isInFlight ?? false
    }

    /// Whether this conversation's composer is on screen with the app in front.
    public func isOnScreen(_ conversationID: UUID) -> Bool {
        attached.contains(conversationID) && foreground.isActive
    }

    // MARK: - Launch

    /// Resume every run a previous process left in flight, and bring back the failures
    /// nobody has seen. Call once at launch, after the upload session is connected.
    public func restore() {
        store.sweepUnclaimedAudio()
        for record in store.runs() {
            model(for: record.conversationID).resume(record)
            // Its keeper hears about it as a start: the system's progress and background
            // time belong to this process's run, not the one that died.
            runStarted(record)
        }
        for (conversationID, message) in store.failures() where !isInFlight(conversationID) {
            model(for: conversationID).present(error: message)
        }
    }

    // MARK: - The composer

    /// A composer for `conversationID` is on screen. `restored` is true when it has just
    /// put its saved draft back, which already holds anything delivered while it was gone.
    public func attach(_ conversationID: UUID, restored: Bool) {
        attached.insert(conversationID)
        if restored { claims[conversationID] = nil }
        store.clearFailure(conversationID: conversationID)
    }

    public func detach(_ conversationID: UUID) {
        attached.remove(conversationID)
    }

    /// A transcript that landed in the draft while this still-open composer was not in
    /// front. The composer adds it to its live text, once.
    public func claim(_ conversationID: UUID) -> CompletedRecording? {
        claims.removeValue(forKey: conversationID)
    }

    // MARK: - The system

    /// A completion push arrived for a Studio run. Ask about it once, now, instead of waiting
    /// for the next poll, and return once it is delivered or known to be still going.
    public func refresh(studioRunID: String) async {
        guard let model = models.values.first(where: { $0.record?.studioRunID == studioRunID }),
              let runID = model.record?.id else { return }
        // The run's own polling task picks the answer up the moment the process runs again
        // (or, after a relaunch, `restore` started it); this only waits for it to finish,
        // bounded by the caller's own deadline.
        while model.record?.id == runID {
            try? await Task.sleep(for: .milliseconds(200))
            if Task.isCancelled { return }
        }
    }

    /// The system is taking back the background time a run was given. Nothing is cancelled
    /// and nothing is forgotten: the run stays written down, the Studio keeps working, and
    /// the push or the next launch finishes it.
    public func backgroundTimeExpired() {}

    // MARK: - Events

    private func events(for conversationID: UUID) -> RecordingRunEvents {
        var events = RecordingRunEvents()
        events.started = { [weak self] record in
            self?.store.clearFailure(conversationID: conversationID)
            self?.runStarted(record)
        }
        events.progressed = { [weak self] record, update in self?.runProgressed(record, update) }
        events.waitingForApp = { [weak self] record, _ in
            guard let self, !self.foreground.isActive else { return }
            self.notify(.needsApp(conversationID: conversationID, sourceName: record.sourceName))
        }
        events.ended = { [weak self] record, ending in self?.ended(record, ending) }
        return events
    }

    private func ended(_ record: TranscriptionRunRecord, _ ending: RecordingRunEnding) {
        defer { runEnded(record) }
        let conversationID = record.conversationID
        guard let model = models[conversationID] else { return }
        switch ending {
        case .completed:
            // On screen and in front: the composer takes it, exactly as before.
            if isOnScreen(conversationID) { return }
            guard let done = model.takeCompleted() else { return }
            let delivered = deliverToDraft(conversationID, done)
            if attached.contains(conversationID) { claims[conversationID] = done }
            if delivered && !record.bridgeWillPush {
                notify(.ready(conversationID: conversationID, sourceName: record.sourceName))
            }
        case .failed(let message):
            if isOnScreen(conversationID) { return }
            store.recordFailure(message, conversationID: conversationID)
            if !record.bridgeWillPush {
                notify(.failed(conversationID: conversationID, message: message))
            }
        case .cancelled:
            break
        }
    }
}
