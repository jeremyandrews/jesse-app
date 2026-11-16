import SwiftUI
import JesseNetworking

// THE WORK VIEW: the board's `Since your last look` line and a project's short view. Shared
// by the iPhone and the Mac. Every decision is `WorkSemantics`'; these only draw it.
//
// Words: no machine name, no run id, no branch and no step id on anything here. Those live
// behind a run's Details and in the full note.

/// A project pushed from the Work board.
public struct WorkProjectRoute: Hashable, Identifiable, Sendable {
    public let slug: String
    public var id: String { slug }
    public init(slug: String) { self.slug = slug }
}

// MARK: - Since your last look

private struct WorkSinceStartsExpandedKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// Draw `Since your last look` already expanded: for an off screen render, which cannot
    /// tap. False everywhere a person is looking.
    public var workSinceStartsExpanded: Bool {
        get { self[WorkSinceStartsExpandedKey.self] }
        set { self[WorkSinceStartsExpandedKey.self] = newValue }
    }
}

/// `3 finished since your last look`, which expands to one row per run: its title and the
/// first line of its report, verbatim. A row opens the report. Nothing at all for none.
struct WorkSinceBlock: View {
    let runs: [StudioRun]
    @State private var expanded = false
    @Environment(\.studioRunReport) private var reportAction
    @Environment(\.workSinceStartsExpanded) private var startsExpanded

    var body: some View {
        if let line = WorkSemantics.sinceLine(count: runs.count) {
            header(line)
                .onAppear { if startsExpanded { expanded = true } }
            if expanded { rows }
        }
    }

    private func header(_ line: String) -> some View {
        Button {
            withAnimation(.snappy) { expanded.toggle() }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .rotationEffect(.degrees(expanded ? 90 : 0))
                Text(line)
                    .font(.subheadline.weight(.medium))
                Spacer(minLength: 0)
            }
            .foregroundStyle(.secondary)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityHint(expanded ? "Collapse" : "Shows what finished")
    }

