import Foundation

// The runs that are still going, written down, so that a relaunch finishes them.
//
// A recording confirmed for transcription used to live only in the composer's memory: iOS
// terminating the suspended app (which it does freely) lost the run, and the launch purge
// deleted its audio. Now the run is a RECORD on disk from the moment the owner confirms the
// language until it ends, next to the app's own copy of the audio, and the next launch
// picks it up: it follows the Studio run by its id, or reattaches to the upload that was
// still going, or sends the recording again.
//
// THE AUDIO RULE IS UNCHANGED, only its lifetime is: Jesse never keeps a recording. The
// copy here exists exactly as long as its run, and is deleted the moment the run ends, on
// success and cancel alike. A FAILED run is kept, marked failed, so the owner can try it
// again with another engine; it is deleted on the retry's success, on Discard, or by the
// launch sweep a day after it failed. It lives under Application Support rather than Caches
// because the system may empty Caches while the app is suspended, which is precisely when
// this copy has to still be there; and it is excluded from backups.

/// One run in flight.
public struct TranscriptionRunRecord: Codable, Sendable, Equatable, Identifiable {
    /// This device's id for the run. Also the upload's tag.
    public let id: UUID
    public let conversationID: UUID
    /// The name the owner knows the recording by.
    public let sourceName: String
    public let durationSeconds: Double
    /// The language's display name, as the transcript header shows it.
    public let language: String
    public let localeIdentifier: String
    /// The audio's file name inside the store's audio directory.
    public let audioFileName: String
    /// The Studio's id for the run, once the upload has been answered.
    public var studioRunID: String?
    /// Whether the bridge said it will push this run's ending. When it will, the app does
    /// not post its own notification, so the owner is told once.
    public var bridgeWillPush: Bool
    public let startedAt: Date
    /// The engine the owner chose for this run (`local`, `hosted:<id>`), or nil for the
    /// Studio's default. Written down so a relaunch sends it again with the same choice.
    public var engine: String?
    /// Set when the run FAILED and its recording is kept for "Try again with": the sentence
    /// the owner was shown. A kept run is never resumed on its own; it waits for a retry or
    /// a discard, and the launch sweep deletes it after `RecordingHandoffStore.maxAge`.
    public var failureMessage: String?
    public var failedAt: Date?

    public var isKeptAfterFailure: Bool { failureMessage != nil }

    public init(id: UUID = UUID(), conversationID: UUID, sourceName: String, durationSeconds: Double,
                language: String, localeIdentifier: String, audioFileName: String,
                studioRunID: String? = nil, bridgeWillPush: Bool = false, startedAt: Date = Date(),
                engine: String? = nil, failureMessage: String? = nil, failedAt: Date? = nil) {
        self.id = id
        self.conversationID = conversationID
        self.sourceName = sourceName
        self.durationSeconds = durationSeconds
        self.language = language
        self.localeIdentifier = localeIdentifier
        self.audioFileName = audioFileName
        self.studioRunID = studioRunID
        self.bridgeWillPush = bridgeWillPush
        self.startedAt = startedAt
        self.engine = engine
        self.failureMessage = failureMessage
        self.failedAt = failedAt
    }

    /// The same recording, started again as a NEW run: a new id (so the upload's tag cannot
    /// reattach to the failed upload's answer), no Studio id, and the engine asked for now.
    public func retried(engine: String?, at now: Date = Date()) -> TranscriptionRunRecord {
        TranscriptionRunRecord(conversationID: conversationID, sourceName: sourceName,
                               durationSeconds: durationSeconds, language: language,
                               localeIdentifier: localeIdentifier, audioFileName: audioFileName,
                               startedAt: now, engine: engine)
    }
}

/// The records and their audio, in one directory.
///
/// Small and synchronous on purpose: a handful of records, written at the few moments a run
/// changes (start, the Studio's id, the end), and read once at launch.
public struct TranscriptionRunStore: Sendable {
    public let directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    /// The app's store, under Application Support.
    public static func standard() -> TranscriptionRunStore {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return TranscriptionRunStore(directory: base.appendingPathComponent("RecordingRuns", isDirectory: true))
    }

    private struct Contents: Codable {
        var runs: [TranscriptionRunRecord] = []
        /// The last failure per conversation that nobody has seen yet, keyed by conversation
        /// id, so a run that failed while the app was away still says why when it is opened.
        var failures: [String: String] = [:]
    }

