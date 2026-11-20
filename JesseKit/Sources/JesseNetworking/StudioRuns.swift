import Foundation

// The Studio run routes (`bridge/src/hostrun.rs`), decoded exactly as the bridge serializes
// them, and the calls that reach them.
//
// ## The wire
//
// A run is `run_json`: the whole `run.json` record (`RunRecord`, serde's own snake_case field
// names, no rename attribute) plus `host` and `report_paths`. `GET /jesse/runs` wraps a list of
// those with `counts`; `POST /jesse/runs` answers `started_json`; the log answers `log_json`.
// The fixtures the decoding tests read are written by the bridge's own serializers
// (`the_app_wire_fixtures_match_what_the_run_routes_serialize`), never by hand.
//
// Tolerance is the strand board's: only `run_id` is required, every other key decodes to a
// default when it is missing, and a status this build has never heard of is `.unknown`
// rather than a failed decode. A bridge that grows a status must not blank the Runs list.
//
// ## Errors
//
// A refusal is the bridge's sentence (`Refusal::message`), sent as the plain text body of a
// 4xx. It is shown VERBATIM: the bridge already names the run and the state ("HO1b is
// reported; only a queued, running, paused or reviewing run can be cancelled"), and a client paraphrase
// is how the reason gets lost. `StudioRunError.refused` carries it untouched.

/// How a run stands. The bridge's `RunStatus`, plus `unknown` for a word this build lacks.
public enum StudioRunStatus: Equatable, Hashable, Sendable, Codable {
    case queued
    case running
    case needsYou
    case reported
    case failed
    case cancelled
    case interrupted
    /// The plan's usage limit ended the round; the run continues by itself at `resume_at`.
    case paused
    /// Settled without a reply: superseded, its line ticked, its report archived, or closed
    /// (or archived) by its owner. A reply still continues it, as it does a reported run.
    case closed
    /// The implementing round finished and a second model is reading the run's diff. The
    /// bridge hands its findings to the run's own session: moving, like `running`, with
    /// nothing to reply to yet.
    case reviewing
    /// A status a newer bridge serves. Carries the word so the screen can still show it.
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "queued": self = .queued
        case "running": self = .running
        case "needs_you": self = .needsYou
        case "reported": self = .reported
        case "failed": self = .failed
        case "cancelled": self = .cancelled
        case "interrupted": self = .interrupted
        case "paused": self = .paused
        case "closed": self = .closed
        case "reviewing": self = .reviewing
        default: self = .unknown(wire)
        }
    }

    public init(from decoder: any Decoder) throws {
        let raw = (try? decoder.singleValueContainer().decode(String.self)) ?? ""
        self.init(wire: raw)
    }

    /// The wire word, as the bridge writes it.
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(wire)
    }

    /// The word on the wire.
    public var wire: String {
        switch self {
        case .queued: return "queued"
        case .running: return "running"
        case .needsYou: return "needs_you"
        case .reported: return "reported"
        case .failed: return "failed"
        case .cancelled: return "cancelled"
        case .interrupted: return "interrupted"
        case .paused: return "paused"
        case .closed: return "closed"
        case .reviewing: return "reviewing"
        case .unknown(let w): return w
        }
    }

    // Every predicate below is the bridge's own (`RunStatus` in `bridge/src/hostrun.rs`),
    // spelled out per case with no `default`, so a status added here must be decided in each.
    // The views read these; none decides for itself what a status means.

    /// Queued, running, waiting on the owner, paused or being reviewed: the bridge's
    /// `is_live`, which is what refuses a second start of the same draft.
    public var isLive: Bool {
        switch self {
        case .queued, .running, .needsYou, .paused, .reviewing: return true
        case .reported, .failed, .cancelled, .interrupted, .closed, .unknown: return false
        }
    }

    /// Something is still happening without anyone's help: queued, running, paused or being
    /// reviewed. The bridge's `is_moving` (its `moving` count), what keeps a detail screen
    /// polling, and what the bridge refuses to archive.
    public var isMoving: Bool {
        switch self {
        case .queued, .running, .paused, .reviewing: return true
        case .needsYou, .reported, .failed, .cancelled, .interrupted, .closed, .unknown:
            return false
        }
    }

    /// Running, waiting on the owner, paused or being reviewed: the bridge's `is_active`, what
    /// a strand's `runs_active` counts.
    public var isActive: Bool {
        switch self {
        case .running, .needsYou, .paused, .reviewing: return true
        case .queued, .reported, .failed, .cancelled, .interrupted, .closed, .unknown:
            return false
        }
    }

    /// What may carry `archived_at` and leave the list: the bridge's `is_archivable`
    /// (reported, failed, cancelled or closed). A word this build lacks is taken at the
    /// bridge's word: archived when `archived_at` says so.
    public var isArchivable: Bool {
        switch self {
        case .reported, .failed, .cancelled, .closed, .unknown: return true
        case .queued, .running, .needsYou, .interrupted, .paused, .reviewing: return false
        }
    }

    /// What the bridge's cancel route accepts: a queued, running, paused or reviewing run.
    /// A reviewing run is cancelled at once and its review recorded as failed.
    public var isCancellable: Bool {
        switch self {
        case .queued, .running, .paused, .reviewing: return true
        case .needsYou, .reported, .failed, .cancelled, .interrupted, .closed, .unknown:
            return false
        }
    }

    /// Waits on its owner: a question, a cut off round, or a failure. The `Needs me` filter,
    /// the bridge's `needs_me` count.
    public var needsMe: Bool {
        switch self {
        case .needsYou, .interrupted, .failed: return true
        case .queued, .running, .reported, .cancelled, .paused, .closed, .reviewing, .unknown:
            return false
        }
    }

    /// Done with: neither moving nor waiting on its owner. The `Finished` filter.
    public var isFinished: Bool { !isMoving && !needsMe }
}

