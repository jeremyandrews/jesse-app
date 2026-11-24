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
    /// A cloud run's row accent and status badge, whatever its status: teal, apart from every
    /// status colour.
    case cloud
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
        case .closed: return "Closed"
        case .reviewing: return "Reviewing"
        case .awaitingMerge: return "Awaiting your merge"
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
        case .closed: return "checkmark.circle"
        case .reviewing: return "magnifyingglass.circle.fill"
        case .awaitingMerge: return "arrow.triangle.merge"
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
        case .closed: return .secondary
        // A second model is working on it: drawn as running is.
        case .reviewing: return .blue
        // Waits on its owner, as a question does: drawn in the needs-you colour.
        case .awaitingMerge: return .orange
        case .unknown: return .neutral
        }
    }

    // MARK: Cloud runs

    /// The word a cloud run's row and detail carry.
    public static let cloudLabel = "Cloud"
    /// Its SF Symbol.
    public static let cloudSymbol = "cloud"

    /// The tint of a run's status badge: a cloud run's is the cloud colour, so it reads as the
    /// cloud's at a glance; a Studio run's is its status colour, as before.
    public static func statusTint(_ run: StudioRun) -> StudioRunTint {
        run.isCloud ? .cloud : tint(run.status)
    }

    /// The tint of a row's leading accent: the cloud colour on a cloud run, none on a Studio run.
    public static func rowTint(_ run: StudioRun) -> StudioRunTint? {
        run.isCloud ? .cloud : nil
    }

    /// The Details row that says when a run began: `Started` and its start. A cloud run that
    /// has not started (the bridge serves no `started_at`) shows `Queued` and when it was
    /// queued instead, never its queue time as a start. A Studio run keeps its own rule: its
    /// creation stands in for a start it does not have yet.
    public static func beganField(_ run: StudioRun) -> StudioRunTimeField? {
        if let started = run.startedAt, !started.isEmpty {
            return StudioRunTimeField(label: "Started", stamp: started)
        }
        let created = run.isCloud ? (run.queuedAt ?? run.createdAt) : run.createdAt
        guard !created.isEmpty else { return nil }
        return StudioRunTimeField(label: run.isCloud ? "Queued" : "Started", stamp: created)
    }

    /// The one quiet line the list shows when the bridge says the cloud's runs are not listed.
    public static let cloudUnavailableLine = "Cloud runs are unavailable right now."

    // MARK: Order

    /// Live runs first (queued, running, needs you, awaiting your merge, paused, reviewing),
    /// then the rest; newest first
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
        case .needsYou, .awaitingMerge, .interrupted, .reported, .closed: return true
        case .queued, .running, .failed, .cancelled, .paused, .reviewing, .unknown: return false
        }
    }

    /// Whether the run's detail draws a composer at all. Not while it is being reviewed: the
    /// bridge hands the findings to the run's own session, so there is nothing to reply to and
    /// the one line (`replyDisabledReason`) stands where the composer would be.
    public static func showsComposer(_ status: StudioRunStatus) -> Bool {
        status != .reviewing
    }

    /// Whether `Resume` is offered: an interrupted run, with nothing typed. Never a
    /// `needs_you` run: it asked a question, and an empty continue would answer it blind (the
    /// bridge refuses that one with a 409).
    public static func offersResume(_ status: StudioRunStatus, composer: String) -> Bool {
        switch status {
        case .interrupted:
            return composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .queued, .running, .needsYou, .awaitingMerge, .reported, .failed, .cancelled,
             .paused, .closed, .reviewing, .unknown:
            return false
        }
    }

    /// Whether Cancel is offered: what the bridge's cancel route accepts (a queued, running,
    /// paused or reviewing run).
    public static func canCancel(_ status: StudioRunStatus) -> Bool {
        status.isCancellable
    }

    /// What the Cancel confirmation says happens.
    public static func cancelMessage(_ status: StudioRunStatus) -> String {
        status == .reviewing
            ? "The review is stopped and the run is cancelled. Its findings never reach the run."
            : "The agent is stopped and the round is reported as cancelled."
    }

    /// The line under a run's title on a row and atop its detail while it moves: who is
    /// reviewing it (`Reviewing with GLM 5.3`) while it is being reviewed, else the last line
    /// of its log. Nil for a run that is not moving or has nothing to say.
    public static func movingLine(_ run: StudioRun) -> String? {
        guard run.status.isMoving else { return nil }
        if run.status == .reviewing {
            guard let name = run.reviewerName, !name.isEmpty else {
                return "Reviewing with a second model"
            }
            return "Reviewing with \(name)"
        }
        guard let line = run.lastLine, !line.isEmpty else { return nil }
        return line
    }

    // MARK: Awaiting a merge

    /// The pull request link a run shows: on an `awaiting_merge` run (and on any run that still
    /// carries a `merge_wait`, whose wait has ended), when its URL is a web one. Nil otherwise.
    public static func mergeLink(_ run: StudioRun) -> StudioRunMergeLink? {
        guard let wait = run.mergeWait, let url = wait.webURL else { return nil }
        let reference = wait.reference
        let trimmed = wait.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let title: String? = trimmed.isEmpty ? nil : trimmed
        let text = reference ?? title ?? url.absoluteString
        let shownTitle = reference != nil ? title : nil
        let spoken = ["Pull request", reference, shownTitle].compactMap { $0 }
            .joined(separator: ", ")
        return StudioRunMergeLink(url: url, text: text, title: shownTitle,
                                  accessibilityLabel: spoken)
    }

    /// Whether a row shows the pull request link: only while the run waits on the merge.
    public static func rowMergeLink(_ run: StudioRun) -> StudioRunMergeLink? {
        guard run.status == .awaitingMerge, run.mergeWait?.hasEnded != true else { return nil }
        return mergeLink(run)
    }

    /// The line beside the link: what happens while it waits, or how the wait ended.
    public static func mergeLine(_ run: StudioRun, now: Date) -> String? {
        guard let wait = run.mergeWait else { return nil }
        if let ended = wait.ended?.trimmingCharacters(in: .whitespacesAndNewlines),
           !ended.isEmpty {
            return "Wait ended: \(ended)"
        }
        guard run.status == .awaitingMerge else { return nil }
        var line = "Merge it and the run continues by itself."
        if let at = clock(wait.lastCheckAt, now: now) { line += " Last checked \(at)." }
        return line
    }

    // MARK: Reviews

    /// A review's status in words.
    public static func reviewLabel(_ status: StudioRunReviewStatus) -> String {
        switch status {
        case .running: return "Reviewing"
        case .ready: return "Note written"
        case .handedOff: return "Handed to the run"
        case .answered: return "Answered"
        case .failed: return "Failed"
        case .dropped: return "Dropped"
        case .unknown(let word): return word.isEmpty ? "Unknown" : word
        }
    }

    /// One line of the Review section: `Codex · Answered · 18:02`, the time only once it
    /// finished.
    public static func reviewLine(_ review: StudioRunReview, now: Date,
                                  calendar: Calendar = .current) -> String {
        var parts = [review.reviewerName.isEmpty ? "Reviewer" : review.reviewerName,
                     reviewLabel(review.status)]
        if let at = clock(review.finishedAt, now: now, calendar: calendar) { parts.append(at) }
        return parts.joined(separator: " \u{00B7} ")
    }

    /// The Review section's line for a reviewed run whose review has not begun.
    public static func pendingReviewLine(_ run: StudioRun) -> String? {
        guard run.reviews.isEmpty, let name = run.reviewerName, !name.isEmpty else { return nil }
        return "\(name) reviews the work when this round reports."
    }

    /// The one line under a disabled composer, or nil where it is enabled.
    public static func replyDisabledReason(_ run: StudioRun, now: Date) -> String? {
        if run.isCloud {
            return "A cloud run on kc1. Cloud runs are managed through Jesse."
        }
        switch run.status {
        case .needsYou, .awaitingMerge, .interrupted, .reported, .closed:
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
        case .reviewing:
            return "Being reviewed. The findings go to the run itself; nothing to reply to yet."
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
        case .reported, .failed, .cancelled, .closed, .unknown: return false
        // Awaiting a merge: the bridge resumes the run by itself when the pull request merges.
        case .queued, .running, .needsYou, .awaitingMerge, .interrupted, .paused, .reviewing:
            return true
        }
    }

    // MARK: Archiving

    /// Whether Archive is offered: on a run that is not moving and not archived. The bridge
    /// refuses a queued, running, paused or reviewing run; a `needs_you`, `awaiting_merge` or
    /// `interrupted` run it closes first, so those are offered too.
    public static func canArchive(_ run: StudioRun) -> Bool {
        !run.isCloud && !run.status.isMoving && !run.isArchived
    }

    /// Whether Unarchive is offered: on an archived run. Never on a cloud run.
    public static func canUnarchive(_ run: StudioRun) -> Bool {
        !run.isCloud && run.isArchived
    }

    // MARK: The board's badge

    /// The badge a strand row shows, or nil with no active run.
    public static func badge(active: Int?, needsYou: Int?) -> StudioRunBadge? {
        guard let active, active > 0 else { return nil }
        let waiting = max(0, min(needsYou ?? 0, active))
        let runs = active == 1 ? "1 Studio run" : "\(active) Studio runs"
        let spoken: String
        switch waiting {
        case 0: spoken = runs
        case 1: spoken = "\(runs), 1 needs you"
        default: spoken = "\(runs), \(waiting) need you"
        }
        return StudioRunBadge(text: "\(active)", needsYou: waiting > 0, accessibilityLabel: spoken)
    }

    public static func badge(for strand: Strand) -> StudioRunBadge? {
        badge(active: strand.runsActive, needsYou: strand.runsNeedsYou)
    }
}

