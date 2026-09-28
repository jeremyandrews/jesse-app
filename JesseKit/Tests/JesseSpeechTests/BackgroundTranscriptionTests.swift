import Synchronization
import XCTest
@testable import JesseSpeech

// A recording's run on the iPhone outlives everything that used to end it: the composer
// going away, the app going to the background, the system terminating it. Every test here
// drives the real owner, model, store and transcriber against a scripted Studio, a scripted
// clock and a temporary directory; nothing needs a network, a device or a wait longer than
// a few milliseconds.

// MARK: - Fakes

/// A Studio under the test's control. `polls` are answered in order; once they run out it
/// keeps answering `hold` (running, by default), so a run can be kept in flight at will.
private final class ScriptedStudio: StudioTranscriptionTransport, Sendable {
    private struct State {
        var upload: Result<StudioRunStatus, StudioTransportError>
        var reattach: StudioRunStatus?
        var polls: [Result<StudioRunStatus, StudioTransportError>]
        /// Nil: the call never returns, like a process iOS has suspended.
        var hold: Result<StudioRunStatus, StudioTransportError>?
        var uploads: [StudioUploadOptions] = []
        var cancelled: [String] = []
        var pollCount = 0
    }

    private let state: Mutex<State>

    init(upload: Result<StudioRunStatus, StudioTransportError> = .success(.init(id: "tr-1", state: "running", phase: "queued")),
         reattach: StudioRunStatus? = nil,
         polls: [Result<StudioRunStatus, StudioTransportError>] = [],
         hold: Result<StudioRunStatus, StudioTransportError>? = .success(.init(id: "tr-1", state: "running", phase: "transcribing"))) {
        state = Mutex(State(upload: upload, reattach: reattach, polls: polls, hold: hold))
    }

    var uploads: [StudioUploadOptions] { state.withLock { $0.uploads } }
    var cancelled: [String] { state.withLock { $0.cancelled } }
    var pollCount: Int { state.withLock { $0.pollCount } }

    func answer(with polls: [Result<StudioRunStatus, StudioTransportError>]) {
        state.withLock { $0.polls = polls }
    }

    func upload(fileAt url: URL, contentType: String, language: String,
                onProgress: @escaping @Sendable (Double) -> Void) async throws -> StudioRunStatus {
        try await upload(fileAt: url, contentType: contentType, language: language, options: .plain,
                         onProgress: onProgress)
    }

    func upload(fileAt url: URL, contentType: String, language: String, options: StudioUploadOptions,
                onProgress: @escaping @Sendable (Double) -> Void) async throws -> StudioRunStatus {
        let answer = state.withLock { s -> Result<StudioRunStatus, StudioTransportError> in
            s.uploads.append(options)
            return s.upload
        }
        onProgress(1)
        return try answer.get()
    }

    func reattachUpload(tag: String,
                        onProgress: @escaping @Sendable (Double) -> Void) async throws -> StudioRunStatus? {
        state.withLock { $0.reattach }
    }

    func status(id: String) async throws -> StudioRunStatus {
        let next = state.withLock { s -> Result<StudioRunStatus, StudioTransportError>? in
            s.pollCount += 1
            return s.polls.isEmpty ? s.hold : s.polls.removeFirst()
        }
        guard let next else {
            while true { try await Task.sleep(for: .seconds(3_600)) }
        }
        return try next.get()
    }

    func cancel(id: String) async {
        state.withLock { $0.cancelled.append(id) }
    }
}

/// This device's engine: records that it was asked.
private final class DeviceEngine: AudioFileTranscribing, Sendable {
    private let calls = Mutex(0)
    var called: Int { calls.withLock { $0 } }

    func transcribe(fileAt url: URL, locale: Locale,
                    onProgress: @escaping @Sendable (TranscriptionUpdate) -> Void) async throws -> TranscriptionResult {
        calls.withLock { $0 += 1 }
        return TranscriptionResult(text: "Read on the device.", engine: TranscriptionPlace.thisDevice)
    }
}

private struct Probe: AudioFileProbing {
    func facts(forFileAt url: URL) throws -> AudioFileFacts {
        AudioFileFacts(durationSeconds: 75, sampleRate: 16_000)
    }
}

/// What a hook said, in order.
private final class Said: Sendable {
    private let lines = Mutex<[String]>([])
    var all: [String] { lines.withLock { $0 } }
    func add(_ line: String) { lines.withLock { $0.append(line) } }
}