/// One round of a run: the first run of the prompt, or one reply.
public struct StudioRunRound: Decodable, Equatable, Hashable, Sendable {
    public var n: Int
    /// The reply text; nil for round 1.
    public var text: String?
    public var queuedAt: String?
    public var startedAt: String?
    public var finishedAt: String?
    public var reportPath: String?
    public var outcome: StudioRunStatus?
    public var cancelRequestedAt: String?
    /// Who queued the round when no person did (the usage limit, a restart).
    public var auto: String?

    public init(n: Int, text: String? = nil, queuedAt: String? = nil, startedAt: String? = nil,
                finishedAt: String? = nil, reportPath: String? = nil,
                outcome: StudioRunStatus? = nil, cancelRequestedAt: String? = nil,
                auto: String? = nil) {
        self.n = n
        self.text = text
        self.queuedAt = queuedAt
        self.startedAt = startedAt
        self.finishedAt = finishedAt
        self.reportPath = reportPath
        self.outcome = outcome
        self.cancelRequestedAt = cancelRequestedAt
        self.auto = auto
    }

    private enum CodingKeys: String, CodingKey {
        case n, text, outcome, auto
        case queuedAt = "queued_at"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case reportPath = "report_path"
        case cancelRequestedAt = "cancel_requested_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        n = try c.decodeIfPresent(Int.self, forKey: .n) ?? 0
        text = try c.decodeIfPresent(String.self, forKey: .text)
        queuedAt = try c.decodeIfPresent(String.self, forKey: .queuedAt)
        startedAt = try c.decodeIfPresent(String.self, forKey: .startedAt)
        finishedAt = try c.decodeIfPresent(String.self, forKey: .finishedAt)
        reportPath = try c.decodeIfPresent(String.self, forKey: .reportPath)
        outcome = try c.decodeIfPresent(StudioRunStatus.self, forKey: .outcome)
        cancelRequestedAt = try c.decodeIfPresent(String.self, forKey: .cancelRequestedAt)
        auto = try c.decodeIfPresent(String.self, forKey: .auto)
    }
}

