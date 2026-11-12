import Foundation

// The Decodable models for the strand endpoints (`bridge/src/strands.rs`), mirroring
// the bridge's serialization exactly.
//
// ## Two wire dialects in one package, on purpose
//
// `TodaySnapshot` decodes camelCase because `today.rs` carries
// `#[serde(rename_all = "camelCase")]`. The strand structs carry no such attribute, so
// they serialize with Rust's own field names: `global_findings`, `waits_on`,
// `generated_at`. That is the wire, and it is spelled out here in explicit
// `CodingKeys` rather than papered over with a `.convertFromSnakeCase` decoder — a
// key strategy set on the decoder would also rewrite every key of anything else
// decoded through it, and the one thing this file must not do is make the day file
// and the strand board disagree about what a key is called.
//
// Tolerance is `TodaySnapshot`'s, for the same reason: a bridge that grows a field
// and one that predates a field must both decode, and one field the bridge stops
// emitting must not blank the screen. Only `slug` is required, because a strand the
// client cannot address is not one it can render.
//
// These are PURE DATA. Every judgement drawn from them — the order of the list, which
// rows group under which heading, what the row says about a gate — lives in
// `StrandsSemantics` (JesseTodayDisplay), never here and never in a view.

/// One thing the nightly audit found wrong with a note.
///
/// `code` stays a `String` rather than an enum: the bridge documents seventeen codes
/// and reserves the right to add more, and an unknown code must render as a line of
/// text rather than fail the whole snapshot's decode. The same reasoning `TodayLink.kind`
/// is a string for.
public struct StrandFinding: Decodable, Equatable, Hashable, Identifiable, Sendable {
    public var code: String
    public var message: String
    /// The 1-based line of the note. Absent on a GLOBAL finding, which is about a file
    /// that is not a strand note at all.
    public var line: Int?

    /// A finding has no id of its own on the wire, and does not need one: no two
    /// findings on one note share both a code and a message, and this is only ever used
    /// to key a `ForEach` over one note's list.
    public var id: String { "\(code)|\(line ?? 0)|\(message)" }

    public init(code: String, message: String = "", line: Int? = nil) {
        self.code = code
        self.message = message
        self.line = line
    }

    private enum CodingKeys: String, CodingKey { case code, message, line }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        code = try c.decodeIfPresent(String.self, forKey: .code) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        line = try c.decodeIfPresent(Int.self, forKey: .line)
    }
}

/// The note's `**Waiting on:**` line.
///
/// `jeremy` is the one bit the phone renders differently, and the bridge says why: a
/// gate on the operator is one he can clear right now, and a gate on a provider or a
/// dependency is not. The row turns the first into a caption and leaves the second as
/// part of the sentence.
public struct StrandWaiting: Decodable, Equatable, Hashable, Sendable {
    public var text: String
    public var jeremy: Bool

    public init(text: String, jeremy: Bool = false) {
        self.text = text
        self.jeremy = jeremy
    }

    private enum CodingKeys: String, CodingKey { case text, jeremy }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        jeremy = try c.decodeIfPresent(Bool.self, forKey: .jeremy) ?? false
    }
}

/// The next step: the first unchecked `## Queue` item above `### Later`.
///
/// `link` and `waitsOn` are genuinely optional on the wire (the bridge serializes them
/// as `null` rather than omitting them), and both mean something by their absence: no
/// link is a step with no draft behind it yet, and no `waits_on` is a step nothing is
/// blocking.
public struct StrandNext: Decodable, Equatable, Hashable, Sendable {
    public var id: String
    public var text: String
    /// A vault target, workspace relative and without `.md` — the same shape a wiki
    /// link carries.
    public var link: String?
    public var waitsOn: String?

    public init(id: String, text: String = "", link: String? = nil, waitsOn: String? = nil) {
        self.id = id
        self.text = text
        self.link = link
        self.waitsOn = waitsOn
    }

    /// `waits_on`, not `waitsOn`. See the note at the top of the file.
    private enum CodingKeys: String, CodingKey {
        case id, text, link
        case waitsOn = "waits_on"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        link = try c.decodeIfPresent(String.self, forKey: .link)
        waitsOn = try c.decodeIfPresent(String.self, forKey: .waitsOn)
    }
}

/// One note's section tallies.
public struct StrandCounts: Decodable, Equatable, Hashable, Sendable {
    public var queue: Int
    public var later: Int
    public var running: Int
    public var done: Int

    public init(queue: Int = 0, later: Int = 0, running: Int = 0, done: Int = 0) {
        self.queue = queue
        self.later = later
        self.running = running
        self.done = done
    }