/// Uptime the test moves by hand.
private final class Uptime: Sendable {
    private let value = Mutex<Double>(1_000)
    var now: Double { value.withLock { $0 } }
    func advance(_ by: Double) { value.withLock { $0 += by } }
}

private func done(_ id: String = "tr-1", text: String = "Il idraulico viene giovedì alle nove.") -> StudioRunStatus {
    StudioRunStatus(id: id, state: "done", phase: "done", fraction: 1, transcript: text,
                    engines: [.init(id: "large", label: "Whisper large-v3", role: "primary")])
}

private func running(_ id: String = "tr-1") -> StudioRunStatus {
    StudioRunStatus(id: id, state: "running", phase: "transcribing", fraction: 0.4)
}

// MARK: - The owner

@MainActor
final class RecordingRunsTests: XCTestCase {

    private let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("runs-\(UUID().uuidString)", isDirectory: true)
    private let conversation = UUID()
    private let uptime = Uptime()

    private var store: TranscriptionRunStore {
        TranscriptionRunStore(directory: root.appendingPathComponent("store", isDirectory: true))
    }
    private var working: RecordingWorkingCopy {
        RecordingWorkingCopy(directory: root.appendingPathComponent("work", isDirectory: true))
    }

    override nonisolated func tearDown() {
        try? FileManager.default.removeItem(at: root)
        super.tearDown()
    }

    private func makeClock(active: Bool = true) -> ForegroundClock {
        let uptime = self.uptime
        return ForegroundClock(active: active, uptime: { uptime.now })
    }

    private func makeRuns(_ studio: ScriptedStudio, device: DeviceEngine = DeviceEngine(),
                          clock: ForegroundClock) -> RecordingRuns {
        let transcriber = StudioFirstTranscriber(studio: studio, onDevice: device,
                                                 pollInterval: .milliseconds(2),
                                                 foreground: clock)
        let working = self.working
        return RecordingRuns(store: store, studio: transcriber, foreground: clock) { durable in
            RecordingAttachment(probe: Probe(),
                                workingCopy: working,
                                handoffStore: nil,
                                supportedLocales: { [Locale(identifier: "it-IT")] },
                                preferredLanguages: { ["it-IT"] },
                                readLastLanguage: { nil },
                                writeLastLanguage: { _ in },
                                durable: durable)
        }
    }