/// Where one review of a run stands: the bridge's `ReviewStatus`, plus `unknown` for a word
/// this build lacks.
public enum StudioRunReviewStatus: Equatable, Hashable, Sendable, Decodable {
    /// The reviewer is reading the diff.
    case running
    /// The note is written and not yet handed to the run.
    case ready
    /// The run's session has the findings; its answering round is queued or running.
    case handedOff
    /// The answering round reported; the note is in `Inbox/archive/`.
    case answered
    /// The review could not run.
    case failed
    /// Never handed off, and the run was closed or archived.
    case dropped
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "running": self = .running
        case "ready": self = .ready
        case "handed_off": self = .handedOff
        case "answered": self = .answered
        case "failed": self = .failed
        case "dropped": self = .dropped
        default: self = .unknown(wire)
        }
    }

    public init(from decoder: any Decoder) throws {
        let raw = (try? decoder.singleValueContainer().decode(String.self)) ?? ""
        self.init(wire: raw)
    }

    public var wire: String {
        switch self {
        case .running: return "running"
        case .ready: return "ready"
        case .handedOff: return "handed_off"
        case .answered: return "answered"
        case .failed: return "failed"
        case .dropped: return "dropped"
        case .unknown(let w): return w
        }
    }
}

/// One review of a run, as the run routes serve it (`reviews_json`): who read it, where it
/// stands and where its note is. The findings are in the note, never here.
public struct StudioRunReview: Decodable, Equatable, Hashable, Sendable {
    /// 1, 2, …: the note's `-review-<n>`.
    public var n: Int
    /// The registry id of the reviewing model.
    public var reviewer: String
    /// Its label when the review was asked for (`GLM 5.3`). Empty from a record that lacks it.
    public var reviewerLabel: String
    /// `start` (a reviewed run's own review) or `request` (asked for after the fact).
    public var origin: String
    public var status: StudioRunReviewStatus
    public var focus: String?
    public var autoReply: Bool?
    public var requestedAt: String?
    public var finishedAt: String?
    /// The commit reviewed and the merge base its diff starts from.
    public var head: String?
    public var base: String?
    /// The note under the notes directory, where it is now (`Inbox/` or `Inbox/archive/`).
    public var path: String?

    public init(n: Int, reviewer: String = "", reviewerLabel: String = "", origin: String = "",
                status: StudioRunReviewStatus = .running, focus: String? = nil,
                autoReply: Bool? = nil, requestedAt: String? = nil, finishedAt: String? = nil,
                head: String? = nil, base: String? = nil, path: String? = nil) {
        self.n = n
        self.reviewer = reviewer
        self.reviewerLabel = reviewerLabel
        self.origin = origin
        self.status = status
        self.focus = focus
        self.autoReply = autoReply
        self.requestedAt = requestedAt
        self.finishedAt = finishedAt
        self.head = head
        self.base = base
        self.path = path
    }

    private enum CodingKeys: String, CodingKey {
        case n, reviewer, origin, status, focus, head, base, path
        case reviewerLabel = "reviewer_label"
        case autoReply = "auto_reply"
        case requestedAt = "requested_at"
        case finishedAt = "finished_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        n = try c.decodeIfPresent(Int.self, forKey: .n) ?? 0
        reviewer = try c.decodeIfPresent(String.self, forKey: .reviewer) ?? ""
        reviewerLabel = try c.decodeIfPresent(String.self, forKey: .reviewerLabel) ?? ""
        origin = try c.decodeIfPresent(String.self, forKey: .origin) ?? ""
        status = try c.decodeIfPresent(StudioRunReviewStatus.self, forKey: .status)
            ?? .unknown("")
        focus = try c.decodeIfPresent(String.self, forKey: .focus)
        autoReply = try c.decodeIfPresent(Bool.self, forKey: .autoReply)
        requestedAt = try c.decodeIfPresent(String.self, forKey: .requestedAt)
        finishedAt = try c.decodeIfPresent(String.self, forKey: .finishedAt)
        head = try c.decodeIfPresent(String.self, forKey: .head)
        base = try c.decodeIfPresent(String.self, forKey: .base)
        path = try c.decodeIfPresent(String.self, forKey: .path)
    }

