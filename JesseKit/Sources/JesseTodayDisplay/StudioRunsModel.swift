import Foundation
import JesseNetworking

// STUDIO RUNS, WITHOUT CHAT: what the Runs list, a run's detail, a strand's Drafts lines and
// the board's badge decide, kept out of the views so a test can ask every question a button
// asks.
//
// Every enablement rule here mirrors the bridge's, never widens it: a reply the bridge would
// refuse is not offered, and where the bridge would accept something this screen chooses not
// to offer (a reply to a failed run), the line under the composer says why. When the bridge
// refuses anyway (a run moved on between two polls), its sentence is shown verbatim.

// MARK: - The look of a status

/// The colour family a status is drawn in. An enum rather than a `Color` so a test can pin it.
public enum StudioRunTint: String, Equatable, Sendable {
    case neutral, blue, orange, green, red, yellow, purple, secondary
}

public enum StudioRunSemantics {
    /// The words a person reads.
    public static func label(_ status: StudioRunStatus) -> String {
        switch status {
        case .queued: return "Queued"
        case .running: return "Running"
        case .needsYou: return "Needs you"
        case .reported: return "Reported"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .interrupted: return "Interrupted"
        case .paused: return "Paused"
        case .unknown(let word): return word.isEmpty ? "Unknown" : word
        }
    }

    public static func symbol(_ status: StudioRunStatus) -> String {
        switch status {
        case .queued: return "clock"
        case .running: return "play.circle.fill"
        case .needsYou: return "person.crop.circle.badge.exclamationmark"
        case .reported: return "checkmark.circle.fill"
        case .failed: return "xmark.octagon.fill"
        case .cancelled: return "stop.circle"
        case .interrupted: return "exclamationmark.triangle.fill"
        case .paused: return "pause.circle.fill"
        case .unknown: return "questionmark.circle"
        }
    }

    public static func tint(_ status: StudioRunStatus) -> StudioRunTint {
        switch status {
        case .queued: return .neutral
        case .running: return .blue
        case .needsYou: return .orange
        case .reported: return .green
        case .failed: return .red
        case .cancelled: return .secondary
        case .interrupted: return .yellow
        case .paused: return .purple
        case .unknown: return .neutral
        }
    }

    // MARK: Order

    /// Live runs first (queued, running, needs you, paused), then the rest; newest first
    /// within each, by creation and then by id so two runs created in one millisecond keep a
    /// stable order.
    public static func sorted(_ runs: [StudioRun]) -> [StudioRun] {
        runs.sorted { a, b in
            if a.status.isLive != b.status.isLive { return a.status.isLive }
            if a.createdAt != b.createdAt { return a.createdAt > b.createdAt }
            return a.runID > b.runID
        }
    }

    /// The live run of one strand line, if it has one: what replaces `Run on Studio`.
    public static func liveRun(strand: String, itemID: String,
                               in runs: [StudioRun]) -> StudioRun? {
        sorted(runs).first { $0.strand == strand && $0.itemID == itemID && $0.status.isLive }
    }

    // MARK: Time

    /// An RFC 3339 stamp as the bridge writes it (`2026-11-09T22:56:45.707Z`), with or without
    /// the milliseconds.
    public static func date(_ stamp: String?) -> Date? {
        guard let stamp, !stamp.isEmpty else { return nil }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = fractional.date(from: stamp) { return d }
        return ISO8601DateFormatter().date(from: stamp)
    }

    /// `just now`, `12m`, `3h`, `2d`: how long ago `stamp` was.
    public static func age(_ stamp: String?, now: Date) -> String? {
        guard let at = date(stamp) else { return nil }
        let seconds = max(0, now.timeIntervalSince(at))
        if seconds < 60 { return "just now" }
        if seconds < 3600 { return "\(Int(seconds / 60))m" }
        if seconds < 86_400 { return "\(Int(seconds / 3600))h" }
        return "\(Int(seconds / 86_400))d"
    }

    /// A stamp as a clock time: `18:00` today, `Nov 10 18:00` otherwise, in this device's zone.
    public static func clock(_ stamp: String?, now: Date,
                             calendar: Calendar = .current) -> String? {
        guard let at = date(stamp) else { return nil }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = calendar.timeZone
        f.dateFormat = calendar.isDate(at, inSameDayAs: now) ? "HH:mm" : "MMM d HH:mm"
        return f.string(from: at)
    }