/// The pull request a run waits on, as a link: `owner/repo#number`, its title beside it.
public struct StudioRunMergeLink: Equatable, Sendable {
    public let url: URL
    /// The link's words: `owner/repo#number`, else the title, else the URL.
    public let text: String
    /// The pull request's title, shown beside a reference; nil when `text` is the title.
    public let title: String?
    public let accessibilityLabel: String
}

/// One timed row of a run's Details: what the time is, and the RFC 3339 stamp.
public struct StudioRunTimeField: Equatable, Sendable {
    public let label: String
    public let stamp: String
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

// MARK: - The filters

/// The runs list's filter bar: four segments and the Archived entry.
public enum StudioRunFilter: String, CaseIterable, Equatable, Sendable {
    case needsMe, moving, finished, all, archived

    /// Where the last choice is remembered across launches (`@AppStorage`).
    public static let storageKey = "studioRunsFilter"
    /// What a first launch shows.
    public static let initial: StudioRunFilter = .all
    /// The segmented control's segments; `archived` is a toolbar item of its own, since five
    /// segments with their counts truncate on an iPhone.
    public static let segments: [StudioRunFilter] = [.needsMe, .moving, .finished, .all]
    /// What the bar draws on this platform: the four segments on an iPhone, and Archived as a
    /// fifth on the Mac, where the width is there.
    public static var barSegments: [StudioRunFilter] {
        #if os(macOS)
        return segments + [.archived]
        #else
        return segments
        #endif
    }