    /// Who reviewed it, as a person reads it: the label, else the registry id.
    public var reviewerName: String {
        let label = reviewerLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        return label.isEmpty ? reviewer : label
    }
}

/// One run, as `run_json` serves it.
public struct StudioRun: Decodable, Equatable, Hashable, Identifiable, Sendable {
    public var runID: String
    public var strand: String?
    public var itemID: String?
    /// The draft the prompt came from, under the notes directory.
    public var promptPath: String?
    public var itemText: String?
    public var repo: String
    public var branch: String
    public var origin: String
    public var status: StudioRunStatus
    /// Why a queued run is not running: `slot`, `repo <owner/name>`, `disk`, `limit`.
    public var waitingFor: String?
    public var createdAt: String
    public var queuedAt: String?
    public var startedAt: String?
    public var finishedAt: String?
    public var lastEventAt: String?
    public var stalledSince: String?
    public var rounds: [StudioRunRound]
    public var model: String?
    public var error: String?
    /// While `paused`: when the bridge queues the next round by itself.
    public var resumeAt: String?
    public var host: String?
    /// Every round's Inbox report, vault relative, oldest first.
    public var reportPaths: [String]
    /// What the run is called (`work::run_title`): its Drafts line's title, its prompt's
    /// heading, or its branch, never its id. Nil from a bridge that predates the Work view.
    public var title: String?
    /// The last non blank line of its readable log, at most 200 characters. Nil before
    /// anything is logged, and from an older bridge.
    public var lastLine: String?
    /// The first line of its newest report. Nil before a report, and from an older bridge.
    public var reportLine: String?
    /// When it left the runs list (RFC 3339). Nil while it is in the list, and from a bridge
    /// that predates archiving.
    public var archivedAt: String?
    /// Why: `archived by Jeremy`, `item ticked`, `report archived`, `finished over 14 days ago`.
    public var archivedReason: String?
    /// A reviewed run's reviewer, a registry id. Nil for a run started without one, and from a
    /// bridge that predates review (0.211.0).
    public var reviewer: String?
    /// The reviewer's label when the run was started (`GLM 5.3`).
    public var reviewerLabel: String?
    /// What the start asked the reviewer to look at most closely.
    public var reviewFocus: String?
    /// Every review of the run, oldest first. Empty for a run never reviewed.
    public var reviews: [StudioRunReview]

    public var id: String { runID }

    public init(runID: String, strand: String? = nil, itemID: String? = nil,
                promptPath: String? = nil, itemText: String? = nil, repo: String = "",
                branch: String = "", origin: String = "", status: StudioRunStatus = .queued,
                waitingFor: String? = nil, createdAt: String = "", queuedAt: String? = nil,
                startedAt: String? = nil, finishedAt: String? = nil,
                lastEventAt: String? = nil, stalledSince: String? = nil,
                rounds: [StudioRunRound] = [], model: String? = nil, error: String? = nil,
                resumeAt: String? = nil, host: String? = nil, reportPaths: [String] = [],
                title: String? = nil, lastLine: String? = nil, reportLine: String? = nil,
                archivedAt: String? = nil, archivedReason: String? = nil,
                reviewer: String? = nil, reviewerLabel: String? = nil,
                reviewFocus: String? = nil, reviews: [StudioRunReview] = []) {
        self.runID = runID
        self.strand = strand
        self.itemID = itemID
        self.promptPath = promptPath
        self.itemText = itemText
        self.repo = repo
        self.branch = branch
        self.origin = origin
        self.status = status
        self.waitingFor = waitingFor
        self.createdAt = createdAt
        self.queuedAt = queuedAt
        self.startedAt = startedAt
        self.finishedAt = finishedAt
        self.lastEventAt = lastEventAt
        self.stalledSince = stalledSince
        self.rounds = rounds
        self.model = model
        self.error = error
        self.resumeAt = resumeAt
        self.host = host
        self.reportPaths = reportPaths
        self.title = title
        self.lastLine = lastLine
        self.reportLine = reportLine
        self.archivedAt = archivedAt
        self.archivedReason = archivedReason
        self.reviewer = reviewer
        self.reviewerLabel = reviewerLabel
        self.reviewFocus = reviewFocus
        self.reviews = reviews
    }