    private var file: URL { directory.appendingPathComponent("runs.json") }
    private var audioDirectory: URL { directory.appendingPathComponent("audio", isDirectory: true) }

    public func audioURL(for record: TranscriptionRunRecord) -> URL {
        audioDirectory.appendingPathComponent(record.audioFileName)
    }

    /// Take custody of a recording by MOVING the working copy in, so there is never a second
    /// copy of it. Returns the file name to record.
    public func adopt(moving source: URL, id: UUID) throws -> String {
        let manager = FileManager.default
        try manager.createDirectory(at: audioDirectory, withIntermediateDirectories: true)
        var excluded = directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? excluded.setResourceValues(values)
        let ext = source.pathExtension
        let name = ext.isEmpty ? id.uuidString : "\(id.uuidString).\(ext)"
        let destination = audioDirectory.appendingPathComponent(name)
        try? manager.removeItem(at: destination)
        do {
            try manager.moveItem(at: source, to: destination)
        } catch {
            try manager.copyItem(at: source, to: destination)
            try? manager.removeItem(at: source)
        }
        return name
    }

    // MARK: - Records

    public func runs() -> [TranscriptionRunRecord] {
        load().runs.sorted { $0.startedAt < $1.startedAt }
    }

    public func save(_ record: TranscriptionRunRecord) {
        var contents = load()
        contents.runs.removeAll { $0.id == record.id }
        contents.runs.append(record)
        contents.failures[record.conversationID.uuidString] = nil
        write(contents)
    }

    /// Swap one record for another that keeps the same audio: a failed run started again.
    public func replace(_ old: TranscriptionRunRecord, with new: TranscriptionRunRecord) {
        var contents = load()
        contents.runs.removeAll { $0.id == old.id || $0.id == new.id }
        contents.runs.append(new)
        contents.failures[new.conversationID.uuidString] = nil
        write(contents)
    }

    /// Delete every run kept after a failure for longer than `maxAge`, with its audio: the
    /// owner did not come back to try again, and Jesse does not keep recordings. Returns
    /// what it deleted. Run at launch, before anything is restored.
    @discardableResult
    public func sweepExpiredFailures(now: Date = Date(),
                                     maxAge: TimeInterval = RecordingHandoffStore.maxAge) -> [TranscriptionRunRecord] {
        let expired = load().runs.filter { record in
            guard record.isKeptAfterFailure else { return false }
            return now.timeIntervalSince(record.failedAt ?? record.startedAt) > maxAge
        }
        for record in expired { remove(record) }
        return expired
    }

    /// The run is over: forget it and delete its audio.
    public func remove(_ record: TranscriptionRunRecord) {
        try? FileManager.default.removeItem(at: audioURL(for: record))
        var contents = load()
        contents.runs.removeAll { $0.id == record.id }
        write(contents)
    }

    public func recordFailure(_ message: String, conversationID: UUID) {
        var contents = load()
        contents.failures[conversationID.uuidString] = message
        write(contents)
    }

    public func failures() -> [UUID: String] {
        var out: [UUID: String] = [:]
        for (key, value) in load().failures {
            if let id = UUID(uuidString: key) { out[id] = value }
        }
        return out
    }

    public func clearFailure(conversationID: UUID) {
        var contents = load()
        guard contents.failures.removeValue(forKey: conversationID.uuidString) != nil else { return }
        write(contents)
    }

    /// Delete any audio no record claims: a run whose record was removed but whose file
    /// survived a kill in between. Run at launch, before anything resumes.
    public func sweepUnclaimedAudio() {
        let manager = FileManager.default
        let claimed = Set(load().runs.map(\.audioFileName))
        let names = (try? manager.contentsOfDirectory(atPath: audioDirectory.path)) ?? []
        for name in names where !claimed.contains(name) {
            try? manager.removeItem(at: audioDirectory.appendingPathComponent(name))
        }
    }

    private func load() -> Contents {
        guard let data = try? Data(contentsOf: file) else { return Contents() }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return (try? decoder.decode(Contents.self, from: data)) ?? Contents()
    }

    private func write(_ contents: Contents) {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try encoder.encode(contents).write(to: file, options: .atomic)
        } catch {
            // Nothing to show: the run goes on in memory, and only its survival of a
            // termination is at stake.
        }
    }
}