    /// A remembered raw value, or the default for none or a word this build does not know.
    public static func restored(_ raw: String?) -> StudioRunFilter {
        raw.flatMap(StudioRunFilter.init(rawValue:)) ?? initial
    }

    public var title: String {
        switch self {
        case .needsMe: return "Needs me"
        case .moving: return "Moving"
        case .finished: return "Finished"
        case .all: return "All"
        case .archived: return "Archived"
        }
    }

    /// The one plain line an empty filter shows.
    public var emptyLine: String {
        switch self {
        case .needsMe: return "Nothing needs you."
        case .moving: return "Nothing is moving."
        case .finished: return "Nothing has finished."
        case .all: return "No runs."
        case .archived: return "Nothing is archived."
        }
    }

    /// Whether `run` is under this filter. `all` is every run not archived.
    public func includes(_ run: StudioRun) -> Bool {
        if self == .archived { return run.isArchived }
        if run.isArchived { return false }
        switch self {
        case .needsMe: return run.status.needsMe
        case .moving: return run.status.isMoving
        case .finished: return run.status.isFinished
        case .all, .archived: return true
        }
    }

    /// The bridge's tally for this filter, nil from a bridge that does not count it. The
    /// cloud's tallies (`counts.cloud`) are added to the Studio's: the list shows both.
    public func count(in counts: StudioRunCounts) -> Int? {
        let cloud = counts.cloud
        switch self {
        case .needsMe: return counts.needsMe.map { $0 + (cloud?.needsMe ?? 0) }
        case .moving: return counts.moving.map { $0 + (cloud?.moving ?? 0) }
        case .finished: return counts.finished.map { $0 + (cloud?.finished ?? 0) }
        case .archived: return counts.archived
        case .all:
            guard let a = StudioRunFilter.needsMe.count(in: counts),
                  let b = StudioRunFilter.moving.count(in: counts),
                  let c = StudioRunFilter.finished.count(in: counts) else {
                return nil
            }
            return a + b + c
        }
    }