    private enum CodingKeys: String, CodingKey {
        case strand, repo, branch, origin, status, rounds, model, error, host, title
        case reviewer, reviews
        case reviewerLabel = "reviewer_label"
        case reviewFocus = "review_focus"
        case runID = "run_id"
        case itemID = "item_id"
        case promptPath = "prompt_path"
        case itemText = "item_text"
        case waitingFor = "waiting_for"
        case createdAt = "created_at"
        case queuedAt = "queued_at"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case lastEventAt = "last_event_at"
        case stalledSince = "stalled_since"
        case resumeAt = "resume_at"
        case reportPaths = "report_paths"
        case lastLine = "last_line"
        case reportLine = "report_line"
        case archivedAt = "archived_at"
        case archivedReason = "archived_reason"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runID = try c.decode(String.self, forKey: .runID)
        strand = try c.decodeIfPresent(String.self, forKey: .strand)
        itemID = try c.decodeIfPresent(String.self, forKey: .itemID)
        promptPath = try c.decodeIfPresent(String.self, forKey: .promptPath)
        itemText = try c.decodeIfPresent(String.self, forKey: .itemText)
        repo = try c.decodeIfPresent(String.self, forKey: .repo) ?? ""
        branch = try c.decodeIfPresent(String.self, forKey: .branch) ?? ""
        origin = try c.decodeIfPresent(String.self, forKey: .origin) ?? ""
        status = try c.decodeIfPresent(StudioRunStatus.self, forKey: .status) ?? .unknown("")
        waitingFor = try c.decodeIfPresent(String.self, forKey: .waitingFor)
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        queuedAt = try c.decodeIfPresent(String.self, forKey: .queuedAt)
        startedAt = try c.decodeIfPresent(String.self, forKey: .startedAt)
        finishedAt = try c.decodeIfPresent(String.self, forKey: .finishedAt)
        lastEventAt = try c.decodeIfPresent(String.self, forKey: .lastEventAt)
        stalledSince = try c.decodeIfPresent(String.self, forKey: .stalledSince)
        rounds = try c.decodeIfPresent([StudioRunRound].self, forKey: .rounds) ?? []
        model = try c.decodeIfPresent(String.self, forKey: .model)
        error = try c.decodeIfPresent(String.self, forKey: .error)
        resumeAt = try c.decodeIfPresent(String.self, forKey: .resumeAt)
        host = try c.decodeIfPresent(String.self, forKey: .host)
        reportPaths = try c.decodeIfPresent([String].self, forKey: .reportPaths) ?? []
        title = try c.decodeIfPresent(String.self, forKey: .title)
        lastLine = try c.decodeIfPresent(String.self, forKey: .lastLine)
        reportLine = try c.decodeIfPresent(String.self, forKey: .reportLine)
        archivedAt = try c.decodeIfPresent(String.self, forKey: .archivedAt)
        archivedReason = try c.decodeIfPresent(String.self, forKey: .archivedReason)
        reviewer = try c.decodeIfPresent(String.self, forKey: .reviewer)
        reviewerLabel = try c.decodeIfPresent(String.self, forKey: .reviewerLabel)
        reviewFocus = try c.decodeIfPresent(String.self, forKey: .reviewFocus)
        reviews = try c.decodeIfPresent([StudioRunReview].self, forKey: .reviews) ?? []
    }

    public static func decode(from data: Data) throws -> StudioRun {
        try JSONDecoder().decode(StudioRun.self, from: data)
    }

    /// The current round's number, or 0 before the first.
    public var round: Int { rounds.last?.n ?? 0 }

    /// The newest report, the one the detail screen links.
    public var latestReportPath: String? { reportPaths.last }

