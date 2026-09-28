import Foundation
import Observation

// The whole flow of attaching a recording, as one model both apps drive.
//
// It lives here rather than inside a composer view because it is the same flow twice
// over — the iPhone's file picker, the iPhone's share sheet, and the Mac's file picker
// all converge on it — and because it owns the two promises that must not be
// re-implemented per platform:
//
//   1. THE WORKING COPY IS DELETED ON EVERY WAY OUT. Success, cancellation, a language
//      sheet dismissed without choosing, and an explicit Discard. There is exactly one
//      place that ends a run (`settle`) and it is the only place that clears state, so
//      "the transcript arrived but the audio is still on disk" is not a state this type
//      can be in. A FAILED run is the one exception, and a bounded one: its recording is
//      kept so the owner can "Try again with" another engine (a local engine failing was
//      the day the owner had no transcript at all), until they retry, discard it, pick
//      another recording, or the launch sweep deletes it after a day.
//   2. EVERY FAILURE HAS ITS OWN SENTENCE. The taxonomy is `TranscriptionFailure`'s; this
//      only pairs it with the name of the file the user actually picked.
//
// Everything it depends on is injected — the transcriber, the probe, the storage, the
// list of supported languages and the memory of the last one — so the tests drive real
// state transitions with no speech model, no microphone and no permission prompt.

/// A recording that has been transcribed, ready to become a message.
public struct CompletedRecording: Equatable, Sendable {
    public let sourceName: String
    public let durationSeconds: Double
    /// The human name of the language it was read in, as it appears in the header.
    public let language: String
    public let transcript: String
    /// Where it was transcribed — the Studio and its engines, or this device.
    public let engine: String?
    /// Where two engines disagreed, for the uncertainty block under the transcript.
    public let disagreements: [TranscriptDisagreement]
    public let notes: [String]

    public init(sourceName: String, durationSeconds: Double, language: String, transcript: String,
                engine: String? = nil, disagreements: [TranscriptDisagreement] = [],
                notes: [String] = []) {
        self.sourceName = sourceName
        self.durationSeconds = durationSeconds
        self.language = language
        self.transcript = transcript
        self.engine = engine
        self.disagreements = disagreements
        self.notes = notes
    }

    /// The message body for a composer currently holding `typed`.
    ///
    /// Composed at the moment the transcript lands rather than when the file was picked,
    /// so anything typed WHILE an hour of audio was transcribing is kept.
    public func messageBody(typed: String) -> String {
        RecordingTranscript.messageBody(typed: typed,
                                        sourceName: sourceName,
                                        seconds: durationSeconds,
                                        language: language,
                                        transcript: transcript,
                                        engine: engine,
                                        disagreements: disagreements,
                                        notes: notes)
    }
}

/// Where the audio came from, and what has to be cleaned up when it is done with.
public struct RecordingSource: Equatable, Sendable {
    /// The name the user knows the file by, and the one stamped into the header.
    public let displayName: String
    /// The app's own copy, which this model created and this model deletes.
    public let workingURL: URL
    public let durationSeconds: Double
    /// Set when the audio arrived through the share extension, so the app-group
    /// hand-off is discarded alongside the working copy.
    public let handoff: PendingRecording?

    public init(displayName: String, workingURL: URL, durationSeconds: Double,
                handoff: PendingRecording? = nil) {
        self.displayName = displayName
        self.workingURL = workingURL
        self.durationSeconds = durationSeconds
        self.handoff = handoff
    }
}

/// What makes a run DURABLE: it belongs to a conversation, is written down in a store from
/// the moment the language is confirmed, and is sent through the Studio path that can name
/// its upload and be resumed by id. The iPhone's runs are all durable; the Mac's are not,
/// and behave exactly as they always have.
public struct DurableRecordingRun: Sendable {
    public let conversationID: UUID
    public let store: TranscriptionRunStore
    public let studio: StudioFirstTranscriber

    public init(conversationID: UUID, store: TranscriptionRunStore, studio: StudioFirstTranscriber) {
        self.conversationID = conversationID
        self.store = store
        self.studio = studio
    }
}