    private enum CodingKeys: String, CodingKey { case queue, later, running, done }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        queue = try c.decodeIfPresent(Int.self, forKey: .queue) ?? 0
        later = try c.decodeIfPresent(Int.self, forKey: .later) ?? 0
        running = try c.decodeIfPresent(Int.self, forKey: .running) ?? 0
        done = try c.decodeIfPresent(Int.self, forKey: .done) ?? 0
    }
}

/// Where a strand stands as a whole.
///
/// A closed set with a tolerant decode, exactly like `TodayProject`: the bridge serves
/// three states and never serves `done`, and a state this build has never heard of
/// decodes to `.active` rather than throwing. `.active` is the right fallback because
/// it is the one that keeps the row visible and unqualified — a strand quietly filed
/// under a collapsed `Dormant` heading because the bridge grew a fourth word would be
/// work disappearing off the screen.
public enum StrandState: String, CaseIterable, Codable, Equatable, Hashable, Sendable {
    case active
    case waiting
    case dormant

    public init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = StrandState(rawValue: raw) ?? .active
    }

    /// Whether a strand in this state belongs in the collapsed group at the bottom of
    /// the list rather than in the body of it.
    public var isDormant: Bool { self == .dormant }
}

/// One checkbox line of a strand note, as `annotate_strands` serves it under `drafts`.
///
/// `studioRunnable` is the bridge's own answer to "would a person's start of this line with
/// no repository be accepted", so the app never re-derives eligibility. It does NOT say
/// whether the line already has a live run: the start route answers that with a 409, and the
/// app checks the runs list first so it never offers a second start.
public struct StrandDraft: Decodable, Equatable, Hashable, Identifiable, Sendable {
    public var id: String
    public var text: String
    /// `queue`, `later`, `running` or `done`. A string, for `StrandFinding.code`'s reason.
    public var section: String
    public var checked: Bool
    public var studioRunnable: Bool
    /// The line's plain title (`work::line_title`): the words before its first colon, cut
    /// at 57 characters. Nil from a bridge that predates the Work view.
    public var title: String?
    /// Whether the Work view may offer `Start` on this line: runnable, not under Later,
    /// waiting on nothing, its draft aimed at this bridge's machine, and no live run. Nil from
    /// an older bridge, which the view treats as not startable.
    public var startable: Bool?
    /// Why `startable` is false, in the bridge's words. Nil when it is startable.
    public var startableReason: String?

    public init(id: String, text: String = "", section: String = "queue",
                checked: Bool = false, studioRunnable: Bool = false,
                title: String? = nil, startable: Bool? = nil, startableReason: String? = nil) {
        self.id = id
        self.text = text
        self.section = section
        self.checked = checked
        self.studioRunnable = studioRunnable
        self.title = title
        self.startable = startable
        self.startableReason = startableReason
    }

    private enum CodingKeys: String, CodingKey {
        case id, text, section, checked, title, startable
        case studioRunnable = "studio_runnable"
        case startableReason = "startable_reason"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        section = try c.decodeIfPresent(String.self, forKey: .section) ?? ""
        checked = try c.decodeIfPresent(Bool.self, forKey: .checked) ?? false
        studioRunnable = try c.decodeIfPresent(Bool.self, forKey: .studioRunnable) ?? false
        title = try c.decodeIfPresent(String.self, forKey: .title)
        startable = try c.decodeIfPresent(Bool.self, forKey: .startable)
        startableReason = try c.decodeIfPresent(String.self, forKey: .startableReason)
    }

    /// What the line is called on screen: the bridge's title, else its own text.
    public var displayTitle: String {
        if let title, !title.isEmpty { return title }
        return text
    }

    /// An open line the strand's view lists: unchecked and not yet done.
    public var isOpen: Bool { !checked && section != "done" }
}