    /// Out of the runs list: archived, and not since put back to work. The bridge's
    /// `is_archived`: a run a reply has queued again (or that is waiting on its owner) shows
    /// in the list even with `archived_at` still set.
    public var isArchived: Bool {
        archivedAt != nil && status.isArchivable
    }

    /// Whether the run has a Review section: a reviewer, or any review.
    public var isReviewed: Bool { reviewer != nil || !reviews.isEmpty }

    /// Who is reviewing (or will review) the run, as a person reads it: the newest review's
    /// label, else the run's `reviewer_label`, else its reviewer id. Nil for a run never
    /// reviewed.
    public var reviewerName: String? {
        if let latest = reviews.last, !latest.reviewerName.isEmpty { return latest.reviewerName }
        if let label = reviewerLabel?.trimmingCharacters(in: .whitespacesAndNewlines),
           !label.isEmpty { return label }
        return reviewer
    }

    /// When anything last happened to the run: its last agent event, else the last of its
    /// own stamps.
    public var updatedAt: String {
        [lastEventAt, finishedAt, startedAt, queuedAt].compactMap { $0 }.max() ?? createdAt
    }
}

/// The bridge's own tallies in `GET /jesse/runs`.
public struct StudioRunCounts: Decodable, Equatable, Sendable {
    public var running: Int
    public var queued: Int
    public var needsYou: Int
    public var paused: Int
    /// The four filters' tallies over the whole store, whatever the list's mode. Nil from a
    /// bridge that predates archiving; the app then counts what it holds.
    public var needsMe: Int?
    public var moving: Int?
    public var finished: Int?
    public var archived: Int?

    public init(running: Int = 0, queued: Int = 0, needsYou: Int = 0, paused: Int = 0,
                needsMe: Int? = nil, moving: Int? = nil, finished: Int? = nil,
                archived: Int? = nil) {
        self.running = running
        self.queued = queued
        self.needsYou = needsYou
        self.paused = paused
        self.needsMe = needsMe
        self.moving = moving
        self.finished = finished
        self.archived = archived
    }

    private enum CodingKeys: String, CodingKey {
        case running, queued, paused, moving, finished, archived
        case needsYou = "needs_you"
        case needsMe = "needs_me"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        running = try c.decodeIfPresent(Int.self, forKey: .running) ?? 0
        queued = try c.decodeIfPresent(Int.self, forKey: .queued) ?? 0
        needsYou = try c.decodeIfPresent(Int.self, forKey: .needsYou) ?? 0
        paused = try c.decodeIfPresent(Int.self, forKey: .paused) ?? 0
        needsMe = try c.decodeIfPresent(Int.self, forKey: .needsMe)
        moving = try c.decodeIfPresent(Int.self, forKey: .moving)
        finished = try c.decodeIfPresent(Int.self, forKey: .finished)
        archived = try c.decodeIfPresent(Int.self, forKey: .archived)
    }
}

/// Which runs `GET /jesse/runs` serves: the list without archived runs, only the archived
/// ones (newest archived first), or all of them. An older bridge ignores the query and serves
/// every run, none of them archived.
public enum StudioRunArchivedMode: String, Equatable, Sendable {
    case hide, only, all

    /// The query item, none for the default.
    var queryItems: [URLQueryItem] {
        self == .hide ? [] : [URLQueryItem(name: "archived", value: rawValue)]
    }
}

/// `GET /jesse/runs`. `disk` and `orphans` are housekeeping for the Studio's owner at a
/// terminal and are not decoded.
public struct StudioRunList: Decodable, Equatable, Sendable {
    public var runs: [StudioRun]
    public var counts: StudioRunCounts

    public init(runs: [StudioRun] = [], counts: StudioRunCounts = StudioRunCounts()) {
        self.runs = runs
        self.counts = counts
    }