/// How a durable run ended, for whoever keeps it.
public enum RecordingRunEnding: Equatable, Sendable {
    /// `completed` holds the transcript.
    case completed
    /// `errorMessage` holds the sentence.
    case failed(String)
    case cancelled
}

/// What a durable run tells its keeper, on the main actor.
@MainActor
public struct RecordingRunEvents {
    public var started: (TranscriptionRunRecord) -> Void = { _ in }
    /// The record changed: the Studio answered the upload.
    public var updated: (TranscriptionRunRecord) -> Void = { _ in }
    public var progressed: (TranscriptionRunRecord, TranscriptionUpdate) -> Void = { _, _ in }
    /// The run needs this device's engine and the app is in the background, so it waits.
    public var waitingForApp: (TranscriptionRunRecord, String) -> Void = { _, _ in }
    /// Called BEFORE the run's audio and record are deleted, so a completed transcript can
    /// be delivered first.
    public var ended: (TranscriptionRunRecord, RecordingRunEnding) -> Void = { _, _ in }

    public init() {}
}

@MainActor
@Observable
public final class RecordingAttachment {
    /// What the composer should currently be showing.
    public enum Stage: Equatable, Sendable {
        /// Nothing in flight.
        case idle
        /// A readable recording is in hand and the language picker is up.
        case choosingLanguage
        /// Transcribing. The payload is what the progress view draws.
        case running(TranscriptionUpdate)
    }

    public private(set) var stage: Stage = .idle
    /// The one sentence explaining the last failure, or nil. Never a generic one.
    public private(set) var errorMessage: String?
    /// Set when the last transcript was made somewhere other than the Studio — it could not
    /// be reached — so the composer can say so beside the text. Not an error: the transcript
    /// arrived. It is the difference between a weaker reading the user knows about and one
    /// they do not.
    public private(set) var notice: String?
    /// The languages the picker offers, device-preferred first.
    public private(set) var languages: [Locale] = []
    /// The picker's selection. Pre-set by `resolve`; writable because the picker binds
    /// to it.
    public var selectedLanguage: Locale?
    /// The file currently in hand, for the picker's title and the error sentences.
    public private(set) var sourceName: String = ""
    /// A finished transcript waiting to be moved into the composer. Read it with
    /// `takeCompleted()`, which also clears it, so one recording can never land twice.
    public private(set) var completed: CompletedRecording?
    /// The engines the paired bridge offers, loaded when a recording is picked. Nil from a
    /// bridge that offers no choice, and then the picker shows none.
    public private(set) var engineMenu: SpeechEngineMenu?
    /// The engine chosen for this recording, or nil for the Studio's own default. The picker
    /// binds to it; it is sent as the upload's `engine`.
    public var selectedEngine: String?
    /// A failed run's recording, kept for another try. Nil when there is none.
    public private(set) var retry: RetryOffer?

    /// What the composer offers after a failure: the same recording, again, with any engine
    /// the bridge lists (or just again, when it lists none).
    public struct RetryOffer: Equatable, Sendable {
        public let sourceName: String
        public let engines: [SpeechEngineOption]
    }

    public var isBusy: Bool {
        if case .running = stage { return true }
        return false
    }

    /// Whether a recording is in hand at all — the language picker is up, or the engine is
    /// reading it. Wider than `isBusy`, which is only the transcription itself.
    ///
    /// The composer's durable draft records this: a run that is in flight when the composer
    /// goes away does NOT survive, and cannot, because the working copy is deleted on every
    /// exit path (and swept at the next launch for a run the system killed). So the draft
    /// notes the name and the restored composer says the recording is gone, rather than
    /// handing back text that has quietly lost its transcript.
    ///
    /// A failed recording kept for another try counts: it is still in hand.
    public var isInFlight: Bool { stage != .idle || retry != nil }

    private let transcriber: any AudioFileTranscribing
    private let probe: any AudioFileProbing
    private let workingCopy: RecordingWorkingCopy
    private let handoffStore: RecordingHandoffStore?
    private let supportedLocales: @Sendable () async -> [Locale]
    private let preferredLanguages: @Sendable () -> [String]
    private let readLastLanguage: @Sendable () -> String?
    private let writeLastLanguage: @Sendable (String) -> Void
    private let loadEngineMenu: @Sendable () async -> SpeechEngineMenu?
    /// The language a kept recording was read in, for its retry.
    private var retainedLocale: Locale?
    /// The language of the run in progress, kept if it fails.
    private var runLocale: Locale?

