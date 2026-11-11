import Foundation
import JesseNetworking

// THE WORK VIEW, WITHOUT A SCREEN: every question the Work board, a project's view and a run's
// detail ask, answered from the one strands snapshot and the one runs list, so a test can ask
// them all.
//
// The board answers three things per project, in this order: does anything need me, what is
// moving, what is next. Each row says exactly one of them, in plain words. Step ids, run ids,
// branches, rounds, audit findings and machine names are never on the first screen.

/// The one status line a Work board row carries, in precedence order.
public enum WorkRowStatus: Equatable, Sendable {
    /// A run waits on the owner, a run was cut off or failed, or the note's Waiting on is a
    /// `you:` gate.
    case yourTurn
    /// This many runs are queued, running or paused.
    case moving(Int)
    /// The first open Drafts line's title.
    case next(String)

    /// The words on the row.
    public var text: String {
        switch self {
        case .yourTurn: return "Your turn"
        case .moving(let n): return "\(n) moving"
        case .next(let title): return "Next: \(title)"
        }
    }

    /// Drawn in orange: the only status the owner can clear.
    public var isYourTurn: Bool { self == .yourTurn }
}

public enum WorkSemantics {

    // MARK: Which runs belong where

    /// A project's runs, the newest of each step only: a failure that a later run of the same
    /// step has already retried is no longer anyone's turn.
    public static func currentRuns(of slug: String, in runs: [StudioRun]) -> [StudioRun] {
        var seen = Set<String>()
        var out: [StudioRun] = []
        let mine = runs.filter { $0.strand == slug }
            .sorted { ($0.createdAt, $0.runID) > ($1.createdAt, $1.runID) }
        for run in mine {
            let key = run.itemID ?? run.runID
            if seen.insert(key).inserted { out.append(run) }
        }
        return out
    }

    /// Queued, running or paused: what the Moving section lists.
    public static func moving(_ slug: String, in runs: [StudioRun]) -> [StudioRun] {
        StudioRunSemantics.sorted(runs.filter { $0.strand == slug && $0.status.isMoving })
    }

    /// The runs that wait on the owner, in the order the Your turn section lists them:
    /// questions first, then cut off runs, then failures.
    public static func yourTurnRuns(_ slug: String, in runs: [StudioRun]) -> [StudioRun] {
        let current = currentRuns(of: slug, in: runs)
        let rank: (StudioRunStatus) -> Int? = { status in
            switch status {
            case .needsYou: return 0
            case .interrupted: return 1
            case .failed: return 2
            default: return nil
            }
        }
        return current
            .compactMap { run in rank(run.status).map { (run, $0) } }
            .sorted { $0.1 != $1.1 ? $0.1 < $1.1 : $0.0.createdAt > $1.0.createdAt }
            .map(\.0)
    }

    /// The note's `you:` gate, its words after `you:`, or nil when the note waits on nobody
    /// or on someone else.
    public static func youGate(_ strand: Strand) -> String? {
        guard let text = strand.waiting?.text.trimmingCharacters(in: .whitespacesAndNewlines),
              text.lowercased().hasPrefix("you:") else { return nil }
        let rest = text.dropFirst(4).trimmingCharacters(in: .whitespaces)
        guard let first = rest.first else { return "You" }
        return first.uppercased() + rest.dropFirst()
    }

    // MARK: The board row

    /// The row's one status line, or nil for nothing to say. `runs` is every run the list
    /// holds; with none loaded (`runsLoaded` false) the board falls back on the snapshot's
    /// own counts.
    public static func rowStatus(_ strand: Strand, runs: [StudioRun],
                                 runsLoaded: Bool = true) -> WorkRowStatus? {
        let needsYou = strand.runsNeedsYou ?? 0
        let stuck = currentRuns(of: strand.slug, in: runs)
            .contains { $0.status == .interrupted || $0.status == .failed }
        if needsYou > 0 || stuck || youGate(strand) != nil {
            return .yourTurn
        }
        let movingCount = runsLoaded
            ? moving(strand.slug, in: runs).count
            : max(0, (strand.runsActive ?? 0) - needsYou)
        if movingCount > 0 {
            return .moving(movingCount)
        }
        if let first = nextLines(strand).first {
            return .next(first.displayTitle)
        }
        if strand.drafts == nil, let next = strand.next, !next.text.isEmpty {
            return .next(next.text)
        }
        return nil
    }

