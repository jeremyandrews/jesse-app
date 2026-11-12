import SwiftUI
import JesseNetworking
import JesseVault

// THE STRAND BOARD: about fifteen long running pieces of work, newest first, each with
// where it stands and what runs next.
//
// It shares the Today tab rather than taking a fifth tab of its own, and the two
// segments answer genuinely different questions. The day answers "what am I doing
// today"; a strand answers "where does this work stand and what runs next". Those are
// the vault's own two objects, and a fifth tab would have made a board that is read
// once or twice a day as prominent as the one that is read twenty times.
//
// ## One snapshot, no per-row call
//
// Every row here is drawn from the single `GET /jesse/strands` the model holds. A row
// makes no request of its own — not for its counts, not for its findings, not for its
// next step — so the board costs one conditional round trip whether it holds two
// strands or forty.
//
// ## What a tap opens, and why it is the vault reader
//
// A tap hands the strand to the shell's `StrandOpener`, the one definition of opening a
// strand, which the Today rows and the item detail use too; the shell attaches the one
// sheet it presents (`strandNoteSheet`). A strand note is a NOTE: its `## Queue` is a list of checkboxes, and ticking one is
// how the next step becomes the last step. The device already has a reader that ticks a
// box through a stamped, guarded write (`VaultNoteReaderView`), so a tap opens that and
// nothing new is built. The bridge's markdown is the fallback for a device with no
// vault folder, and it is READ ONLY on purpose: a copy fetched over the network has no
// stamp to guard a write with, and a second write path to the same lines is how two
// spellings of "tick a box" end up disagreeing.

// ## Work (bridge 0.203.0)
//
// The segment reads **Work**. A row is two lines (HO5, `WorkSemantics.rowText`): the project's
// title with what it is about right now beside it, then what is his to do, else Next. A tap pushes the
// project's short view (`WorkProjectView`); the full note, with its Drafts block and Run on
// Studio, is one tap further, behind Full note. A board with no runs model (a preview, a
// shell without runs) still opens the note directly, as before.

/// The Work segment (internally still the strand board).
public struct StrandsListView: View {
    @Bindable private var model: StrandsModel

    /// The shell's opener: the same one a Today row's strand chip and the item detail
    /// use, so the board and the chip can never open a strand two different ways.
    @Bindable private var opener: StrandOpener

    /// How many open Today items name a strand, by slug. The count behind a row's
    /// `N on Today` caption, filtered from the day the app already holds.
    private let onTodayCount: (String) -> Int

    /// The runs, for each row's status line, the `Since your last look` line and the project
    /// view. Nil in a shell or preview without them: rows then open the note, as before.
    private let runs: StudioRunsModel?

    /// The project pushed from a row.
    @State private var openedProject: WorkProjectRoute?

    /// The strand whose findings sheet is up.
    @State private var findingsFor: Strand?
    /// Which project groups are collapsed. Only `Dormant` starts that way.
    @State private var collapsed: Set<String> = ["dormant"]
    /// Which `Tree` parents are collapsed, by slug, remembered on this device. Every
    /// parent starts expanded, so a new strand's subtree is never hidden by default.
    @AppStorage(StrandsListView.collapsedTreeKey) private var collapsedTreeStored = ""

    /// The stored key for the collapsed `Tree` parents.
    public static let collapsedTreeKey = "strands.tree.collapsed"

    /// The shell's two strand actions, read here and handed to every row's menu. Nil in a
    /// preview, in which case those entries are listed and inert; the shells inject both.
    @Environment(\.strandDiscuss) private var discussAction
    @Environment(\.strandRecord) private var recordAction
    /// Which appearance the board is drawn in, because a strand's tone is resolved per
    /// appearance before it reaches a row.
    @Environment(\.colorScheme) private var scheme

    public init(model: StrandsModel,
                opener: StrandOpener,
                onTodayCount: @escaping (String) -> Int = { _ in 0 },
                runs: StudioRunsModel? = nil,
                // For a render of the whole board (the Mac screenshot test): Dormant open.
                dormantStartsOpen: Bool = false) {
        self.model = model
        self.opener = opener
        self.onTodayCount = onTodayCount
        self.runs = runs
        self._collapsed = State(initialValue: dormantStartsOpen ? [] : ["dormant"])
    }