    /// A picked recording, as the file picker hands it over.
    private func pickedFile() throws -> URL {
        let dir = root.appendingPathComponent("picked", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("Plumber.m4a")
        try Data(repeating: 7, count: 4_096).write(to: url)
        return url
    }

    private func audioFiles() -> [String] {
        let dir = store.directory.appendingPathComponent("audio", isDirectory: true)
        return (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
    }

    private func waitUntil(_ what: String = "condition", _ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<1_000 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("timed out waiting for \(what)")
    }

    /// Start a run the way the composer does, and return its model.
    private func start(_ runs: RecordingRuns) async throws -> RecordingAttachment {
        let model = runs.model(for: conversation)
        await model.begin(pickedFileAt: try pickedFile())
        XCTAssertEqual(model.stage, .choosingLanguage)
        model.confirmLanguage()
        return model
    }

    // Leaving the conversation used to drop the run's result: the model was view state.

    func testARunSurvivesItsComposerGoingAwayAndLandsInTheDraft() async throws {
        let studio = ScriptedStudio(polls: [.success(running()), .success(running())],
                                    hold: .success(done()))
        let runs = makeRuns(studio, clock: makeClock())
        var drafts: [UUID: String] = [:]
        var notices: [RecordingRunNotice] = []
        runs.deliverToDraft = { id, done in
            drafts[id] = done.messageBody(typed: drafts[id] ?? "")
            return true
        }
        runs.notify = { notices.append($0) }

        runs.attach(conversation, restored: true)
        _ = try await start(runs)
        XCTAssertEqual(store.runs().count, 1, "the run is written down the moment it starts")
        runs.detach(conversation) // the composer is gone; nothing holds the model but the owner

        try await waitUntil("delivery") { drafts[conversation] != nil }
        let body = try XCTUnwrap(drafts[conversation])
        XCTAssertTrue(body.hasPrefix("Recording: “Plumber.m4a” · 1m 15s · Italian"), body)
        XCTAssertTrue(body.contains("Il idraulico viene giovedì alle nove."))
        XCTAssertEqual(notices, [.ready(conversationID: conversation, sourceName: "Plumber.m4a")],
                       "the bridge was not asked to push here, so the app says it")
        try await waitUntil("cleanup") { store.runs().isEmpty }
        XCTAssertEqual(audioFiles(), [], "the audio is deleted once the transcript is delivered")
        XCTAssertEqual(studio.cancelled, [], "leaving never cancels")
        XCTAssertEqual(studio.uploads.first?.conversationID, conversation.uuidString)
        XCTAssertEqual(studio.uploads.first?.notify, true)
    }

    func testOnScreenTheComposerTakesItAsBefore() async throws {
        let studio = ScriptedStudio(hold: .success(done()))
        let runs = makeRuns(studio, clock: makeClock())
        var delivered = 0
        runs.deliverToDraft = { _, _ in delivered += 1; return true }
        runs.attach(conversation, restored: true)
        let model = try await start(runs)
        try await waitUntil("completion") { model.completed != nil }
        XCTAssertEqual(delivered, 0, "the composer on screen takes it; the draft is not touched")
        XCTAssertNotNil(model.takeCompleted())
    }

    func testAComposerBehindALockedScreenGetsItBackOnReturnAndTheDraftHasItMeanwhile() async throws {
        let studio = ScriptedStudio(polls: [.success(running())], hold: .success(done()))
        let clock = makeClock()
        let runs = makeRuns(studio, clock: clock)
        var draft: String?
        runs.deliverToDraft = { _, done in draft = done.messageBody(typed: "Ciao"); return true }
        runs.attach(conversation, restored: true)
        let model = try await start(runs)
        clock.setActive(false)
        try await waitUntil("delivery") { draft != nil }
        XCTAssertNil(model.completed, "taken for the draft, so the view cannot apply it twice")
        clock.setActive(true)
        XCTAssertNotNil(runs.claim(conversation), "the live composer adds it once on return")
        XCTAssertNil(runs.claim(conversation))
    }

    // Termination used to lose the run and purge its audio at launch.

    func testAPersistedRunResumesAfterARelaunchAndDeliversIntoTheDraft() async throws {
        let studioOne = ScriptedStudio(upload: .success(.init(id: "tr-7", state: "running", phase: "queued",
                                                              notify: true)),
                                       hold: nil)
        let runsOne = makeRuns(studioOne, clock: makeClock())
        _ = try await start(runsOne)
        try await waitUntil("the Studio's id is recorded") { store.runs().first?.studioRunID == "tr-7" }
        XCTAssertEqual(store.runs().first?.bridgeWillPush, true)

        // The process dies. A new one starts with a new owner over the same store.
        let studioTwo = ScriptedStudio(upload: .failure(.refused("must not upload again")),
                                       hold: .success(done("tr-7")))
        let runsTwo = makeRuns(studioTwo, clock: makeClock(active: false))
        var drafts: [UUID: String] = [:]
        var notices: [RecordingRunNotice] = []
        runsTwo.deliverToDraft = { id, done in drafts[id] = done.messageBody(typed: ""); return true }
        runsTwo.notify = { notices.append($0) }
        runsTwo.restore()

        try await waitUntil("delivery after relaunch") { drafts[conversation] != nil }
        XCTAssertTrue(drafts[conversation]?.contains("giovedì") ?? false)
        XCTAssertEqual(studioTwo.uploads, [], "a run the Studio already has is followed, not re-sent")
        XCTAssertEqual(notices, [], "the bridge said it would push, so the app does not notify too")
        try await waitUntil("cleanup") { store.runs().isEmpty }
        XCTAssertEqual(audioFiles(), [])
    }

    func testAnUploadAPreviousProcessStartedIsReattachedNotSentTwice() async throws {
        let record = try seedRecord(studioRunID: nil)
        let studio = ScriptedStudio(upload: .failure(.refused("must not upload again")),
                                    reattach: .init(id: "tr-3", state: "running", phase: "queued"),
                                    hold: .success(done("tr-3")))
        let runs = makeRuns(studio, clock: makeClock())
        var delivered = false
        runs.deliverToDraft = { _, _ in delivered = true; return true }
        runs.restore()
        try await waitUntil("delivery") { delivered }
        XCTAssertEqual(studio.uploads, [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.audioURL(for: record).path))
    }

    // The continued-processing task expiring must not end the run anywhere.

    func testExpiryOfTheBackgroundTimeCancelsNothingAndForgetsNothing() async throws {
        let studio = ScriptedStudio(upload: .success(.init(id: "tr-5", state: "running", phase: "queued")),
                                    hold: nil)
        let runs = makeRuns(studio, clock: makeClock())
        let model = try await start(runs)
        try await waitUntil("the Studio's id") { store.runs().first?.studioRunID == "tr-5" }

        runs.backgroundTimeExpired()
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertEqual(studio.cancelled, [], "the Studio keeps working")
        XCTAssertEqual(store.runs().count, 1, "the run stays written down for the push or the next launch")
        XCTAssertEqual(audioFiles().count, 1)
        XCTAssertTrue(model.isBusy)
    }

    // Only Cancel ends a run early, and it ends it everywhere.

    func testCancelStopsTheStudioRunAndDeletesTheAudioAndTheRecord() async throws {
        let studio = ScriptedStudio(upload: .success(.init(id: "tr-8", state: "running", phase: "queued")),
                                    hold: nil)
        let runs = makeRuns(studio, clock: makeClock())
        var ended: [TranscriptionRunRecord] = []
        runs.runEnded = { ended.append($0) }
        let model = try await start(runs)
        try await waitUntil("the Studio's id") { store.runs().first?.studioRunID == "tr-8" }

        model.cancel()
        try await waitUntil("the Studio cancel") { studio.cancelled == ["tr-8"] }
        XCTAssertEqual(store.runs(), [])
        XCTAssertEqual(audioFiles(), [], "a cancelled run leaves nothing behind")
        XCTAssertEqual(ended.count, 1)
        XCTAssertNil(model.errorMessage, "a cancel says nothing")
    }

    func testAFailureWhileAwayIsKeptUntilTheConversationIsOpened() async throws {
        let failed = StudioRunStatus(id: "tr-1", state: "failed", phase: "failed",
                                     error: .init(kind: "engine_failed", message: "The engine ran out of memory."))
        let runs = makeRuns(ScriptedStudio(hold: .success(failed)), clock: makeClock())
        var notices: [RecordingRunNotice] = []
        runs.notify = { notices.append($0) }
        _ = try await start(runs)
        try await waitUntil("the failure") { !notices.isEmpty }
        guard case .failed(_, let message) = notices[0] else { return XCTFail("\(notices)") }
        XCTAssertTrue(message.contains("ran out of memory"))
        XCTAssertEqual(store.failures()[conversation], message)

        // Relaunch: the conversation still says why when it is opened, then forgets it.
        let again = makeRuns(ScriptedStudio(), clock: makeClock())
        again.restore()
        XCTAssertEqual(again.model(for: conversation).errorMessage, message)
        again.attach(conversation, restored: true)
        XCTAssertNil(store.failures()[conversation])
    }

    /// A failed run keeps its recording for "Try again with": across a relaunch it comes back
    /// as an offer and never starts on its own; a retry sends the chosen engine under a NEW
    /// tag, and its success deletes the audio.
    func testAFailedRunIsKeptAcrossARelaunchAndRetriedWithTheChosenEngine() async throws {
        let failed = StudioRunStatus(id: "tr-1", state: "failed", phase: "failed",
                                     error: .init(kind: "engine_failed", message: "The engine looped."))
        let runs = makeRuns(ScriptedStudio(hold: .success(failed)), clock: makeClock())
        let model = try await start(runs)
        try await waitUntil("the kept failure") { model.retry != nil }
        let kept = try XCTUnwrap(store.runs().first)
        XCTAssertTrue(kept.isKeptAfterFailure)
        XCTAssertEqual(audioFiles().count, 1, "the recording is kept for another try")

        // Relaunch.
        let studio = ScriptedStudio(hold: .success(done()))
        let again = makeRuns(studio, clock: makeClock())
        var delivered: CompletedRecording?
        again.deliverToDraft = { _, done in delivered = done; return true }
        again.restore()
        let restored = again.model(for: conversation)
        try await waitUntil("the offer") { restored.retry != nil }
        XCTAssertEqual(restored.stage, .idle)
        XCTAssertTrue(studio.uploads.isEmpty, "a kept failure never starts itself")
        XCTAssertTrue(restored.errorMessage?.contains("looped") ?? false)

        restored.retry(engine: "hosted:glm")
        try await waitUntil("delivery") { delivered != nil }
        XCTAssertEqual(studio.uploads.first?.engine, "hosted:glm")
        XCTAssertNotEqual(studio.uploads.first?.tag, kept.id.uuidString,
                          "a new tag, so the failed upload's answer cannot be reattached")
        try await waitUntil("cleanup") { store.runs().isEmpty }
        XCTAssertEqual(audioFiles(), [])
    }

    /// Unanswered, a kept failure is swept after a day; Discard deletes it at once.
    func testAKeptFailureIsSweptAfterADayAndDiscardDeletesItAtOnce() async throws {
        let failed = StudioRunStatus(id: "tr-1", state: "failed", phase: "failed",
                                     error: .init(kind: "no_speech", message: "No speech."))
        let runs = makeRuns(ScriptedStudio(hold: .success(failed)), clock: makeClock())
        let model = try await start(runs)
        try await waitUntil("the kept failure") { model.retry != nil }
        XCTAssertEqual(store.sweepExpiredFailures(now: Date()), [], "not yet")
        let later = Date().addingTimeInterval(RecordingHandoffStore.maxAge + 60)
        XCTAssertEqual(store.sweepExpiredFailures(now: later).count, 1)
        XCTAssertEqual(store.runs(), [])
        XCTAssertEqual(audioFiles(), [])

        let second = makeRuns(ScriptedStudio(hold: .success(failed)), clock: makeClock())
        let other = try await start(second)
        try await waitUntil("the kept failure") { other.retry != nil }
        other.discardRecording()
        XCTAssertEqual(store.runs(), [])
        XCTAssertEqual(audioFiles(), [])
        XCTAssertFalse(other.isInFlight)
    }

    func testAPushWaitsForItsRunToLand() async throws {
        let studio = ScriptedStudio(upload: .success(.init(id: "tr-4", state: "running", phase: "queued")),
                                    polls: Array(repeating: .success(running("tr-4")), count: 5),
                                    hold: .success(done("tr-4")))
        let runs = makeRuns(studio, clock: makeClock())
        var delivered = false
        runs.deliverToDraft = { _, _ in delivered = true; return true }
        _ = try await start(runs)
        try await waitUntil("the Studio's id") { store.runs().first?.studioRunID == "tr-4" }
        await runs.refresh(studioRunID: "tr-4")
        XCTAssertTrue(delivered)
        await runs.refresh(studioRunID: "tr-unknown") // returns at once
    }

    func testASecondRecordingCannotTakeOverARunInFlight() async throws {
        let studio = ScriptedStudio(upload: .success(.init(id: "tr-6", state: "running", phase: "queued")),
                                    hold: nil)
        let runs = makeRuns(studio, clock: makeClock())
        let model = try await start(runs)
        try await waitUntil("the Studio's id") { store.runs().first?.studioRunID == "tr-6" }
        let first = try XCTUnwrap(model.record)

        await model.begin(pickedFileAt: try pickedFile())
        XCTAssertTrue(model.isBusy, "still the first run, not a language sheet for a second")
        XCTAssertEqual(model.record, first)
        XCTAssertEqual(store.runs().map(\.id), [first.id])
        XCTAssertEqual(studio.uploads.count, 1)
        model.cancel()
    }

    private func seedRecord(studioRunID: String?) throws -> TranscriptionRunRecord {
        let id = UUID()
        let name = try store.adopt(moving: try pickedFile(), id: id)
        let record = TranscriptionRunRecord(id: id, conversationID: conversation, sourceName: "Plumber.m4a",
                                            durationSeconds: 75, language: "Italian",
                                            localeIdentifier: "it-IT", audioFileName: name,
                                            studioRunID: studioRunID)
        store.save(record)
        return record
    }
}

// MARK: - Silence while suspended

final class ForegroundSilenceTests: XCTestCase {

    private let url = URL(fileURLWithPath: "/tmp/runs/Plumber.m4a")

    /// The script for "the owner locked the phone for ten minutes mid-run": polls fail while
    /// the app is suspended (the process cannot make them), then the run is found finished.
    private func lockedForTenMinutes(clock: ForegroundClock?, uptime: Uptime)
        -> (StudioFirstTranscriber, ScriptedStudio, DeviceEngine) {
        let studio = ScriptedStudio(polls: [
            .success(running()),
            .failure(.lostContact("the network connection was lost")),
            .failure(.lostContact("the network connection was lost")),
            .success(done()),
        ])
        let device = DeviceEngine()
        let sleeps = Mutex(0)
        let transcriber = StudioFirstTranscriber(
            studio: studio, onDevice: device, pollInterval: .seconds(1), contactTolerance: 90,
            sleep: { _ in
                let n = sleeps.withLock { $0 += 1; return $0 }
                if n == 2 { clock?.setActive(false) }
                uptime.advance(n == 2 ? 600 : 1)
                if n == 3 { clock?.setActive(true) }
            },
            now: { uptime.now },
            foreground: clock)
        return (transcriber, studio, device)
    }

    func testSuspendedTimeIsNotLostContact() async throws {
        let uptime = Uptime()
        let clock = ForegroundClock(active: true, uptime: { uptime.now })
        let (t, studio, device) = lockedForTenMinutes(clock: clock, uptime: uptime)
        let result = try await t.transcribe(fileAt: url, locale: Locale(identifier: "it-IT")) { _ in }
        XCTAssertEqual(result.text, "Il idraulico viene giovedì alle nove.")
        XCTAssertEqual(device.called, 0, "ten locked minutes are not a reason to read it here")
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(studio.cancelled, [], "nor to stop the Studio")
    }

    /// The same script on the uptime clock, which is what every run used before: the locked
    /// minutes count as silence, the Studio run is cancelled and the phone reads it itself.
    func testOnTheUptimeClockTheSameLockAbandonsTheStudio() async throws {
        let uptime = Uptime()
        let (t, studio, device) = lockedForTenMinutes(clock: nil, uptime: uptime)
        let result = try await t.transcribe(fileAt: url, locale: Locale(identifier: "it-IT")) { _ in }
        XCTAssertEqual(result.text, "Read on the device.")
        XCTAssertEqual(device.called, 1)
        for _ in 0..<200 where studio.cancelled.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertEqual(studio.cancelled, ["tr-1"])
    }

    func testForegroundSilencePastTheToleranceStillFallsBack() async throws {
        let uptime = Uptime()
        let clock = ForegroundClock(active: true, uptime: { uptime.now })
        let studio = ScriptedStudio(polls: [], hold: .failure(.lostContact("timed out")))
        let device = DeviceEngine()
        let t = StudioFirstTranscriber(studio: studio, onDevice: device, pollInterval: .seconds(1),
                                       contactTolerance: 5, sleep: { _ in uptime.advance(1) },
                                       foreground: clock)
        let result = try await t.transcribe(fileAt: url, locale: Locale(identifier: "it-IT")) { _ in }
        XCTAssertEqual(device.called, 1)
        XCTAssertTrue(result.notice?.contains("contact with it was lost") ?? false)
    }

    func testAFallbackNeededInTheBackgroundWaitsForTheAppAndSaysSo() async throws {
        let uptime = Uptime()
        let clock = ForegroundClock(active: false, uptime: { uptime.now })
        let studio = ScriptedStudio(upload: .failure(.unavailable("the Studio can’t be reached")))
        let device = DeviceEngine()
        let waiting = Said()
        let t = StudioFirstTranscriber(studio: studio, onDevice: device, foreground: clock)
        let hooks = StudioRunHooks(onWaitingForApp: { why in waiting.add(why) })
        let url = self.url
        let run = Task {
            try await t.transcribe(fileAt: url, locale: Locale(identifier: "it-IT"), options: .plain,
                                   hooks: hooks) { _ in }
        }
        for _ in 0..<200 where waiting.all.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertEqual(waiting.all, ["the Studio can’t be reached"])
        XCTAssertEqual(device.called, 0, "this device's engine does not run in the background")
        clock.setActive(true)
        let result = try await run.value
        XCTAssertEqual(device.called, 1)
        XCTAssertEqual(result.text, "Read on the device.")
    }

    func testARunTheStudioNoLongerHasIsReadHere() async throws {
        let studio = ScriptedStudio(polls: [.failure(.gone("the result expired"))])
        let device = DeviceEngine()
        let t = StudioFirstTranscriber(studio: studio, onDevice: device, pollInterval: .milliseconds(1))
        let result = try await t.resume(runID: "tr-1", fileAt: url, locale: Locale(identifier: "it-IT"),
                                        hooks: .none) { _ in }
        XCTAssertEqual(device.called, 1)
        XCTAssertTrue(result.notice?.contains("the result expired") ?? false)
    }

    func testTheClockStandsStillInTheBackground() {
        let uptime = Uptime()
        let clock = ForegroundClock(active: true, uptime: { uptime.now })
        uptime.advance(10)
        clock.setActive(false)
        uptime.advance(1_000)
        XCTAssertEqual(clock.now(), 10)
        clock.setActive(true)
        uptime.advance(5)
        XCTAssertEqual(clock.now(), 15)
    }
}

// MARK: - The upload, on the background session

private final class RecordingUploader: StudioAudioUploading, Sendable {
    private let sent = Mutex<[(URLRequest, String?)]>([])
    var requests: [(URLRequest, String?)] { sent.withLock { $0 } }

    func upload(_ request: URLRequest, fromFile url: URL, tag: String?,
                onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse) {
        sent.withLock { $0.append((request, tag)) }
        let body = #"{"id":"tr-2","state":"running","phase":"queued","notify":true}"#
        let response = HTTPURLResponse(url: request.url!, statusCode: 202, httpVersion: nil, headerFields: nil)!
        return (Data(body.utf8), response)
    }

    func reattach(tag: String,
                  onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse)? {
        nil
    }
}

final class BackgroundUploadTests: XCTestCase {

    /// The pin on the one request that carries audio, extended to the background session:
    /// the same paired bridge, the same route, and only the push's two fields added.
    func testTheBackgroundUploadGoesToThePairedBridgeAndNowhereElse() async throws {
        let endpoint = try XCTUnwrap(StudioEndpoint(baseURL: URL(string: "http://studio.example.ts.net:8765/"),
                                                    token: "tok"))
        let uploader = RecordingUploader()
        let transport = URLSessionStudioTransport(endpoint: { endpoint }, uploader: uploader)
        let options = StudioUploadOptions(tag: "run-1", conversationID: "C0FFEE", notify: true)
        let accepted = try await transport.upload(fileAt: URL(fileURLWithPath: "/tmp/memo.m4a"),
                                                  contentType: "audio/mp4", language: "it",
                                                  options: options) { _ in }
        XCTAssertEqual(accepted.notify, true)
        let (request, tag) = try XCTUnwrap(uploader.requests.first)
        XCTAssertEqual(uploader.requests.count, 1)
        XCTAssertEqual(tag, "run-1")
        XCTAssertEqual(request.url?.absoluteString,
                       "http://studio.example.ts.net:8765/jesse/transcriptions?language=it&notify=1&conversation_id=C0FFEE")
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer tok")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "audio/mp4")
        XCTAssertNil(request.httpBody, "streamed from its file")
    }

    func testAPlainUploadIsTheRequestAnOlderAppSends() throws {
        let endpoint = try XCTUnwrap(StudioEndpoint(baseURL: URL(string: "http://studio:8765/"), token: "tok"))
        let plain = URLSessionStudioTransport.uploadRequest(endpoint: endpoint, language: "it",
                                                            contentType: "audio/mp4", options: .plain)
        XCTAssertEqual(plain.url?.absoluteString, "http://studio:8765/jesse/transcriptions?language=it")
    }

    func testTheSessionIsABackgroundOneThatWakesTheApp() {
        let c = BackgroundStudioUploader.configuration(identifier: BackgroundStudioUploader.sessionIdentifier)
        XCTAssertEqual(c.identifier, "com.tag1.Jesse.transcription-upload")
        XCTAssertFalse(c.isDiscretionary, "the owner is waiting; it must not be deferred")
        #if os(iOS)
        XCTAssertTrue(c.sessionSendsLaunchEvents)
        #endif
    }

    func testTheUploadAnswerSaysWhetherAPushIsComing() throws {
        let json = #"{"id":"tr-1","state":"running","phase":"queued","notify":false}"#
        XCTAssertEqual(try JSONDecoder().decode(StudioRunStatus.self, from: Data(json.utf8)).notify, false)
        let old = #"{"id":"tr-1","state":"running","phase":"queued"}"#
        XCTAssertNil(try JSONDecoder().decode(StudioRunStatus.self, from: Data(old.utf8)).notify,
                     "an older bridge says nothing, which means no push")
    }
}