    // MARK: Next

    /// The first three unticked Drafts lines outside `### Later`, in the note's order.
    public static func nextLines(_ strand: Strand, limit: Int = 3) -> [StrandDraft] {
        Array((strand.drafts ?? [])
            .filter { $0.isOpen && $0.section != "later" }
            .prefix(limit))
    }

    /// Whether `Start` is offered on a Next line: only the first, and only when the bridge
    /// says it is startable.
    public static func offersStart(_ draft: StrandDraft, at index: Int) -> Bool {
        index == 0 && draft.startable == true
    }

    /// The small line under the first Next line when it cannot start: the bridge's reason.
    public static func startNote(_ draft: StrandDraft, at index: Int) -> String? {
        guard index == 0, draft.startable != true else { return nil }
        guard let reason = draft.startableReason?
            .trimmingCharacters(in: .whitespacesAndNewlines), !reason.isEmpty else { return nil }
        return reason
    }

    // MARK: Since your last look

    /// Settled with something to read: what `Since your last look` counts.
    public static func countsAsFinished(_ status: StudioRunStatus) -> Bool {
        switch status {
        case .reported, .needsYou, .failed, .interrupted: return true
        default: return false
        }
    }

    /// When a run finished: its own stamp, else its last round's.
    public static func finishedAt(_ run: StudioRun) -> Date? {
        StudioRunSemantics.date(run.finishedAt ?? run.rounds.last?.finishedAt)
    }

    /// Runs that finished after `lastLook`, newest first. Nothing before the first look:
    /// with no last look there is nothing to compare against.
    public static func finishedSince(_ lastLook: String?, in runs: [StudioRun],
                                     strand: String? = nil) -> [StudioRun] {
        guard let since = StudioRunSemantics.date(lastLook) else { return [] }
        return runs
            .filter { strand == nil || $0.strand == strand }
            .filter { countsAsFinished($0.status) }
            .compactMap { run in finishedAt(run).map { (run, $0) } }
            .filter { $0.1 > since }
            .sorted { $0.1 > $1.1 }
            .map(\.0)
    }

    /// `3 finished since your last look`, or nil for none.
    public static func sinceLine(count: Int) -> String? {
        guard count > 0 else { return nil }
        return "\(count) finished since your last look"
    }

    // MARK: A run's words

    /// What a run is called: the bridge's title, else its Drafts line's text, else a plain
    /// placeholder. Never the run id.
    public static func title(_ run: StudioRun) -> String {
        if let t = run.title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty {
            return t
        }
        if let t = run.itemText?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty {
            return t
        }
        return "Untitled run"
    }

    /// The prefix a question's report line opens on.
    public static let needsYouPrefix = "NEEDS YOU:"

    /// The question a `needs_you` run asks: its report's first line without `NEEDS YOU:`.
    public static func question(_ run: StudioRun) -> String? {
        guard let line = run.reportLine?.trimmingCharacters(in: .whitespacesAndNewlines),
              !line.isEmpty else { return nil }
        if line.uppercased().hasPrefix(needsYouPrefix) {
            let rest = line.dropFirst(needsYouPrefix.count)
                .trimmingCharacters(in: .whitespaces)
            return rest.isEmpty ? nil : rest
        }
        return line
    }

    /// The line under a run's title on its detail: the question while it waits, else the
    /// first line of its report.
    public static func headline(_ run: StudioRun) -> String? {
        if run.status == .needsYou { return question(run) }
        return run.reportLine?.trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty
    }

    /// Whether an answer may be sent: typed (or dictated) text, and a run that takes one.
    public static func canSend(_ status: StudioRunStatus, text: String) -> Bool {
        StudioRunSemantics.canReply(status)
            && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

extension String {
    fileprivate var nilIfEmpty: String? { isEmpty ? nil : self }
}