    public var body: some View {
        Group {
            switch model.displayState {
            case .loading:
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            case .empty:
                TodayEmptyState(symbol: "point.3.connected.trianglepath.dotted",
                                title: Self.emptyTitle,
                                message: Self.emptyMessage)
            case .unavailable(let message):
                TodayEmptyState(symbol: "wifi.exclamationmark",
                                title: "Can't reach the bridge",
                                message: message)
            case .offline:
                TodayEmptyState(
                    symbol: "wifi.slash",
                    title: "You're offline",
                    message: "This device hasn't loaded the strands yet, so there's nothing to show. They'll be here the next time the bridge is reachable.")
            case .content(let groups):
                list(groups)
            }
        }
        .task {
            // The last board this device was given, drawn BEFORE the fetch. On a
            // reachable launch the conditional GET it primes the ETag for answers
            // `304` and nothing redraws; on an unreachable one it is the whole feature.
            model.primeFromCache()
            await model.load()
            if let runs {
                await runs.loadLastLook()
                await runs.load()
            }
        }
        // Opening Work posts the last look, but only once this visit's rows are on screen,
        // so the visit itself still shows what was new.
        .task(id: isReadyToMarkLastLook) {
            guard isReadyToMarkLastLook, let runs else { return }
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            await runs.markLastLook()
        }
        .refreshable {
            await model.refresh()
            await runs?.load()
        }
        .sheet(item: $findingsFor) { strand in
            StrandFindingsSheet(strand: strand) { findingsFor = nil }
        }
        .navigationDestination(item: $openedProject) { route in
            if let runs {
                WorkProjectView(slug: route.slug, strands: model, runs: runs, opener: opener)
            }
        }
    }

    /// The board has drawn its rows and the runs and last look they compare against.
    private var isReadyToMarkLastLook: Bool {
        guard let runs, case .content = model.displayState else { return false }
        return runs.hasLoaded && runs.lastLookLoaded
    }

    /// The empty board, in two sentences that say it is a real answer rather than a
    /// failure: the bridge replied, and it had nothing to list.
    static let emptyTitle = "No strands yet."
    static let emptyMessage =
        "A strand is one long running piece of work, with a status note under Strands in the vault. None are active right now."

    // MARK: - The list

    @ViewBuilder
    private func list(_ groups: [StrandsGroup]) -> some View {
        // Every tone on the board, from the WHOLE snapshot, once per redraw: a tone never
        // depends on the lens, on what is collapsed, or on which rows are on screen.
        let family = StrandFamily(strands: model.snapshot?.strands ?? [], scheme: scheme)
        List {
            if let notice = opener.notice {
                TodayNoticeRow(message: notice) { opener.notice = nil }
                    .listRowSeparator(.hidden)
            }
            if model.isReadOnly {
                TodayStatusBanner(isOffline: true,
                                  isPendingReplay: false,
                                  message: model.lastErrorMessage,
                                  staleness: model.stalenessLine)
                    .listRowSeparator(.hidden)
            }
            if let runs {
                let since = runs.finishedSinceLastLook()
                if !since.isEmpty {
                    Section {
                        WorkSinceBlock(runs: since)
                    }
                }
            }
            ForEach(groups) { group in
                section(group, family: family)
            }
        }
        .listStyle(.plain)
    }

    @ViewBuilder
    private func section(_ group: StrandsGroup, family: StrandFamily) -> some View {
        if let title = group.title {
            Section {
                if !collapsed.contains(group.id) {
                    ForEach(group.strands) { row($0, family: family, showsParent: true) }
                }
            } header: {
                StrandsGroupHeader(title: title,
                                   count: group.strands.count,
                                   isCollapsible: group.isDormant,
                                   isCollapsed: collapsed.contains(group.id)) {
                    withAnimation(.snappy) { toggleCollapsed(group.id) }
                }
            }
        } else if let treeRows = group.treeRows {
            let folded = StrandsSemantics.decodeCollapsed(collapsedTreeStored)
            Section {
                ForEach(StrandsSemantics.visibleTreeRows(treeRows, collapsed: folded)) {
                    treeRow($0, isCollapsed: folded.contains($0.strand.slug), family: family)
                }
            }
        } else {
            Section {
                ForEach(group.strands) { row($0, family: family, showsParent: true) }
            }
        }
    }