    /// `Resumes 18:00` for a paused run that says when; nil otherwise.
    public static func resumeLine(_ run: StudioRun, now: Date) -> String? {
        guard run.status == .paused, let at = clock(run.resumeAt, now: now) else { return nil }
        return "Resumes \(at)"
    }

    // MARK: What a run offers

    /// The text a `Resume` sends: the bridge's own continue round, word for word.
    public static let resumeText = "Continue where you left off."

    /// Whether the composer may send: a run waiting on its owner, cut off, or done and
    /// open to a follow up.
    public static func canReply(_ status: StudioRunStatus) -> Bool {
        switch status {
        case .needsYou, .interrupted, .reported: return true
        default: return false
        }
    }

    /// Whether `Resume` is offered: an interrupted or needs-you run, with nothing typed.
    public static func offersResume(_ status: StudioRunStatus, composer: String) -> Bool {
        switch status {
        case .needsYou, .interrupted:
            return composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        default:
            return false
        }
    }

    /// Whether Cancel is offered: the bridge cancels only a queued, running or paused run.
    public static func canCancel(_ status: StudioRunStatus) -> Bool {
        switch status {
        case .queued, .running, .paused: return true
        default: return false
        }
    }

    /// The one line under a disabled composer, or nil where it is enabled.
    public static func replyDisabledReason(_ run: StudioRun, now: Date) -> String? {
        switch run.status {
        case .needsYou, .interrupted, .reported:
            return nil
        case .queued:
            return "Queued. A reply can be sent once the round has run."
        case .running:
            return "Running. A reply can be sent when this round finishes."
        case .paused:
            if let at = clock(run.resumeAt, now: now) {
                return "Paused for the usage limit. It continues by itself at \(at)."
            }
            return "Paused for the usage limit. It continues by itself."
        case .failed:
            return "Failed. Start it again from the strand's Drafts."
        case .cancelled:
            return "Cancelled. There is nothing to reply to."
        case .unknown(let word):
            return "This bridge says \(word.isEmpty ? "something" : "\u{201C}\(word)\u{201D}") and this app doesn't know what a reply would do."
        }
    }

    /// Whether a detail screen keeps polling: until the run is reported, failed, cancelled or
    /// in a state this build cannot read.
    public static func keepsPolling(_ status: StudioRunStatus) -> Bool {
        switch status {
        case .reported, .failed, .cancelled, .unknown: return false
        default: return true
        }
    }

    // MARK: The board's badge

    /// The badge a strand row shows, or nil with no active run.
    public static func badge(active: Int?, needsYou: Int?) -> StudioRunBadge? {
        guard let active, active > 0 else { return nil }
        let waiting = max(0, min(needsYou ?? 0, active))
        let runs = active == 1 ? "1 Studio run" : "\(active) Studio runs"
        let spoken = waiting > 0 ? "\(runs), \(waiting) needs you" : runs
        return StudioRunBadge(text: "\(active)", needsYou: waiting > 0, accessibilityLabel: spoken)
    }

    public static func badge(for strand: Strand) -> StudioRunBadge? {
        badge(active: strand.runsActive, needsYou: strand.runsNeedsYou)
    }
}

/// A strand row's run badge.
public struct StudioRunBadge: Equatable, Sendable {
    /// The count drawn in the capsule.
    public let text: String
    /// Tinted the needs-you colour: one of the strand's runs is waiting on its owner.
    public let needsYou: Bool
    /// What VoiceOver reads: `2 Studio runs, 1 needs you`.
    public let accessibilityLabel: String
}

// MARK: - The list

/// The runs the bridge knows, and the one start.
@MainActor
@Observable
public final class StudioRunsModel {
    nonisolated deinit {}

    /// Every run, live first then newest first.
    public private(set) var runs: [StudioRun] = []
    public private(set) var hasLoaded = false
    public private(set) var isLoading = false
    /// Why the last load failed, shown above the list. Cleared by the next good load.
    public private(set) var loadError: String?
    /// The bridge's refusal of an action, verbatim, for an alert.
    public var alert: String?
    /// The id of the run a start is posting for, so its line can say so.
    public private(set) var starting: String?
    /// Whether the Today tab's Runs sheet is up. Here so the shell can close it.
    public var presentsList = false

    private let makeClient: @MainActor () -> any StudioRunsProviding
    private let now: @Sendable () -> Date
    @ObservationIgnored private var details: [String: StudioRunDetailModel] = [:]

    public init(makeClient: @escaping @MainActor () -> any StudioRunsProviding,
                now: @escaping @Sendable () -> Date = { Date() }) {
        self.makeClient = makeClient
        self.now = now
    }

