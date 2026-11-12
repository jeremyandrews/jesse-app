import Foundation
import JesseNetworking

// THE WORK VIEW, WITHOUT A SCREEN: every question the Work board, a project's view and a run's
// detail ask, answered from the one strands snapshot and the one runs list, so a test can ask
// them all.
//
// The board answers three things per project, in this order: does anything need me, what is
// moving, what is next. Each row says exactly one of them, in plain words. Step ids, run ids,
// branches, rounds, audit findings and machine names are never on the first screen.

/// A project's state in one word, in precedence order: what VoiceOver hears first on a Work
/// board row (HO2 drew it as the row's status line; since HO5 the row draws `WorkRowText`).
public enum WorkRowStatus: Equatable, Sendable {
    /// A run waits on the owner, a run was cut off or failed, or the note's Waiting on is a
    /// `you:` gate.
    case yourTurn
    /// This many runs are queued, running or paused.
    case moving(Int)
    /// The first open Drafts line's title.
    case next(String)

    /// The state as words: what VoiceOver says first on a row (the row itself draws
    /// `WorkRowText`).
    public var text: String {
        switch self {
        case .yourTurn: return "Your turn"
        case .moving(let n): return "\(n) moving"
        case .next(let title): return "Next: \(title)"
        }
    }

    /// The only state the owner can clear (HO2 drew it in orange; the row's orange line is now
    /// `WorkRowText.tone`, decided by the same `action`).
    public var isYourTurn: Bool { self == .yourTurn }
}

/// What a Work board row says beside its title and under it.
public struct WorkRowText: Equatable, Sendable {
    /// How the second line is drawn.
    public enum Tone: Equatable, Sendable {
        /// Something is his to do: orange.
        case you
        /// Anything else: secondary.
        case plain
    }

    /// Beside the title, in secondary, or nil.
    public var detail: String?
    /// The second line, or nil.
    public var line: String?
    public var tone: Tone
    /// The one word for what is moving (`Running`), kept beside the title when the grey text
    /// does not fit there; nil when nothing moves.
    public var marker: String?

    public init(detail: String? = nil, line: String? = nil, tone: Tone = .plain,
                marker: String? = nil) {
        self.detail = detail
        self.line = line
        self.tone = tone
        self.marker = marker
    }

    /// Line 2 when the grey text does not fit beside the title: his action stays; else what is
    /// moving (the grey text, while it is a run); else Next; else the grey text.
    public var fallbackLine: String? {
        if tone == .you { return line }
        if marker != nil, let detail { return detail }
        return line ?? detail
    }
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