    private enum CodingKeys: String, CodingKey { case runs, counts }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runs = try c.decodeIfPresent([StudioRun].self, forKey: .runs) ?? []
        counts = try c.decodeIfPresent(StudioRunCounts.self, forKey: .counts) ?? StudioRunCounts()
    }

    public static func decode(from data: Data) throws -> StudioRunList {
        try JSONDecoder().decode(StudioRunList.self, from: data)
    }
}

/// What `POST /jesse/runs` answers.
public struct StudioRunStarted: Decodable, Equatable, Sendable {
    public var runID: String
    public var status: StudioRunStatus
    public var waitingFor: String?

    public init(runID: String, status: StudioRunStatus, waitingFor: String? = nil) {
        self.runID = runID
        self.status = status
        self.waitingFor = waitingFor
    }

    private enum CodingKeys: String, CodingKey {
        case status
        case runID = "run_id"
        case waitingFor = "waiting_for"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runID = try c.decode(String.self, forKey: .runID)
        status = try c.decodeIfPresent(StudioRunStatus.self, forKey: .status) ?? .queued
        waitingFor = try c.decodeIfPresent(String.self, forKey: .waitingFor)
    }

    public static func decode(from data: Data) throws -> StudioRunStarted {
        try JSONDecoder().decode(StudioRunStarted.self, from: data)
    }
}

/// What `GET /jesse/runs/:run_id/log` answers: the readable lines, oldest first.
public struct StudioRunLog: Decodable, Equatable, Sendable {
    public var runID: String
    public var lines: [String]

    public init(runID: String, lines: [String] = []) {
        self.runID = runID
        self.lines = lines
    }

    private enum CodingKeys: String, CodingKey {
        case lines
        case runID = "run_id"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runID = try c.decodeIfPresent(String.self, forKey: .runID) ?? ""
        lines = try c.decodeIfPresent([String].self, forKey: .lines) ?? []
    }

    public static func decode(from data: Data) throws -> StudioRunLog {
        try JSONDecoder().decode(StudioRunLog.self, from: data)
    }
}

/// `GET`/`POST /jesse/work/last-look`: when the Work board was last looked at, on any device.
public struct WorkLastLook: Decodable, Equatable, Sendable {
    /// RFC 3339, or nil before the first look (and from a bridge with no state directory).
    public var lastLook: String?

    public init(lastLook: String? = nil) {
        self.lastLook = lastLook
    }

    private enum CodingKeys: String, CodingKey {
        case lastLook = "last_look"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        lastLook = try c.decodeIfPresent(String.self, forKey: .lastLook)
    }

    public static func decode(from data: Data) throws -> WorkLastLook {
        try JSONDecoder().decode(WorkLastLook.self, from: data)
    }
}

/// A run route failed.
public enum StudioRunError: LocalizedError, Equatable, Sendable {
    /// The bridge answered with a refusal; `message` is its own sentence, shown verbatim.
    case refused(status: Int, message: String)
    /// Anything else: the transport's own description.
    case other(String)

    public var errorDescription: String? {
        switch self {
        case .refused(let status, let message):
            let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? "The bridge answered \(status) with no reason." : trimmed
        case .other(let message):
            return message
        }
    }
}

/// The run calls and the two last look calls. A seam so the screens' models can be driven by a
/// fake in a test.
public protocol StudioRunsProviding: Sendable {
    /// `GET /jesse/runs`, `?archived=only|all` for the other modes.
    func listRuns(archived: StudioRunArchivedMode) async throws -> StudioRunList
    func startRun(strand: String, itemID: String) async throws -> StudioRunStarted
    func getRun(id: String) async throws -> StudioRun
    func runLog(id: String, tail: Int?) async throws -> StudioRunLog
    func replyToRun(id: String, text: String) async throws -> StudioRun
    func cancelRun(id: String) async throws -> StudioRun
    /// `POST /jesse/runs/:id/archive`: out of the list. Refused (409) for a queued, running,
    /// paused or reviewing run; a `needs_you` or `interrupted` run is closed first.
    func archive(id: String) async throws -> StudioRun
    /// `POST /jesse/runs/:id/unarchive`: back in the list.
    func unarchive(id: String) async throws -> StudioRun
    /// `GET /jesse/work/last-look`.
    func lastLook() async throws -> WorkLastLook
    /// `POST /jesse/work/last-look`: the last look becomes now.
    func markLastLook() async throws -> WorkLastLook
}