/// One status note, as the bridge parsed and audited it.
public struct Strand: Decodable, Equatable, Hashable, Identifiable, Sendable {
    /// The file stem of `Strands/<slug>.md`, and the only way a client addresses one.
    public var slug: String
    public var title: String
    /// The Dashboard topic the note's frontmatter files it under. The SAME five slugs
    /// a day-file item carries, decoded by the same closed enum, so one palette and one
    /// grouping order serve both screens.
    public var group: TodayProject
    public var state: StrandState
    /// `YYYY-MM-DD`, the note's own frontmatter value. The list's default order.
    public var updated: String
    public var repos: [String]
    public var now: String?
    public var waiting: StrandWaiting?
    public var next: StrandNext?
    public var counts: StrandCounts
    public var findings: [StrandFinding]
    /// The slug of the live strand this one sits under, or nil at the top level. Only
    /// ever the bridge's word: the app never reads a note to find it. A declared parent
    /// the bridge could not resolve arrives as nil with a `PARENT-MISSING` finding.
    public var parent: String?
    /// Whether the bridge sent `parent` at all, `null` included. False only from a
    /// bridge that predates the key, and what hides the `Tree` lens rather than drawing
    /// every strand at the top level as if the vault had no tree.
    public var servesParent: Bool
    /// Its queued, running, needs-you and paused Studio runs, by the bridge's count
    /// (`annotate_strands`). Nil from a bridge that predates Studio runs.
    public var runsActive: Int?
    /// How many of those are waiting on the owner (`runs_needs_you`). Nil from an older bridge.
    public var runsNeedsYou: Int?
    /// Every checkbox line, each with `studio_runnable`. Nil from a bridge that predates
    /// Studio runs, which is different from a note with no lines.
    public var drafts: [StrandDraft]?
    /// The lead clause of the note's Now in plain words, ids reworded around their titles
    /// (`work::row::headline`). Nil from a bridge before 0.205.0, or a Now with nothing usable.
    public var headline: String?
    /// The first action of a `you:` Waiting on, in plain words and as written
    /// (`work::row::your_action`). Nil from a bridge before 0.205.0, or when the gate is not his.
    public var yourAction: String?
    /// Whether the bridge sent `your_action` at all, `null` included: false only from a bridge
    /// before 0.205.0, which is when the app reads the Waiting on itself.
    public var servesYourAction: Bool
    /// The first queued Drafts line's title, cleaned for the row (`work::row::next_title`). Nil
    /// from a bridge before 0.205.0, or with no queued line.
    public var nextTitle: String?

    public var id: String { slug }

    public init(slug: String, title: String = "", group: TodayProject = .unfiled,
                state: StrandState = .active, updated: String = "", repos: [String] = [],
                now: String? = nil, waiting: StrandWaiting? = nil, next: StrandNext? = nil,
                counts: StrandCounts = StrandCounts(), findings: [StrandFinding] = [],
                parent: String? = nil, servesParent: Bool = true,
                runsActive: Int? = nil, runsNeedsYou: Int? = nil,
                drafts: [StrandDraft]? = nil,
                headline: String? = nil, yourAction: String? = nil,
                servesYourAction: Bool = true, nextTitle: String? = nil) {
        self.slug = slug
        self.title = title
        self.group = group
        self.state = state
        self.updated = updated
        self.repos = repos
        self.now = now
        self.waiting = waiting
        self.next = next
        self.counts = counts
        self.findings = findings
        self.parent = parent
        self.servesParent = servesParent
        self.runsActive = runsActive
        self.runsNeedsYou = runsNeedsYou
        self.drafts = drafts
        self.headline = headline
        self.yourAction = yourAction
        self.servesYourAction = servesYourAction
        self.nextTitle = nextTitle
    }

    private enum CodingKeys: String, CodingKey {
        case slug, title, group, state, updated, repos, now, waiting, next, counts, findings,
             parent, drafts, headline
        case runsActive = "runs_active"
        case runsNeedsYou = "runs_needs_you"
        case yourAction = "your_action"
        case nextTitle = "next_title"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        slug = try c.decode(String.self, forKey: .slug)
        // A note whose frontmatter has no title is named by its file, which is what the
        // vault means by the file name anyway.
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? slug
        group = try c.decodeIfPresent(TodayProject.self, forKey: .group) ?? .unfiled
        state = try c.decodeIfPresent(StrandState.self, forKey: .state) ?? .active
        updated = try c.decodeIfPresent(String.self, forKey: .updated) ?? ""
        repos = try c.decodeIfPresent([String].self, forKey: .repos) ?? []
        now = try c.decodeIfPresent(String.self, forKey: .now)
        waiting = try c.decodeIfPresent(StrandWaiting.self, forKey: .waiting)
        next = try c.decodeIfPresent(StrandNext.self, forKey: .next)
        counts = try c.decodeIfPresent(StrandCounts.self, forKey: .counts) ?? StrandCounts()
        findings = try c.decodeIfPresent([StrandFinding].self, forKey: .findings) ?? []
        // `contains` before the decode: an absent key and a `null` both decode to nil,
        // and only the first means the bridge cannot say.
        servesParent = c.contains(.parent)
        parent = try c.decodeIfPresent(String.self, forKey: .parent)
        runsActive = try c.decodeIfPresent(Int.self, forKey: .runsActive)
        runsNeedsYou = try c.decodeIfPresent(Int.self, forKey: .runsNeedsYou)
        drafts = try c.decodeIfPresent([StrandDraft].self, forKey: .drafts)
        headline = try c.decodeIfPresent(String.self, forKey: .headline)
        servesYourAction = c.contains(.yourAction)
        yourAction = try c.decodeIfPresent(String.self, forKey: .yourAction)
        nextTitle = try c.decodeIfPresent(String.self, forKey: .nextTitle)
    }