    /// A segment's words: `Needs me 3`, or the title alone with no count.
    public func label(count: Int?) -> String {
        count.map { "\(title) \($0)" } ?? title
    }
}

/// The runs an archive just took out of the list, for the Undo banner.
public struct StudioRunUndo: Equatable, Identifiable, Sendable {
    public let id: UUID
    public let runIDs: [String]
    /// `Archived "Fix the build".` or `Archived 4 runs.`
    public let text: String

    public init(id: UUID = UUID(), runIDs: [String], text: String) {
        self.id = id
        self.runIDs = runIDs
        self.text = text
    }
}

// MARK: - The list

/// The runs the bridge knows, and the one start.
@MainActor
@Observable
public final class StudioRunsModel {
    nonisolated deinit {}

    /// Every Studio run not archived, live first then newest first. What the Work view, a
    /// strand's Drafts and the board's badge read: the cloud's runs are not here.
    public private(set) var runs: [StudioRun] = []
    /// The cloud's runs the bridge lists beside the Studio's (`host: "cloud"`, bridge 0.216.0),
    /// shown in the Runs list only, read only. Empty from an older bridge.
    public private(set) var cloudRuns: [StudioRun] = []
    /// Why the cloud's runs are not listed, when the bridge says they are not; nil when they are
    /// and from a bridge that predates cloud rows. One quiet line, never an error.
    public private(set) var cloudUnavailable: String?
    /// The archived runs, newest archived first, once the Archived filter has asked for them.
    public private(set) var archivedRuns: [StudioRun] = []
    public private(set) var hasLoadedArchived = false
    /// The bridge's tallies from the last list.
    public private(set) var counts = StudioRunCounts()
    /// The last archive, while its Undo banner shows.
    public private(set) var undo: StudioRunUndo?
    /// How long the Undo banner stays.
    public static let undoSeconds: Double = 5
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
    /// The shared last look this visit to Work compares against (RFC 3339), as it was when
    /// Work opened. Posting a new one does not move it, so the visit still shows what was new.
    public private(set) var lastLook: String?
    /// Whether the last look has been read from the bridge at all.
    public private(set) var lastLookLoaded = false
    @ObservationIgnored private var lastLookReadAt: Date?
    @ObservationIgnored private var lastLookPostedAt: Date?