extension StudioRunsProviding {
    /// The list as the app shows it: every run not archived.
    public func listRuns() async throws -> StudioRunList {
        try await listRuns(archived: .hide)
    }
}

extension JesseBridgeClient: StudioRunsProviding {

    public func listRuns(archived: StudioRunArchivedMode) async throws -> StudioRunList {
        let data = try await runCall("/jesse/runs", method: "GET", query: archived.queryItems)
        return try runDecode { try StudioRunList.decode(from: data) }
    }

    public func archive(id: String) async throws -> StudioRun {
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))/archive",
                                     method: "POST", body: [String: String]())
        return try runDecode { try StudioRun.decode(from: data) }
    }

    public func unarchive(id: String) async throws -> StudioRun {
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))/unarchive",
                                     method: "POST", body: [String: String]())
        return try runDecode { try StudioRun.decode(from: data) }
    }

    public func startRun(strand: String, itemID: String) async throws -> StudioRunStarted {
        let data = try await runCall("/jesse/runs", method: "POST",
                                     body: ["strand": strand, "item_id": itemID])
        return try runDecode { try StudioRunStarted.decode(from: data) }
    }

    public func getRun(id: String) async throws -> StudioRun {
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))", method: "GET")
        return try runDecode { try StudioRun.decode(from: data) }
    }

    public func runLog(id: String, tail: Int? = nil) async throws -> StudioRunLog {
        let query = tail.map { [URLQueryItem(name: "tail", value: String($0))] } ?? []
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))/log", method: "GET",
                                     query: query)
        return try runDecode { try StudioRunLog.decode(from: data) }
    }

    public func replyToRun(id: String, text: String) async throws -> StudioRun {
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))/reply",
                                     method: "POST", body: ["text": text])
        return try runDecode { try StudioRun.decode(from: data) }
    }

    public func cancelRun(id: String) async throws -> StudioRun {
        let data = try await runCall("/jesse/runs/\(Self.runPathEscaped(id))/cancel",
                                     method: "POST", body: [String: String]())
        return try runDecode { try StudioRun.decode(from: data) }
    }

    public func lastLook() async throws -> WorkLastLook {
        let data = try await runCall("/jesse/work/last-look", method: "GET")
        return try runDecode { try WorkLastLook.decode(from: data) }
    }

    public func markLastLook() async throws -> WorkLastLook {
        let data = try await runCall("/jesse/work/last-look", method: "POST",
                                     body: [String: String]())
        return try runDecode { try WorkLastLook.decode(from: data) }
    }

    /// One call: the paired request, the bridge's own words for any refusal.
    private func runCall(_ path: String, method: String, query: [URLQueryItem] = [],
                         body: [String: String]? = nil) async throws -> Data {
        guard var req = todayRequest(path, method: method, query: query) else {
            throw JesseError.notConfigured
        }
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, http) = try await todaySend(req)
        guard (200..<300).contains(http.statusCode) else {
            throw StudioRunError.refused(status: http.statusCode, message: Self.bodyText(data))
        }
        return data
    }

    /// A run id as one path segment. Only what is not an RFC 3986 unreserved character is
    /// escaped: the shared `pathEscaped` turns `-` into `%2D`, which the bridge's router does not
    /// decode, so every free run (`s-…`) answered "no run" to its detail, log, reply, cancel and
    /// archive.
    static func runPathEscaped(_ id: String) -> String {
        var unreserved = CharacterSet.alphanumerics
        unreserved.insert(charactersIn: "-._~")
        return id.addingPercentEncoding(withAllowedCharacters: unreserved) ?? id
    }

    private func runDecode<T>(_ decode: () throws -> T) throws -> T {
        do { return try decode() } catch { throw JesseError.decoding }
    }
}