    /// Set for the iPhone's runs, which outlive their screen; nil for the Mac's.
    public let durable: DurableRecordingRun?
    /// What this run tells its keeper. Set by `RecordingRuns`.
    var events = RecordingRunEvents()
    /// The durable run's record, while one is in flight.
    public private(set) var record: TranscriptionRunRecord?

    private var source: RecordingSource?
    private var work: Task<Void, Never>?
    /// Bumped every time a run ends. Callbacks carry the generation they were started
    /// under, so anything arriving after a cancel (or after a second recording has been
    /// picked) is recognised as belonging to a run that is over, and dropped.
    private var generation = 0

    public init(transcriber: any AudioFileTranscribing = SpeechAnalyzerFileTranscriber(),
                probe: any AudioFileProbing = AVAudioFileProbe(),
                workingCopy: RecordingWorkingCopy = .standard(),
                handoffStore: RecordingHandoffStore? = RecordingHandoffStore.shared(),
                supportedLocales: @escaping @Sendable () async -> [Locale]
                    = { await SpeechTranscriptionSupport.supportedLocales() },
                preferredLanguages: @escaping @Sendable () -> [String]
                    = { Locale.preferredLanguages },
                readLastLanguage: @escaping @Sendable () -> String?
                    = { UserDefaults.standard.string(forKey: RecordingAttachment.lastLanguageKey) },
                writeLastLanguage: @escaping @Sendable (String) -> Void
                    = { UserDefaults.standard.set($0, forKey: RecordingAttachment.lastLanguageKey) },
                engineMenu: (@Sendable () async -> SpeechEngineMenu?)? = nil,
                durable: DurableRecordingRun? = nil) {
        self.durable = durable
        self.transcriber = durable?.studio ?? transcriber
        self.probe = probe
        self.workingCopy = workingCopy
        self.handoffStore = handoffStore
        self.supportedLocales = supportedLocales
        self.preferredLanguages = preferredLanguages
        self.readLastLanguage = readLastLanguage
        self.writeLastLanguage = writeLastLanguage
        // The Studio transcriber knows how to ask the bridge; any other transcriber (this
        // device's own) has no engines to offer.
        if let engineMenu {
            self.loadEngineMenu = engineMenu
        } else if let studio = durable?.studio ?? (transcriber as? StudioFirstTranscriber) {
            self.loadEngineMenu = { await studio.engineMenu() }
        } else {
            self.loadEngineMenu = { nil }
        }
    }

    /// The remembered language is one device-wide preference, not a per-conversation
    /// one: someone who records in Italian records in Italian everywhere.
    ///
    /// `nonisolated` because the default `UserDefaults` accessors above are `@Sendable`
    /// closures, and a main-actor-isolated constant is not readable from one.
    public nonisolated static let lastLanguageKey = "jesse.recording.lastLanguage"

    // MARK: - Starting

    /// Adopt a file the user picked. `url` is the picker's URL, which may be
    /// security-scoped and is never used after this call returns.
    public func begin(pickedFileAt url: URL, displayName: String? = nil) async {
        // A new recording means the owner has moved on from a failed one kept for a retry.
        discardRecording()
        // One run per conversation: a durable run in flight is never replaced by a second
        // recording, whose adoption would take over its source and its record.
        if durable != nil, isInFlight { return }
        let name = displayName ?? url.lastPathComponent
        let working: URL
        do {
            working = try workingCopy.adopt(copying: url)
        } catch {
            fail(.unreadableFile, named: name, run: generation)
            return
        }
        await adopt(RecordingSource(displayName: name,
                                    workingURL: working,
                                    durationSeconds: 0))
    }