    /// The project's state in one word (`WorkRowStatus`), or nil for nothing to say. `runs` is
    /// every run the list holds; with none loaded (`runsLoaded` false) it falls back on the
    /// snapshot's own counts. `Your turn` is decided by `action`, the rule the row's orange line
    /// follows, so the two never disagree.
    public static func rowStatus(_ strand: Strand, runs: [StudioRun],
                                 runsLoaded: Bool = true) -> WorkRowStatus? {
        let needsYou = strand.runsNeedsYou ?? 0
        // The board row's own rule (`action`), so the spoken state and the orange line agree.
        if action(strand, runs: runs) != nil {
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

    // MARK: The board row's words (HO5)

    /// What is his to do on this project, in the words the row shows, or nil: a run's question,
    /// a run to resume, a failed run, else the note's `you:` action. The project view's Your turn
    /// section leads with the same words.
    public static func action(_ strand: Strand, runs: [StudioRun]) -> String? {
        if let run = yourTurnRuns(strand.slug, in: runs).first {
            let title = WorkSemantics.title(run)
            switch run.status {
            case .needsYou:
                if let q = question(run).map(withoutParentheticals), !q.isEmpty {
                    return "For you: answer \u{201C}\(q)\u{201D}"
                }
                return "For you: answer \(title)"
            case .interrupted: return "For you: resume \(title)"
            default: return "For you: see why \(title) failed"
            }
        }
        if (strand.runsNeedsYou ?? 0) > 0 {
            return "For you: answer a question"
        }
        return gateLine(strand)
    }

    /// The note's `you:` gate as the row and the project view both show it: `For you: <phrase>`,
    /// or, for a gate the bridge could not put in words (`you: PR 331`), `For you: see the note`.
    /// Nil when the Waiting on is not his.
    public static func gateLine(_ strand: Strand) -> String? {
        if let phrase = gatePhrase(strand) { return "For you: \(phrase)" }
        return youGate(strand) == nil ? nil : "For you: see the note"
    }

    /// The note's `you:` action as a phrase: the bridge's `your_action`, else (an older bridge)
    /// the gate's own words.
    public static func gatePhrase(_ strand: Strand) -> String? {
        if let a = strand.yourAction?.trimmingCharacters(in: .whitespacesAndNewlines), !a.isEmpty {
            return a
        }
        // A bridge that sent `your_action: null` found nothing usable; only an older one, which
        // sends no key, leaves the gate's own words to the app.
        guard !strand.servesYourAction, let gate = youGate(strand) else { return nil }
        // `youGate` capitalizes for a sentence; after `For you:` the words read as written. A
        // first word in capitals (`SOC 2`) stays as it is.
        guard gate != "You" else { return nil }
        let first = gate.prefix { $0 != " " }
        let plainWord = first.count > 1 && first.dropFirst().allSatisfy(\.isLowercase)
        return plainWord ? gate.prefix(1).lowercased() + gate.dropFirst() : gate
    }

    /// What is moving, in words: `Running: Ablation A2`, `+ 1 more` when there are others.
    public static func doing(_ strand: Strand, runs: [StudioRun], runsLoaded: Bool) -> String? {
        guard runsLoaded else {
            let n = max(0, (strand.runsActive ?? 0) - (strand.runsNeedsYou ?? 0))
            return n > 0 ? "\(n) moving" : nil
        }
        let rank: (StudioRunStatus) -> Int = { $0 == .running ? 0 : ($0 == .paused ? 1 : 2) }
        let live = moving(strand.slug, in: runs)
            .enumerated()
            .sorted { (rank($0.element.status), $0.offset) < (rank($1.element.status), $1.offset) }
            .map(\.element)
        guard let first = live.first else { return runningLine(strand, runs: runs) }
        let word: String
        switch first.status {
        case .queued: word = "Queued"
        case .paused: word = "Paused"
        default: word = "Running"
        }
        // The others by what they are doing: a queued run is not running.
        let others = live.dropFirst()
        var more = ""
        let sameKind = others.filter { $0.status == first.status }.count
        if sameKind > 0 { more += " and \(sameKind) more" }
        let queued = others.filter { $0.status == .queued && first.status != .queued }.count
        if queued > 0 { more += ", \(queued) queued" }
        let paused = others.filter { $0.status == .paused && first.status != .paused }.count
        if paused > 0 { more += ", \(paused) paused" }
        return "\(word): \(rowTitle(title(first)))\(more)"
    }

    /// A line the note files under Running with no Studio run of its own (work under way on
    /// the cluster, by hand, in another session), as `Running: <title>`.
    static func runningLine(_ strand: Strand, runs: [StudioRun]) -> String? {
        let lines = (strand.drafts ?? []).filter { draft in
            draft.isOpen && draft.section == "running"
                && !runs.contains { $0.strand == strand.slug && $0.itemID == draft.id }
        }
        guard let first = lines.first else { return nil }
        var title = rowTitle(first.displayTitle)
        var more = ""
        if lines.count > 1 {
            // `… raidz2-3… and 4 more` reads as two cuts: the count says there is more.
            if title.hasSuffix("\u{2026}") { title.removeLast() }
            more = " and \(lines.count - 1) more"
        }
        return "Running: \(title)\(more)"
    }

    /// The first queued line, as `Next: <title>`. A line under Running is under way, not next.
    public static func nextPhrase(_ strand: Strand) -> String? {
        // The bridge has cleaned `next_title` already; only a stale `, due …` tail goes here.
        if let title = strand.nextTitle?.trimmingCharacters(in: .whitespaces), !title.isEmpty {
            return "Next: \(withoutDue(title))"
        }
        if let first = nextLines(strand, limit: .max).first(where: { $0.section == "queue" }) {
            return "Next: \(rowTitle(first.displayTitle))"
        }
        if strand.drafts == nil, let next = strand.next, !next.text.isEmpty {
            return "Next: \(next.text)"
        }
        return nil
    }

    /// A Drafts title as the row shows it: no parentheticals (one cut short included) and no
    /// `, due …` tail, whose relative date goes stale on a line nobody has ticked.
    static func rowTitle(_ title: String) -> String {
        var t = withoutDue(withoutParentheticals(title))
        // A title the bridge cut short (`… doing and…`) loses the small words it stopped on.
        if t.hasSuffix("\u{2026}") {
            var words = t.dropLast().split(separator: " ").map(String.init)
            while words.count > 1, let last = words.last,
                  WordFitLine.dangling.contains(last.lowercased()
                    .trimmingCharacters(in: .punctuationCharacters)) {
                words.removeLast()
            }
            t = words.joined(separator: " ")
                .trimmingCharacters(in: CharacterSet(charactersIn: ",;: ")) + "\u{2026}"
        }
        t = t.trimmingCharacters(in: .whitespaces)
        return t.isEmpty ? title : t
    }

    /// `title` without a `, due …` tail.
    static func withoutDue(_ title: String) -> String {
        guard let r = title.range(of: ", due ") else { return title }
        return String(title[..<r.lowerBound])
    }

    /// Whether two phrases say the same thing: past a leading `Next:`, they open on the same two
    /// words, the first ends in the second after its colon, or the first holds the second
    /// (when the second is three words or more).
    static func sameLead(_ a: String, _ b: String) -> Bool {
        let body: (String) -> String = { text in
            var t = text.lowercased()
            if t.hasPrefix("next:") { t = String(t.dropFirst(5)) }
            return t.trimmingCharacters(in: .whitespaces)
        }
        let (x, y) = (body(a), body(b))
        let words: (String) -> [String] = {
            $0.split(separator: " ").prefix(2).map {
                $0.trimmingCharacters(in: .punctuationCharacters)
            }
        }
        if words(x) == words(y) { return true }
        let tail: (String) -> String = { t in
            t.split(separator: ":", maxSplits: 1).last.map(String.init)?
                .trimmingCharacters(in: .whitespaces) ?? t
        }
        // A one or two word title (`Blog`) is inside many a headline without repeating it.
        let long = y.split(separator: " ").count >= 3
        return !y.isEmpty && (tail(x) == y || (long && x.contains(y)))
    }

    /// `s` without its parentheticals: a run's question names job labels and paths in them.
    static func withoutParentheticals(_ s: String) -> String {
        var out = ""
        var depth = 0
        for ch in s {
            if ch == "(" {
                if depth == 0 { while out.last == " " { out.removeLast() } }
                depth += 1
            } else if ch == ")" && depth > 0 {
                depth -= 1
            } else if depth == 0 {
                out.append(ch)
            }
        }
        return out.split(separator: " ").joined(separator: " ")
            .replacingOccurrences(of: " ,", with: ",")
            .replacingOccurrences(of: " ?", with: "?")
    }

    /// The row's words (docs/ho5-work-board-rows.md): beside the title, what is moving, else the
    /// note's headline; under it, what is his to do, else Next. Nil for nothing to say.
    public static func rowText(_ strand: Strand, runs: [StudioRun],
                               runsLoaded: Bool = true) -> WorkRowText? {
        let action = action(strand, runs: runs)
        let doing = doing(strand, runs: runs, runsLoaded: runsLoaded)
        let headline = strand.headline?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        let next = nextPhrase(strand)
        var detail = doing ?? headline
        let line = action ?? next
        // The headline and the Next line saying one thing twice: the line, which has the
        // whole width, keeps it.
        if action == nil, let d = detail, let l = line, sameLead(d, l) { detail = nil }
        if detail == nil && line == nil { return nil }
        // The word for what is moving, only while the grey text is what is moving.
        let marker = detail != nil && detail == doing
            ? doing?.split(separator: ":", maxSplits: 1).first.map(String.init)
            : nil
        return WorkRowText(detail: detail, line: line, tone: action != nil ? .you : .plain,
                           marker: marker)
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