    /// A `Tree` row: indented by depth, with a disclosure control of its own OUTSIDE the
    /// row's combined accessibility element, so the chevron stays a separate control and
    /// the row's tap still opens the note.
    ///
    /// The indent is BLANK SPACE, and exactly one vertical mark is drawn on the row: its
    /// own accent bar, in its own tone. It used to draw a rail per ancestor level as well,
    /// centred under that ancestor's chevron — indent guides, in the manner of a code
    /// editor. On a phone that read as a mistake rather than as a relation: the rail sits
    /// in the disclosure column, LEFT of the parent's own bar and never joined to it, so
    /// every nested row showed two parallel lines a few points apart and the outer one
    /// looked like a stray. Depth is already carried by the indent, the chevron and the
    /// row's own tone, and a screen reader hears it as `in <parent>` in the row's label.
    private func treeRow(_ tree: StrandsTreeRow, isCollapsed: Bool,
                         family: StrandFamily) -> some View {
        HStack(alignment: .top, spacing: 2) {
            if tree.depth > 0 {
                Color.clear
                    .frame(width: CGFloat(tree.depth) * Self.treeIndent)
                    .accessibilityHidden(true)
            }
            Group {
                if tree.hasChildren {
                    Button {
                        withAnimation(.snappy) { toggleTree(tree.strand.slug) }
                    } label: {
                        Image(systemName: "chevron.down")
                            .font(.caption2)
                            .rotationEffect(.degrees(isCollapsed ? -90 : 0))
                            .frame(width: 20, height: 24)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(tree.strand.title)
                    .accessibilityHint(isCollapsed ? "Expand" : "Collapse")
                } else {
                    Color.clear.frame(width: 20, height: 24)
                }
            }
            .padding(.vertical, Self.treeRowPadding)
            row(tree.strand, family: family, showsParent: false,
                insideCaption: tree.hasChildren && isCollapsed
                    ? StrandsSemantics.insideCaption(tree.descendants) : nil)
                .padding(.vertical, Self.treeRowPadding)
        }
        // The row keeps its own vertical inset (the padding above stands in for the
        // list's) so the indent columns of consecutive rows line up exactly.
        .fixedSize(horizontal: false, vertical: true)
        .listRowInsets(.vertical, 0)
    }

    /// The vertical space a `Tree` row keeps inside itself, in place of the list's inset.
    static let treeRowPadding: CGFloat = 5

    /// One level of the tree, in points.
    static let treeIndent: CGFloat = 16

    private func toggleTree(_ slug: String) {
        var folded = StrandsSemantics.decodeCollapsed(collapsedTreeStored)
        if folded.contains(slug) { folded.remove(slug) } else { folded.insert(slug) }
        collapsedTreeStored = StrandsSemantics.encodeCollapsed(folded)
    }

    /// One board row. `showsParent` is the flat lenses' caption naming the strand this one
    /// sits under; the `Tree` lens draws that relation as position instead.
    private func row(_ strand: Strand, family: StrandFamily, showsParent: Bool,
                     insideCaption: String? = nil) -> some View {
        let parent = family.parent(of: strand)
        let all = runs?.runs ?? []
        let loaded = runs?.hasLoaded ?? false
        let text = WorkSemantics.rowText(strand, runs: all, runsLoaded: loaded)
        let status = WorkSemantics.rowStatus(strand, runs: all, runsLoaded: loaded)
        return StrandRow(strand: strand,
                  tone: family.tone(strand.slug),
                  parentTitle: parent?.title,
                  showsParentCaption: showsParent,
                  parentTone: parent.map { family.tone($0.slug) },
                  referenceDay: model.referenceDay,
                  onTodayCaption: StrandOnToday.caption(count: onTodayCount(strand.slug)),
                  insideCaption: insideCaption,
                  text: text,
                  spokenStatus: status,
                  isOpening: opener.opening == strand.slug,
                  onOpen: {
                      if runs != nil {
                          openedProject = WorkProjectRoute(slug: strand.slug)
                      } else {
                          opener.open(slug: strand.slug, title: strand.title)
                      }
                  },
                  onShowFindings: { findingsFor = strand })
            // A long press (a secondary click on the Mac) on ANY board row — live, dormant
            // or nested — offers the same five things the Vault tab's Strands rows offer.
            // Attached here rather than inside `StrandRow` so every group gets it from the
            // one place every group is built, and so the row keeps its tap unchanged.
            .strandContextMenu(Self.menu(for: strand, opener: opener,
                                         discuss: discussAction, record: recordAction))
    }

    /// **The menu one board row carries.** Static and handed everything it needs, so the
    /// test that compares this surface's menu with the Vault tab's can build the same value
    /// the row renders without a screen.
    ///
    /// `Open note` is the opener the row's own tap uses, not a second way in. `Search` and
    /// `Decisions` go through the shell, because the record lives in another tab.
    static func menu(for strand: Strand, opener: StrandOpener,
                     discuss: StrandDiscussAction?,
                     record: StrandRecordAction?) -> StrandMenu {
        // `notePath` is the wire model's own spelling of `Strands/<slug>.md`, so the
        // convention lives in one place rather than being re-derived here.
        let target = StrandMenuTarget(slug: strand.slug, title: strand.title,
                                      path: strand.notePath)
        return StrandMenu(
            target: target,
            onOpenNote: { opener.open(slug: strand.slug, title: strand.title) },
            onDiscuss: discuss.map { action in { action.start(target) } },
            onShowRecord: { section in record?.show(strand.slug, section: section) })
    }

    private func toggleCollapsed(_ id: String) {
        if collapsed.contains(id) { collapsed.remove(id) } else { collapsed.insert(id) }
    }
}

// MARK: - Where the open note came from

/// The two places a strand note can be read from.
///
/// A sum type rather than a path plus a flag: the two cases carry different payloads
/// (one a path this device can write to, one a copy of the bytes it cannot) and the
/// difference is exactly what decides whether the reader offers a checkbox.
public enum StrandNoteSource: Equatable, Identifiable, Sendable {
    /// The note in the Obsidian copy on this device, by vault relative path. The slug
    /// rides along because the sheet's `On Today` block filters by it.
    case local(path: String, slug: String, title: String)
    /// The bridge's copy, read only.
    case remote(slug: String, title: String, markdown: String)