    /// How long one visit to Work lasts for the last look: returning from a project inside
    /// this keeps the line; opening Work after it reads the new last look.
    public static let visitLength: TimeInterval = 5 * 60

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
            let list = try await makeClient().listRuns(archived: .hide)
            // An older bridge ignores the mode and serves every run: none is archived there.
            runs = StudioRunSemantics.sorted(list.runs.filter { !$0.isCloud && !$0.isArchived })
            cloudRuns = StudioRunSemantics.sorted(list.runs.filter(\.isCloud))
            cloudUnavailable = list.cloudAvailable == false
                ? (list.cloudUnavailable ?? "") : nil
            counts = list.counts
            hasLoaded = true
            loadError = nil
        } catch {
            loadError = Self.message(error)
        }
    }

    /// The archived runs, for the Archived filter.
    public func loadArchived() async {
        do {
            let list = try await makeClient().listRuns(archived: .only)
            archivedRuns = list.runs.filter { !$0.isCloud && $0.isArchived }
            counts = list.counts
            hasLoadedArchived = true
            loadError = nil
        } catch {
            loadError = Self.message(error)
        }
    }

    // MARK: Filters

    /// The rows a filter shows, every project's or one's: the Studio's runs and the cloud's
    /// together. Archived runs are newest archived first (none is a cloud run); the rest live
    /// first, then newest first, whichever host runs them.
    public func runs(filter: StudioRunFilter, strand: String?) -> [StudioRun] {
        let source = filter == .archived
            ? archivedRuns : StudioRunSemantics.sorted(runs + cloudRuns)
        return source.filter { (strand == nil || $0.strand == strand) && filter.includes($0) }
    }

    /// The count a filter shows: the bridge's `counts` for the whole list, what this model
    /// holds for one project's. Nil where it is not known (an older bridge's archived count, a
    /// project's archived count before the Archived filter has loaded).
    public func count(filter: StudioRunFilter, strand: String?) -> Int? {
        if strand == nil, let n = filter.count(in: counts) { return n }
        let loaded = filter == .archived ? hasLoadedArchived : hasLoaded
        return loaded ? runs(filter: filter, strand: strand).count : nil
    }

    // MARK: Archiving

    /// Archive one run: its row leaves the list at once and the Undo banner shows. On a refusal
    /// the bridge's words are in `alert` and the row comes back.
    public func archive(_ run: StudioRun) async {
        guard StudioRunSemantics.canArchive(run) else { return }
        await archive([run], text: "Archived \u{201C}\(WorkSemantics.title(run))\u{201D}.")
    }

    /// `Archive all finished`: the runs the Finished filter shows (for one project, or every
    /// one), and no other.
    public func archiveAllFinished(strand: String?) async {
        let shown = archivableFinished(strand: strand)
        guard !shown.isEmpty else { return }
        await archive(shown, text: shown.count == 1 ? "Archived 1 run." : "Archived \(shown.count) runs.")
    }

    /// What `Archive all finished` archives: the Finished filter's Studio runs. A cloud run is
    /// Jesse's, and stays.
    public func archivableFinished(strand: String?) -> [StudioRun] {
        runs(filter: .finished, strand: strand).filter { !$0.isCloud }
    }

    private func archive(_ targets: [StudioRun], text: String) async {
        let ids = Set(targets.map(\.runID))
        runs.removeAll { ids.contains($0.runID) }
        adjustCounts(archiving: targets)
        var done: [String] = []
        let client = makeClient()
        for run in targets {
            do {
                let archived = try await client.archive(id: run.runID)
                done.append(run.runID)
                details[run.runID]?.adopt(archived)
            } catch {
                alert = Self.message(error)
            }
        }
        if !done.isEmpty {
            undo = StudioRunUndo(runIDs: done, text: done.count == targets.count
                                 ? text : "Archived \(done.count) of \(targets.count) runs.")
        }
        await load()
        if hasLoadedArchived { await loadArchived() }
    }

    /// Put an archived run back in the list.
    public func unarchive(_ run: StudioRun) async {
        archivedRuns.removeAll { $0.runID == run.runID }
        do {
            let back = try await makeClient().unarchive(id: run.runID)
            details[run.runID]?.adopt(back)
        } catch {
            alert = Self.message(error)
        }
        await load()
        await loadArchived()
    }

    /// The Undo banner's button: unarchive what the last archive took.
    public func undoArchive() async {
        guard let undo else { return }
        self.undo = nil
        let client = makeClient()
        for id in undo.runIDs {
            do {
                let back = try await client.unarchive(id: id)
                details[id]?.adopt(back)
            } catch {
                alert = Self.message(error)
            }
        }
        await load()
        if hasLoadedArchived { await loadArchived() }
    }

    /// The banner's time is up (or it was swiped away). Only the banner `id` names goes.
    public func dismissUndo(_ id: UUID) {
        if undo?.id == id { undo = nil }
    }

    /// The tallies as they will be once `targets` are archived, until the next list says so.
    private func adjustCounts(archiving targets: [StudioRun]) {
        for run in targets {
            if run.status.needsMe, let n = counts.needsMe { counts.needsMe = max(0, n - 1) }
            if run.status.isFinished, let n = counts.finished { counts.finished = max(0, n - 1) }
            if let n = counts.archived { counts.archived = n + 1 }
        }
    }

    /// Read the shared last look, once per visit to Work. Quietly nothing from a bridge that
    /// predates it: the line simply never shows.
    public func loadLastLook() async {
        if let at = lastLookReadAt, now().timeIntervalSince(at) < Self.visitLength,
           lastLookLoaded { return }
        do {
            lastLook = try await makeClient().lastLook().lastLook
            lastLookLoaded = true
            lastLookReadAt = now()
        } catch {
            lastLookLoaded = false
        }
    }

    /// Post the last look (now), once per visit, after this visit's rows have rendered. The
    /// value this visit compares against is left alone.
    public func markLastLook() async {
        if let at = lastLookPostedAt, now().timeIntervalSince(at) < Self.visitLength { return }
        lastLookPostedAt = now()
        _ = try? await makeClient().markLastLook()
    }

    /// The runs that finished since the last look, every project's or one's.
    public func finishedSinceLastLook(strand: String? = nil) -> [StudioRun] {
        WorkSemantics.finishedSince(lastLook, in: runs, strand: strand)
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
        detailModel(for: StudioRunRoute(runID: runID))
    }

    /// The detail model of a run the list pushed: a Studio run's, or a cloud run's
    /// (`route.isCloud`), each its own even when the two share an id.
    public func detailModel(for route: StudioRunRoute) -> StudioRunDetailModel {
        let key = route.id
        if let model = details[key] { return model }
        let runID = route.runID
        let seed = route.isCloud
            ? cloudRuns.first { $0.runID == runID }
            : runs.first { $0.runID == runID } ?? archivedRuns.first { $0.runID == runID }
        let model = StudioRunDetailModel(
            runID: runID, isCloud: route.isCloud, run: seed, makeClient: makeClient, now: now)
        model.onArchiveChanged = { [weak self] in
            guard let self else { return }
            await self.load()
            if self.hasLoadedArchived { await self.loadArchived() }
        }
        details[key] = model
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
    /// A cloud run (`host: "cloud"`): read with `?host=cloud`, and nothing is offered on it.
    public let isCloud: Bool
    public private(set) var run: StudioRun?
    public private(set) var lines: [String] = []
    public private(set) var loadError: String?
    public var composer = ""
    public private(set) var isSending = false
    /// The bridge's refusal of a reply or a cancel, verbatim.
    public var alert: String?

    /// How many log lines the detail asks for.
    public static let tail = 200
    /// Called after an archive or an unarchive here, so the list catches up.
    @ObservationIgnored public var onArchiveChanged: (@MainActor () async -> Void)?

    private let makeClient: @MainActor () -> any StudioRunsProviding
    private let now: @Sendable () -> Date

    public init(runID: String, isCloud: Bool = false, run: StudioRun? = nil,
                makeClient: @escaping @MainActor () -> any StudioRunsProviding,
                now: @escaping @Sendable () -> Date = { Date() }) {
        self.runID = runID
        self.isCloud = isCloud
        self.run = run
        self.makeClient = makeClient
        self.now = now
    }

    public var currentDate: Date { now() }

    public var status: StudioRunStatus? { run?.status }

    /// Send needs typed (or dictated) text: on a `needs_you` run there is no empty send.
    public var canSend: Bool {
        guard let status, !isSending else { return false }
        return WorkSemantics.canSend(status, text: composer)
    }

    /// What the run is called. Never its id once the run is known.
    public var title: String {
        run.map(WorkSemantics.title) ?? "Run"
    }

    /// The question it asks, or its report's first line.
    public var headline: String? {
        run.flatMap(WorkSemantics.headline)
    }

    /// Whether the detail draws a composer at all (not while the run is being reviewed).
    public var showsComposer: Bool {
        if isCloud { return false }
        guard let status else { return true }
        return StudioRunSemantics.showsComposer(status)
    }

    public var isComposerEnabled: Bool {
        if isCloud { return false }
        guard let status else { return false }
        return StudioRunSemantics.canReply(status) && !isSending
    }

    public var showsResume: Bool {
        if isCloud { return false }
        guard let status else { return false }
        return StudioRunSemantics.offersResume(status, composer: composer)
    }

    public var canCancel: Bool {
        if isCloud { return false }
        guard let status, !isSending else { return false }
        return StudioRunSemantics.canCancel(status)
    }

    /// The toolbar's Archive: hidden for a moving run.
    public var canArchive: Bool {
        guard let run, !isSending else { return false }
        return StudioRunSemantics.canArchive(run)
    }

    /// The toolbar's Unarchive, on an archived run.
    public var canUnarchive: Bool {
        guard let run, !isSending else { return false }
        return StudioRunSemantics.canUnarchive(run)
    }

    public func archive() async {
        if await post({ try await $0.archive(id: self.runID) }) { await onArchiveChanged?() }
    }

    public func unarchive() async {
        if await post({ try await $0.unarchive(id: self.runID) }) { await onArchiveChanged?() }
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
            run = try await client.getRun(id: runID, cloud: isCloud)
            lines = try await client.runLog(id: runID, tail: Self.tail, cloud: isCloud).lines
            loadError = nil
        } catch {
            loadError = StudioRunsModel.message(error)
        }
    }

    /// Send the answer. True when the bridge took it (the composer is then cleared); on a
    /// refusal the bridge's words are in `alert` and the typed text stays.
    @discardableResult
    public func send() async -> Bool {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return false }
        if await post({ try await $0.replyToRun(id: self.runID, text: text) }) {
            composer = ""
            return true
        }
        return false
    }

    /// Show a run the list already holds, without a call: a project's Your turn rows are drawn
    /// from the list, and their detail models must not lag it.
    public func adopt(_ newer: StudioRun) {
        guard newer.runID == runID, newer.isCloud == isCloud else { return }
        run = newer
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