    public var currentDate: Date { now() }

    public func load() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let list = try await makeClient().listRuns()
            runs = StudioRunSemantics.sorted(list.runs)
            hasLoaded = true
            loadError = nil
        } catch {
            loadError = Self.message(error)
        }
    }

    /// The runs of one strand, or every run for nil.
    public func runs(strand: String?) -> [StudioRun] {
        guard let strand else { return runs }
        return runs.filter { $0.strand == strand }
    }

    public func liveRun(strand: String, itemID: String) -> StudioRun? {
        StudioRunSemantics.liveRun(strand: strand, itemID: itemID, in: runs)
    }

    /// `Run on Studio`. The started run on success; nil, with the bridge's words in `alert`,
    /// on a refusal.
    public func start(strand: String, itemID: String) async -> StudioRunStarted? {
        guard starting == nil else { return nil }
        starting = itemID
        defer { starting = nil }
        do {
            let started = try await makeClient().startRun(strand: strand, itemID: itemID)
            await load()
            return started
        } catch {
            alert = Self.message(error)
            return nil
        }
    }

    /// One detail model per run for the life of this model, so a screen rebuilt by its
    /// parent keeps its composer and its log. Seeded with the list's copy so it draws at once.
    public func detailModel(for runID: String) -> StudioRunDetailModel {
        if let model = details[runID] { return model }
        let model = StudioRunDetailModel(runID: runID, run: runs.first { $0.runID == runID },
                                         makeClient: makeClient, now: now)
        details[runID] = model
        return model
    }

    static func message(_ error: any Error) -> String {
        (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
    }
}

// MARK: - One run

/// One run's detail: the record, its log tail, the composer and Cancel.
@MainActor
@Observable
public final class StudioRunDetailModel {
    nonisolated deinit {}

    public let runID: String
    public private(set) var run: StudioRun?
    public private(set) var lines: [String] = []
    public private(set) var loadError: String?
    public var composer = ""
    public private(set) var isSending = false
    /// The bridge's refusal of a reply or a cancel, verbatim.
    public var alert: String?

    /// How many log lines the detail asks for.
    public static let tail = 200

    private let makeClient: @MainActor () -> any StudioRunsProviding
    private let now: @Sendable () -> Date

    public init(runID: String, run: StudioRun? = nil,
                makeClient: @escaping @MainActor () -> any StudioRunsProviding,
                now: @escaping @Sendable () -> Date = { Date() }) {
        self.runID = runID
        self.run = run
        self.makeClient = makeClient
        self.now = now
    }

    public var currentDate: Date { now() }

    public var status: StudioRunStatus? { run?.status }

    public var canSend: Bool {
        guard let status, !isSending else { return false }
        return StudioRunSemantics.canReply(status)
            && !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    public var isComposerEnabled: Bool {
        guard let status else { return false }
        return StudioRunSemantics.canReply(status) && !isSending
    }

    public var showsResume: Bool {
        guard let status else { return false }
        return StudioRunSemantics.offersResume(status, composer: composer)
    }

    public var canCancel: Bool {
        guard let status, !isSending else { return false }
        return StudioRunSemantics.canCancel(status)
    }

    public var replyDisabledReason: String? {
        guard let run else { return nil }
        return StudioRunSemantics.replyDisabledReason(run, now: now())
    }

    public var keepsPolling: Bool {
        guard let status else { return true }
        return StudioRunSemantics.keepsPolling(status)
    }

    public func refresh() async {
        let client = makeClient()
        do {
            run = try await client.getRun(id: runID)
            lines = try await client.runLog(id: runID, tail: Self.tail).lines
            loadError = nil
        } catch {
            loadError = StudioRunsModel.message(error)
        }
    }

    public func send() async {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if await post({ try await $0.replyToRun(id: self.runID, text: text) }) { composer = "" }
    }

    public func resume() async {
        _ = await post { try await $0.replyToRun(id: self.runID, text: StudioRunSemantics.resumeText) }
    }

    public func cancel() async {
        _ = await post { try await $0.cancelRun(id: self.runID) }
    }

    private func post(_ call: @MainActor (any StudioRunsProviding) async throws -> StudioRun) async -> Bool {
        guard !isSending else { return false }
        isSending = true
        defer { isSending = false }
        do {
            run = try await call(makeClient())
            if let log = try? await makeClient().runLog(id: runID, tail: Self.tail) {
                lines = log.lines
            }
            return true
        } catch {
            alert = StudioRunsModel.message(error)
            return false
        }
    }
}