    /// Adopt a recording the share extension left in the app group.
    ///
    /// It is copied out of the group container into the app's own working directory and
    /// the hand-off is remembered, so BOTH copies are deleted when the run ends. Leaving
    /// the audio in the group container to transcribe in place would be one fewer copy
    /// and one more way to leave a recording behind.
    public func begin(handoff: PendingRecording) async {
        discardRecording()
        if durable != nil, isInFlight { return }
        guard let handoffStore else {
            fail(.engineFailed(reason: "the shared container isn’t available"),
                 named: handoff.originalName, run: generation)
            return
        }
        let working: URL
        do {
            working = try workingCopy.adopt(copying: handoffStore.audioURL(for: handoff))
        } catch {
            handoffStore.discard(handoff)
            fail(.unreadableFile, named: handoff.originalName, run: generation)
            return
        }
        await adopt(RecordingSource(displayName: handoff.originalName,
                                    workingURL: working,
                                    durationSeconds: handoff.durationSeconds ?? 0,
                                    handoff: handoff))
    }

    /// Probe the working copy, load the language list, and raise the picker.
    private func adopt(_ candidate: RecordingSource) async {
        errorMessage = nil
        notice = nil
        completed = nil
        sourceName = candidate.displayName

        let facts: AudioFileFacts
        do {
            facts = try probe.facts(forFileAt: candidate.workingURL)
        } catch let failure as TranscriptionFailure {
            source = candidate
            fail(failure, named: candidate.displayName, run: generation)
            return
        } catch {
            source = candidate
            fail(.unreadableFile, named: candidate.displayName, run: generation)
            return
        }

        let supported = await supportedLocales()
        guard !supported.isEmpty else {
            source = candidate
            fail(.engineFailed(reason: "this device has no speech transcription available"),
                 named: candidate.displayName, run: generation)
            return
        }

        source = RecordingSource(displayName: candidate.displayName,
                                 workingURL: candidate.workingURL,
                                 durationSeconds: facts.durationSeconds,
                                 handoff: candidate.handoff)
        languages = TranscriptionLocalePolicy.menu(supported: supported,
                                                   preferred: preferredLanguages())
        selectedLanguage = TranscriptionLocalePolicy.resolve(remembered: readLastLanguage(),
                                                            preferred: preferredLanguages(),
                                                            supported: supported)
        // Each recording starts on the Studio's own default; choosing a hosted engine is a
        // decision made for this recording, never one carried over from the last.
        engineMenu = await loadEngineMenu()
        selectedEngine = nil
        stage = .choosingLanguage
    }

    // MARK: - Running

    /// The picker's confirm. Remembers the language and starts the transcription.
    public func confirmLanguage() {
        guard case .choosingLanguage = stage,
              let source, let locale = selectedLanguage else { return }
        writeLastLanguage(locale.identifier)
        start(source: source, locale: locale)
    }

    /// Start a run over `source`: the picker's confirm, or a retry of a kept recording.
    private func start(source: RecordingSource, locale: Locale, retrying: TranscriptionRunRecord? = nil) {
        runLocale = locale
        let language = TranscriptionLocalePolicy.displayName(locale)
        stage = .running(TranscriptionUpdate(phase: .preparing, fraction: 0))

        if let durable {
            if let retrying {
                restartDurable(durable, record: retrying, locale: locale)
            } else {
                startDurable(durable, source: source, locale: locale, language: language)
            }
            return
        }

        let transcriber = self.transcriber
        let engine = selectedEngine
        let url = source.workingURL
        let name = source.displayName
        let run = generation
        // The task inherits this method's MainActor isolation, so every `self?.` below is
        // an ordinary main-actor call rather than a hop. `onProgress` holds the model for
        // as long as `transcribe` runs — that is what a progress callback is — and the
        // hold ends when the call returns or `cancel()` cancels the task.
        work = Task { [weak self] in
            let onProgress: @Sendable (TranscriptionUpdate) -> Void = { update in
                Task { @MainActor in self?.advance(update, run: run) }
            }
            do {
                let result: TranscriptionResult
                if let studio = transcriber as? StudioFirstTranscriber {
                    result = try await studio.transcribe(fileAt: url, locale: locale,
                                                         options: StudioUploadOptions(engine: engine),
                                                         hooks: .none, onProgress: onProgress)
                } else {
                    result = try await transcriber.transcribe(fileAt: url,
                                                              locale: locale,
                                                              onProgress: onProgress)
                }
                self?.succeed(result, language: language, run: run)
            } catch let failure as TranscriptionFailure {
                self?.fail(failure, named: name, run: run)
            } catch is CancellationError {
                self?.fail(.cancelled, named: name, run: run)
            } catch {
                self?.fail(.engineFailed(reason: error.localizedDescription), named: name, run: run)
            }
        }
    }