    public var id: String {
        switch self {
        case .local(let path, _, _): return "local:\(path)"
        case .remote(let slug, _, _): return "remote:\(slug)"
        }
    }
}

// MARK: - The row

/// One project on the Work board: its group's colour bar and at most two lines
/// (`WorkSemantics.rowText`, docs/ho5-work-board-rows.md). Line 1 is the title in bold with what
/// the project is about right now beside it in grey; line 2 is what is his to do in orange, else
/// Next. No audit triangle and no age: the findings are in the full note's sheet.
struct StrandRow: View {
    let strand: Strand
    /// The strand's family tone, resolved for the current appearance by `StrandTone`:
    /// the one function every surface that draws a strand goes through.
    let tone: TodayProjectColor
    /// The title of the strand this one sits under, or nil for a top level strand. Read
    /// aloud, not drawn: the row has one status line and nothing else.
    var parentTitle: String? = nil
    var showsParentCaption = false
    var parentTone: TodayProjectColor? = nil
    /// The day every `updated` stamp is measured against.
    let referenceDay: String
    var onTodayCaption: String? = nil
    /// `N inside`, on a collapsed `Tree` parent only: read aloud.
    var insideCaption: String? = nil
    var runBadge: StudioRunBadge? = nil
    /// The words beside the title and under it, or nil for nothing to say.
    var text: WorkRowText? = nil
    /// `Your turn` or `2 moving` (`WorkSemantics.rowStatus`; its `Next: …` is not said, line 2
    /// says it): said first to a screen reader, so a row's state is heard before its words.
    var spokenStatus: WorkRowStatus? = nil
    let isOpening: Bool
    let onOpen: () -> Void
    let onShowFindings: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            // The day rows' accent bar, from the same palette, in the strand's family
            // tone: its ROOT STRAND's colour, one step per level below it. The tone is
            // computed in `StrandTone`, inside the palette's contract, never here, and
            // it is the only vertical mark on the row.
            TodayProjectAccentBar(strandTone: tone)
            Button(action: onOpen) {
                // The widest arrangement that fits on one line beside the title: the grey text
                // cut on a word (never under four), else just the word for what is moving, else
                // the title alone, with the grey text moved under it when nothing is his to do.
                ViewThatFits(in: .horizontal) {
                    ForEach(Array(detailVariants.enumerated()), id: \.offset) { _, variant in
                        rowBody(detail: variant, line: text?.line, tone: text?.tone ?? .plain)
                    }
                    if let marker = text?.marker {
                        // Only the word fits: when nothing is his, line 2 says what is moving.
                        rowBody(detail: marker, line: fallbackLine, tone: text?.tone ?? .plain)
                    }
                    rowBody(detail: nil, line: fallbackLine, tone: text?.tone ?? .plain,
                            fitsTitle: true)
                }
                // A wide Mac window does not stretch a line past what reads at a glance.
                .frame(maxWidth: Self.textMaxWidth, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityHint("Opens the project")
    }

    /// The grey text beside the title, whole and then cut shorter on a word, never under four.
    private var detailVariants: [String] {
        guard let detail = text?.detail else { return [] }
        return WordFitLine.variants(detail, minWords: 4)
    }

    /// Line 2 when the grey text does not fit beside the title (`WorkRowText.fallbackLine`).
    private var fallbackLine: String? { text?.fallbackLine }

    private func rowBody(detail: String?, line: String?, tone: WorkRowText.Tone,
                         fitsTitle: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(strand.title)
                    .font(.body.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .fixedSize(horizontal: !fitsTitle, vertical: false)
                if let detail {
                    Text(detail)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                }
                if isOpening {
                    ProgressView()
                        .controlSize(.mini)
                }
            }
            if let line {
                WordFitLine(text: line)
                    .font(.subheadline)
                    .foregroundStyle(tone == .you ? AnyShapeStyle(.orange)
                                                  : AnyShapeStyle(.secondary))
                    // Line 2 takes whatever width there is; only line 1 decides the fit.
                    .frame(minWidth: 0, idealWidth: 0, maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// The widest a row's words run, in points: wider than any phone, narrower than a Mac window.
    static let textMaxWidth: CGFloat = 560

    /// One sentence for a screen reader: the title, its state, the grey text and line 2 whole
    /// (never cut), and where it sits.
    var accessibilityLabel: String {
        var parts = [strand.title]
        switch spokenStatus {
        case .yourTurn, .moving: parts.append(spokenStatus?.text ?? "")
        case .next, nil: break
        }
        if let detail = text?.detail { parts.append(detail) }
        if let line = text?.line { parts.append(line) }
        if let parentTitle { parts.append(StrandsSemantics.parentCaption(parentTitle)) }
        if let insideCaption { parts.append(insideCaption) }
        return parts.joined(separator: ", ")
    }
}

// MARK: - One line, cut on a word

/// One line of text that never ends mid word: the whole text when it fits, else the longest
/// run of its first words that fits, with `…`. The last resort (one word that does not fit)
/// truncates as any line does.
struct WordFitLine: View {
    let text: String

    var body: some View {
        let variants = Self.variants(text)
        ViewThatFits(in: .horizontal) {
            ForEach(Array(variants.enumerated()), id: \.offset) { _, v in
                Text(v).lineLimit(1).fixedSize(horizontal: true, vertical: false)
            }
            Text(variants.last ?? text).lineLimit(1)
        }
        .accessibilityLabel(text)
    }

    /// The text, then each shorter run of its first words with `…`, longest first, none under
    /// `minWords` words (the grey text beside a title asks for four: fewer is a stub). Trailing
    /// punctuation and a dangling small word are dropped before the `…`.
    static func variants(_ text: String, minWords: Int = 1) -> [String] {
        let words = text.split(separator: " ", omittingEmptySubsequences: true).map(String.init)
        guard words.count > 1 else { return [text] }
        var out = [text]
        var k = words.count - 1
        while k >= max(1, minWords) {
            let joined = trimTail(dropDangling(Array(words.prefix(k))).joined(separator: " "))
            if joined.split(separator: " ").count < minWords { break }
            var candidate = joined + "\u{2026}"
            // A quotation cut short still closes.
            if candidate.contains("\u{201C}") && !candidate.contains("\u{201D}") {
                candidate += "\u{201D}"
            }
            if out.last != candidate { out.append(candidate) }
            k -= 1
        }
        return out
    }

    /// `words` without the small words it ends on (one word always stays).
    static func dropDangling(_ words: [String]) -> [String] {
        var out = words
        while out.count > 1, let last = out.last,
              dangling.contains(last.lowercased().trimmingCharacters(in: .punctuationCharacters)) {
            out.removeLast()
        }
        return out
    }

    /// `text` without the punctuation and opening marks a cut can leave at its end.
    static func trimTail(_ text: String) -> String {
        var out = text
        while let last = out.last, ",;:.-\u{2013}\u{2014}\u{201C}( ".contains(last) {
            out.removeLast()
        }
        return out
    }

    /// Small words a cut line should not end on. The bridge's `is_filler`
    /// (`bridge/src/hostrun/work/row.rs`) holds the same list; keep the two together.
    static let dangling: Set<String> = [
        "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "its", "of", "on", "or",
        "per", "the", "their", "to", "with",
    ]
}

// MARK: - A group heading

/// A project heading, or the collapsible `Dormant` one.
struct StrandsGroupHeader: View {
    let title: String
    let count: Int
    let isCollapsible: Bool
    let isCollapsed: Bool
    let onToggle: () -> Void

    var body: some View {
        HStack {
            if isCollapsible {
                Button(action: onToggle) {
                    HStack(spacing: 6) {
                        Image(systemName: "chevron.down")
                            .font(.caption2)
                            .rotationEffect(.degrees(isCollapsed ? -90 : 0))
                        Text(title)
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(title)
                .accessibilityHint(isCollapsed ? "Expand" : "Collapse")
            } else {
                Text(title)
            }
            Spacer()
            Text("\(count)")
                .font(.caption)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - The findings sheet

/// What the nightly audit said about one note, one line each.
///
/// A sheet rather than rows in the list: a finding is about the note's bookkeeping
/// rather than about the work, and fifteen strands with two findings each would bury
/// the board in its own maintenance.
struct StrandFindingsSheet: View {
    let strand: Strand
    let onDone: () -> Void

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(strand.findings) { finding in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(finding.message)
                                .font(.subheadline)
                                .fixedSize(horizontal: false, vertical: true)
                            HStack(spacing: 6) {
                                Text(finding.code)
                                    .font(.caption2)
                                    .monospaced()
                                if let line = finding.line {
                                    Text("line \(line)")
                                        .font(.caption2)
                                        .monospacedDigit()
                                }
                            }
                            .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 2)
                    }
                } header: {
                    Text(strand.findings.count == 1 ? "1 finding" : "\(strand.findings.count) findings")
                } footer: {
                    Text("Found by the nightly audit of the Strands notes. Each one names something in the note that is no longer true.")
                }
            }
            .navigationTitle(strand.title)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: onDone)
                }
            }
        }
    }
}

// MARK: - The off-device reader

/// The bridge's copy of a note, read only.
///
/// Rendered through `TodayNoteView`, which is the same block model and the same link
/// chips the item detail sheet uses, so a note read over the network and a note read
/// off disk are not two different looking screens. It offers no checkbox: these bytes
/// came with no stamp, and a guarded write needs one.
struct StrandRemoteNoteView: View {
    let title: String
    let markdown: String
    /// The `On Today` block, above the provenance line.
    var onToday: AnyView? = nil
    let onOpenLink: (TodayLinkOrigin) -> Void
    let onDone: () -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if let onToday {
                        onToday
                    }
                    Label(Self.provenance, systemImage: "antenna.radiowaves.left.and.right")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    TodayNoteView(markdown: markdown, onOpenLink: onOpenLink)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: onDone)
                }
            }
        }
    }

    static let provenance =
        "Read from the bridge, because this device has no copy of the vault holding this note. Point Jesse at the Obsidian folder in Settings to tick its boxes here."
}

// MARK: - The sort control

/// The board's lens, as a menu. Up to three entries, and none writes anything.
public struct StrandsSortMenu: View {
    @Binding private var selection: StrandsSortKey
    /// The lenses this board offers. `Tree` is absent from an older bridge's board.
    private let available: [StrandsSortKey]

    public init(selection: Binding<StrandsSortKey>,
                available: [StrandsSortKey] = StrandsSortKey.allCases) {
        self._selection = selection
        self.available = available
    }

    public var body: some View {
        Menu {
            Picker("Order", selection: $selection) {
                ForEach(available) { key in
                    Label(key.label, systemImage: key.symbol).tag(key)
                }
            }
            .pickerStyle(.inline)
        } label: {
            Image(systemName: selection != .mostRecent
                  ? "line.3.horizontal.decrease.circle.fill"
                  : "line.3.horizontal.decrease.circle")
        }
        .menuIndicator(.hidden)
        .accessibilityLabel("Order the strands: \(selection.label)")
    }
}