    /// The vault path of the note itself. One function, because the list, the reader and
    /// the detail fallback all have to open the same file.
    public var notePath: String { "\(Strand.directory)/\(slug).md" }

    /// The folder every strand note lives directly inside, vault relative. Named once
    /// so the reader, the Vault scope and the path above cannot drift.
    public static let directory = "Strands"

    /// Whether the gate on this strand is one the reader can clear personally.
    public var isWaitingOnYou: Bool { waiting?.jeremy ?? false }
}

/// Everything `GET /jesse/strands` serves.
public struct StrandsSnapshot: Decodable, Equatable, Sendable {
    public var strands: [Strand]
    public var globalFindings: [StrandFinding]
    public var counts: StrandsCounts
    /// Stamped on by the endpoint at response time, which is why it is optional here —
    /// exactly as on `TodaySnapshot`.
    public var generatedAt: String?
    /// Echoed into the body by the client from the header, so a stored payload need not
    /// also keep headers.
    public var etag: String?

    public init(strands: [Strand] = [], globalFindings: [StrandFinding] = [],
                counts: StrandsCounts = StrandsCounts(),
                generatedAt: String? = nil, etag: String? = nil) {
        self.strands = strands
        self.globalFindings = globalFindings
        self.counts = counts
        self.generatedAt = generatedAt
        self.etag = etag
    }

    /// `global_findings` and `generated_at`. See the note at the top of the file.
    private enum CodingKeys: String, CodingKey {
        case strands, counts, etag
        case globalFindings = "global_findings"
        case generatedAt = "generated_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        strands = try c.decodeIfPresent([Strand].self, forKey: .strands) ?? []
        globalFindings = try c.decodeIfPresent([StrandFinding].self, forKey: .globalFindings) ?? []
        counts = try c.decodeIfPresent(StrandsCounts.self, forKey: .counts) ?? StrandsCounts()
        generatedAt = try c.decodeIfPresent(String.self, forKey: .generatedAt)
        etag = try c.decodeIfPresent(String.self, forKey: .etag)
    }

    public static func decode(from data: Data) throws -> StrandsSnapshot {
        try JSONDecoder().decode(StrandsSnapshot.self, from: data)
    }

    /// Whether this bridge serves `parent`, which is what the `Tree` lens needs. False
    /// for an empty board too, where there is no tree to draw.
    public var servesParents: Bool { strands.contains { $0.servesParent } }

    /// The strand with `slug`, or nil.
    public func strand(slug: String) -> Strand? {
        strands.first { $0.slug == slug }
    }
}

/// The board's own tallies, as the bridge counted them.
public struct StrandsCounts: Decodable, Equatable, Hashable, Sendable {
    public var active: Int
    public var waiting: Int
    public var dormant: Int

    public init(active: Int = 0, waiting: Int = 0, dormant: Int = 0) {
        self.active = active
        self.waiting = waiting
        self.dormant = dormant
    }

    private enum CodingKeys: String, CodingKey { case active, waiting, dormant }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        active = try c.decodeIfPresent(Int.self, forKey: .active) ?? 0
        waiting = try c.decodeIfPresent(Int.self, forKey: .waiting) ?? 0
        dormant = try c.decodeIfPresent(Int.self, forKey: .dormant) ?? 0
    }
}

/// One note's markdown beside its parsed form — what `GET /jesse/strands/{slug}` serves.
///
/// The markdown is what the OFF-DEVICE fallback renders: a phone with no vault folder
/// picked has no `Strands/<slug>.md` to read, and this is the same bytes it would have
/// read, fetched instead.
public struct StrandDetail: Decodable, Equatable, Sendable {
    public var markdown: String
    public var strand: Strand?

    public init(markdown: String, strand: Strand? = nil) {
        self.markdown = markdown
        self.strand = strand
    }

    private enum CodingKeys: String, CodingKey { case markdown, strand }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        markdown = try c.decodeIfPresent(String.self, forKey: .markdown) ?? ""
        strand = try c.decodeIfPresent(Strand.self, forKey: .strand)
    }

    public static func decode(from data: Data) throws -> StrandDetail {
        try JSONDecoder().decode(StrandDetail.self, from: data)
    }
}