    /// Progress from the engine, ignored unless it belongs to the run still on screen.
    ///
    /// The race is real rather than theoretical: an engine callback can be mid-hop to
    /// the main actor when Cancel is tapped, and a transcription can finish in the
    /// instant between the tap and the cancellation reaching the engine. Without the
    /// generation check, the first would put the progress view back up after a cancel
    /// and the second would drop a transcript into a composer the user had already
    /// backed out of.
    private func advance(_ update: TranscriptionUpdate, run: Int) {
        guard run == generation, case .running = stage else { return }
        stage = .running(update)
        if let record { events.progressed(record, update) }
    }

    // MARK: - Durable runs

    /// Write the run down and start it. The working copy is MOVED into the store, so the
    /// one copy of the audio is the store's from here until the run ends; a share hand-off
    /// has done its job and is discarded now rather than at the end.
    private func startDurable(_ durable: DurableRecordingRun, source: RecordingSource,
                              locale: Locale, language: String) {
        let id = UUID()
        let fileName: String
        do {
            fileName = try durable.store.adopt(moving: source.workingURL, id: id)
        } catch {
            fail(.unreadableFile, named: source.displayName, run: generation)
            return
        }
        if let handoff = source.handoff { handoffStore?.discard(handoff) }
        let record = TranscriptionRunRecord(id: id,
                                            conversationID: durable.conversationID,
                                            sourceName: source.displayName,
                                            durationSeconds: source.durationSeconds,
                                            language: language,
                                            localeIdentifier: locale.identifier,
                                            audioFileName: fileName,
                                            engine: selectedEngine)
        self.source = RecordingSource(displayName: source.displayName,
                                      workingURL: durable.store.audioURL(for: record),
                                      durationSeconds: source.durationSeconds)
        durable.store.save(record)
        self.record = record
        events.started(record)
        runDurable(durable, record: record, locale: locale)
    }

    /// Start a kept recording again as a NEW run over the same stored audio, with the engine
    /// chosen now.
    private func restartDurable(_ durable: DurableRecordingRun, record kept: TranscriptionRunRecord,
                                locale: Locale) {
        let record = kept.retried(engine: selectedEngine)
        durable.store.replace(kept, with: record)
        self.record = record
        events.started(record)
        runDurable(durable, record: record, locale: locale)
    }

    /// Pick up a run a previous process started. Nothing is asked of the owner: the language
    /// was chosen then, and the audio is in the store.
    public func resume(_ record: TranscriptionRunRecord) {
        guard let durable, stage == .idle else { return }
        errorMessage = nil
        notice = nil
        sourceName = record.sourceName
        source = RecordingSource(displayName: record.sourceName,
                                 workingURL: durable.store.audioURL(for: record),
                                 durationSeconds: record.durationSeconds)
        self.record = record
        stage = .running(TranscriptionUpdate(phase: record.studioRunID == nil ? .uploading : .queued,
                                             fraction: 0, engine: StudioFirstTranscriber.studioName))
        guard FileManager.default.fileExists(atPath: durable.store.audioURL(for: record).path) else {
            fail(.unreadableFile, named: record.sourceName, run: generation)
            return
        }
        let locale = Locale(identifier: record.localeIdentifier)
        runLocale = locale
        runDurable(durable, record: record, locale: locale)
    }