    private var rows: some View {
        ForEach(runs, id: \.sectionKey.since) { run in
            Button {
                if let path = run.latestReportPath { reportAction?.open(path) }
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text(WorkSemantics.title(run))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                    if let line = run.reportLine, !line.isEmpty {
                        Text(line)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                            .multilineTextAlignment(.leading)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(run.latestReportPath == nil || reportAction == nil)
            .accessibilityHint("Opens the report")
        }
    }
}

// MARK: - A project's view

/// One project, short: what finished, what needs you, what is moving, what is next, and the
/// way to the full note.
public struct WorkProjectView: View {
    let slug: String
    @Bindable var strands: StrandsModel
    @Bindable var runs: StudioRunsModel
    let opener: StrandOpener?

    @Environment(\.studioRunReport) private var reportAction
    @State private var showsAllRuns = false
    @State private var openedRun: StudioRunRoute?

    public init(slug: String, strands: StrandsModel, runs: StudioRunsModel,
                opener: StrandOpener?) {
        self.slug = slug
        self.strands = strands
        self.runs = runs
        self.opener = opener
    }

    private var strand: Strand? { strands.snapshot?.strand(slug: slug) }

    public var body: some View {
        List {
            if let strand {
                content(strand)
            } else {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .navigationTitle(strand?.title ?? slug)
        .refreshable {
            await runs.load()
            await strands.refresh()
        }
        // Moving refreshes every 15 seconds while this is on screen, and never otherwise.
        .task {
            while !Task.isCancelled {
                await runs.load()
                try? await Task.sleep(for: .seconds(15))
            }
        }
        .navigationDestination(for: StudioRunRoute.self) { route in
            StudioRunDetailView(model: runs.detailModel(for: route.runID))
        }
        .navigationDestination(item: $openedRun) { route in
            StudioRunDetailView(model: runs.detailModel(for: route.runID))
        }
        .sheet(isPresented: $showsAllRuns) {
            StudioRunsScreen(model: runs, strand: slug) { showsAllRuns = false }
        }
        .studioRunAlert($runs.alert)
    }

    @ViewBuilder
    private func content(_ strand: Strand) -> some View {
        let since = runs.finishedSinceLastLook(strand: slug)
        if !since.isEmpty {
            Section("Since your last look") {
                WorkSinceBlock(runs: since)
            }
        }

        let turn = WorkSemantics.yourTurnRuns(slug, in: runs.runs)
        let gate = WorkSemantics.youGate(strand)
        // The snapshot counts a question the runs list does not hold yet: the row says so, and so
        // does this section.
        let unlisted = turn.isEmpty && (strand.runsNeedsYou ?? 0) > 0
        if !turn.isEmpty || gate != nil || unlisted {
            Section {
                if unlisted {
                    Text(WorkSemantics.answerAQuestion)
                        .font(.subheadline.weight(.semibold))
                }
                ForEach(turn, id: \.sectionKey.turn) { run in
                    WorkYourTurnRow(run: run, detail: runs.detailModel(for: run.runID),
                                    onOpen: { openedRun = StudioRunRoute(runID: run.runID) },
                                    onSent: { Task { await runs.load() } })
                }
                if let gate {
                    // The row's own words first, so the board and this view never disagree about
                    // what is his to do; the note's whole Waiting on under them.
                    let phrase = WorkSemantics.gatePhrase(strand)
                    VStack(alignment: .leading, spacing: 3) {
                        if let line = WorkSemantics.gateLine(strand) {
                            Text(line)
                                .font(.subheadline.weight(.semibold))
                        }
                        if phrase?.lowercased() != gate.lowercased() {
                            Text(gate)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            } header: {
                Text("Your turn").foregroundStyle(.orange)
            }
        }

        let moving = WorkSemantics.moving(slug, in: runs.runs)
        Section("Moving") {
            if moving.isEmpty {
                Text("Nothing is moving.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            ForEach(moving, id: \.sectionKey.moving) { run in
                #if os(macOS)
                // A button onto `openedRun`, as the Your turn rows open theirs. On the Mac a
                // value link does not push from here: this view was itself pushed by an
                // item binding (`StrandsListView`), and a value link on top of that
                // presentation never lands, wherever its destination is declared. The row
                // drew no chevron and a click did nothing.
                Button {
                    openedRun = StudioRunRoute(runID: run.runID)
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 4) {
                        movingLabel(run)
                        Spacer(minLength: 4)
                        Image(systemName: "chevron.right")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityHint("Opens the run")
                #else
                NavigationLink(value: StudioRunRoute(runID: run.runID)) {
                    movingLabel(run)
                }
                #endif
            }
            Button {
                showsAllRuns = true
            } label: {
                Label("All runs", systemImage: "list.bullet")
                    .font(.subheadline)
            }
            .accessibilityHint("Shows every run of this project")
        }

        let next = WorkSemantics.nextLines(strand)
        if !next.isEmpty {
            Section("Next") {
                // Keyed per section: a step id equals its run's id, and one List holding two rows
                // with the same id draws one of them twice (seen on macOS).
                ForEach(Array(next.enumerated()), id: \.element.nextKey) { index, draft in
                    nextRow(draft, index: index)
                }
            }
        }

        if let opener {
            Section {
                Button {
                    opener.open(slug: strand.slug, title: strand.title)
                } label: {
                    HStack {
                        Label("Full note", systemImage: "doc.text")
                        if opener.opening == strand.slug {
                            Spacer()
                            ProgressView().controlSize(.small)
                        }
                    }
                }
                .accessibilityHint("Opens the project's whole note")
            }
        }
    }

    private func movingLabel(_ run: StudioRun) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(WorkSemantics.title(run))
                .font(.subheadline.weight(.semibold))
                .lineLimit(2)
            if let line = run.lastLine, !line.isEmpty {
                Text(line)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
    }

    @ViewBuilder
    private func nextRow(_ draft: StrandDraft, index: Int) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(draft.displayTitle)
                    .font(.subheadline)
                    .lineLimit(2)
                Spacer(minLength: 4)
                if WorkSemantics.offersStart(draft, at: index) {
                    if runs.starting == draft.id {
                        ProgressView().controlSize(.small)
                    } else {
                        Button("Start") {
                            Task { await start(draft) }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                    }
                }
            }
            if let note = WorkSemantics.startNote(draft, at: index) {
                Text(note)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// The same start `Run on Studio` posts; the run then shows under Moving.
    private func start(_ draft: StrandDraft) async {
        guard await runs.start(strand: slug, itemID: draft.id) != nil else { return }
        await strands.refresh()
    }
}

/// One run that waits on the owner: a question with its answer field, a cut off run with
/// Resume, or a failure with See why.
struct WorkYourTurnRow: View {
    let run: StudioRun
    @Bindable var detail: StudioRunDetailModel
    let onOpen: () -> Void
    let onSent: () -> Void
    @Environment(\.studioRunReport) private var reportAction

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // A plain button, not a link: a link would make the whole row, answer field
            // included, one tap target.
            Button(action: onOpen) {
                HStack(spacing: 4) {
                    Text(WorkSemantics.title(run))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    Image(systemName: "chevron.right")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens the run")
            switch run.status {
            case .needsYou:
                if let question = WorkSemantics.question(run) {
                    Text(question)
                        .font(.subheadline)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack(alignment: .bottom, spacing: 8) {
                    TextField("Your answer", text: $detail.composer, axis: .vertical)
                        .lineLimit(1...5)
                        .textFieldStyle(.roundedBorder)
                    Button("Send") {
                        Task {
                            if await detail.send() { onSent() }
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .fixedSize()
                    .disabled(!detail.canSend)
                }
            case .interrupted:
                HStack {
                    Text("Cut off before it finished.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Resume") {
                        Task {
                            await detail.resume()
                            onSent()
                        }
                    }
                    .buttonStyle(.bordered)
                    .disabled(detail.isSending)
                    .accessibilityHint("Sends \u{201C}\(StudioRunSemantics.resumeText)\u{201D}")
                }
            case .failed:
                HStack {
                    Text("Failed.")
                        .font(.caption)
                        .foregroundStyle(.red)
                    Spacer()
                    Button("See why") {
                        if let path = run.latestReportPath { reportAction?.open(path) }
                    }
                    .buttonStyle(.bordered)
                    .disabled(run.latestReportPath == nil || reportAction == nil)
                }
            default:
                EmptyView()
            }
        }
        .padding(.vertical, 2)
        // The list's copy is the newest; the composer's model follows it.
        .onAppear { detail.adopt(run) }
        .onChange(of: run) { _, newer in detail.adopt(newer) }
        .studioRunAlert($detail.alert)
    }
}

/// A run's row identity per section of the project view, so the same run (or a step whose id
/// is the run's) never shares an identity across two sections of one List.
struct WorkSectionKey {
    let runID: String
    var turn: String { "turn-\(runID)" }
    var moving: String { "moving-\(runID)" }
    var since: String { "since-\(runID)" }
}

extension StudioRun {
    var sectionKey: WorkSectionKey { WorkSectionKey(runID: runID) }
}

extension StrandDraft {
    /// A Next row's identity: `next-<step id>`, never the bare id a run of the step also has.
    var nextKey: String { "next-\(id)" }
}