    private func runDurable(_ durable: DurableRecordingRun, record: TranscriptionRunRecord, locale: Locale) {
        let studio = durable.studio
        let url = durable.store.audioURL(for: record)
        let name = record.sourceName
        let language = record.language
        let run = generation
        let options = StudioUploadOptions(tag: record.id.uuidString,
                                          conversationID: record.conversationID.uuidString,
                                          notify: true,
                                          engine: record.engine)
        work = Task { [weak self] in
            let onProgress: @Sendable (TranscriptionUpdate) -> Void = { update in
                Task { @MainActor in self?.advance(update, run: run) }
            }
            let hooks = StudioRunHooks(
                onAccepted: { status in Task { @MainActor in self?.accepted(status, run: run) } },
                onWaitingForApp: { why in Task { @MainActor in self?.waiting(why, run: run) } })
            do {
                let result: TranscriptionResult
                if let runID = record.studioRunID {
                    result = try await studio.resume(runID: runID, fileAt: url, locale: locale,
                                                     hooks: hooks, onProgress: onProgress)
                } else {
                    result = try await studio.transcribe(fileAt: url, locale: locale, options: options,
                                                         hooks: hooks, onProgress: onProgress)
                }
                self?.succeed(result, language: language, run: run)
            } catch let failure as TranscriptionFailure {
                self?.fail(failure, named: name, run: run)
            } catch is CancellationError {
                self?.fail(.cancelled, named: name, run: run)
            } catch {
                self?.fail(.engineFailed(reason: error.localizedDescription), named: name, run: run)
            }
        }
    }

    /// The Studio answered the upload: write its id down, so a relaunch follows this run
    /// rather than sending the recording again.
    private func accepted(_ status: StudioRunStatus, run: Int) {
        guard run == generation, var record, let durable else { return }
        record.studioRunID = status.id
        record.bridgeWillPush = status.notify ?? false
        durable.store.save(record)
        self.record = record
        events.updated(record)
    }

    private func waiting(_ why: String, run: Int) {
        guard run == generation, let record else { return }
        events.waitingForApp(record, why)
    }

    /// Show a failure that happened while nobody was looking (restored from the store).
    func present(error message: String) {
        errorMessage = message
    }

    /// Bring back a run a previous process kept after it failed: its sentence and its
    /// "Try again with" offer, and nothing started.
    public func restoreKept(_ record: TranscriptionRunRecord) async {
        guard let durable, stage == .idle, record.isKeptAfterFailure else { return }
        let url = durable.store.audioURL(for: record)
        guard FileManager.default.fileExists(atPath: url.path) else {
            durable.store.remove(record)
            return
        }
        sourceName = record.sourceName
        source = RecordingSource(displayName: record.sourceName, workingURL: url,
                                 durationSeconds: record.durationSeconds)
        self.record = record
        retainedLocale = Locale(identifier: record.localeIdentifier)
        errorMessage = record.failureMessage
        engineMenu = await loadEngineMenu()
        retry = RetryOffer(sourceName: record.sourceName, engines: engineMenu?.engines ?? [])
    }

    // MARK: - After a failure

    /// Transcribe the kept recording again, with `engine` (`local`, `hosted:<id>`, or nil for
    /// the Studio's default).
    public func retry(engine: String?) {
        guard retry != nil, stage == .idle, let source, let locale = retainedLocale else { return }
        retry = nil
        errorMessage = nil
        notice = nil
        selectedEngine = engine
        retainedLocale = nil
        start(source: source, locale: locale, retrying: record)
    }

    /// The owner's Discard: delete the kept recording now.
    public func discardRecording() {
        guard retry != nil else { return }
        retry = nil
        errorMessage = nil
        retainedLocale = nil
        // Its ending was already told when it failed.
        settle(.cancelled, tell: false)
    }

    /// Whether another engine could do better than the one that failed. A file that is not
    /// audio will not become audio, and a cancel is a decision; anything else might.
    private static func isWorthRetrying(_ failure: TranscriptionFailure) -> Bool {
        switch failure {
        case .cancelled, .unreadableFile: return false
        default: return true
        }
    }

    /// Keep a failed run's recording for "Try again with", instead of deleting it.
    private func keep(after failure: TranscriptionFailure, message: String, locale: Locale) {
        if var record, let durable {
            record.failureMessage = message
            record.failedAt = Date()
            durable.store.save(record)
            self.record = record
            // After the save, which clears the conversation's unseen failure: the keeper
            // records it again when the owner was not looking.
            events.ended(record, .failed(message))
        }
        retainedLocale = locale
        runLocale = nil
        retry = RetryOffer(sourceName: sourceName, engines: engineMenu?.engines ?? [])
        stage = .idle
        generation &+= 1
        // A run resumed after a relaunch never loaded the menu; ask now, so the offer lists
        // the engines rather than only "again".
        if engineMenu == nil {
            let load = loadEngineMenu
            let name = sourceName
            Task { [weak self] in
                let menu = await load()
                guard let self, self.retry?.sourceName == name, let menu else { return }
                self.engineMenu = menu
                self.retry = RetryOffer(sourceName: name, engines: menu.engines)
            }
        }
    }

    /// The user's Cancel. Stops the engine and deletes the audio; says nothing, because
    /// the user already knows.
    public func cancel() {
        work?.cancel()
        work = nil
        settle(.cancelled)
    }

    /// The language sheet dismissed without choosing: the same disposal as a cancel.
    public func abandon() {
        guard case .choosingLanguage = stage else { return }
        settle(.cancelled)
    }

    public func dismissError() { errorMessage = nil }

    /// Put the "not transcribed on the Studio" notice away.
    public func dismissNotice() { notice = nil }

    /// Take the finished transcript, clearing it. The composer calls this once.
    public func takeCompleted() -> CompletedRecording? {
        defer { completed = nil }
        return completed
    }

    // MARK: - Ending

    private func succeed(_ result: TranscriptionResult, language: String, run: Int) {
        guard run == generation, let source else { return }
        completed = CompletedRecording(sourceName: source.displayName,
                                       durationSeconds: source.durationSeconds,
                                       language: language,
                                       transcript: result.text,
                                       engine: result.engine,
                                       disagreements: result.disagreements,
                                       notes: result.notes)
        // A reading made somewhere other than the Studio says so, beside the composer, for
        // as long as the user wants to see it.
        notice = result.notice
        work = nil
        settle(.completed)
    }

    private func fail(_ failure: TranscriptionFailure, named name: String, run: Int) {
        guard run == generation else { return }
        // A cancel is a decision, not a fault: it deletes the audio and says nothing.
        if failure != .cancelled { errorMessage = failure.message(sourceName: name) }
        work = nil
        // A failed recording is KEPT for "Try again with", when another engine could help and
        // the audio is still here to give it.
        if Self.isWorthRetrying(failure), let source, let locale = runLocale,
           FileManager.default.fileExists(atPath: source.workingURL.path) {
            keep(after: failure, message: failure.message(sourceName: name), locale: locale)
            return
        }
        settle(failure == .cancelled ? .cancelled : .failed(failure.message(sourceName: name)))
    }

    /// The ONE way a run ends. Deletes the working copy and the hand-off, then returns
    /// to idle. Every terminal path goes through here, which is what makes "no copy of
    /// the audio remains on disk" a property of the type rather than a habit. (A failed run
    /// kept for a retry has not ended: it ends here when retried to success, discarded, or
    /// replaced.)
    ///
    /// A durable run first tells its keeper how it ended (so a transcript is delivered while
    /// its record still exists), then its record and its audio are deleted from the store.
    /// `tell` is false only for a kept failure being discarded, whose ending was told when
    /// it failed.
    private func settle(_ ending: RecordingRunEnding, tell: Bool = true) {
        if let record, let durable {
            if tell { events.ended(record, ending) }
            durable.store.remove(record)
            self.record = nil
        }
        runLocale = nil
        if let source {
            workingCopy.remove(source.workingURL)
            if let handoff = source.handoff { handoffStore?.discard(handoff) }
        }
        source = nil
        stage = .idle
        generation &+= 1
    }

    /// Launch-time housekeeping: delete any working copy a crash left behind, and sweep
    /// the shared inbox of hand-offs that were never picked up.
    public func sweepAbandonedAudio(now: Date = Date()) {
        workingCopy.purge()
        handoffStore?.sweep(now: now)
    }

    /// Under this module's default (nonisolated) isolation the synthesized deinit for a
    /// `@MainActor` class is already nonisolated, but it is spelled out for the same
    /// reason the search model spells it out: an instance released off the main actor by
    /// a test host must never route through an isolated-deinit executor hop. The
    /// in-flight task holds `self` weakly, so a dropped model leaves nothing running.
    nonisolated deinit {}
}
