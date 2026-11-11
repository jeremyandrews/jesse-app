import SwiftUI
import JesseNetworking

// The Studio run screens: the Runs list, a run's detail, a strand's Drafts lines with
// `Run on Studio`, and the board's badge. Shared by the iPhone and the Mac; the shells supply
// only where a report opens (`StudioRunReportAction`) and where the Runs sheet hangs.
//
// No chat anywhere in here. A start, a reply and a cancel each go straight to their route,
// and a refusal comes back as the bridge's own sentence in an alert.
//
// Polling is scoped to what is on screen: the list every 15 seconds while it is showing, a
// detail every 5 seconds while its run can still change by itself. Both are `.task` loops,
// so they end the moment the view goes.

// MARK: - The shell's report action

/// Open a run's Inbox report in the Vault tab. Only the shell can switch tabs and close the
/// sheets these screens sit in, so it is injected, like `StrandRecordAction`.
public struct StudioRunReportAction {
    private let handler: @MainActor (String) -> Void

    public init(open: @escaping @MainActor (String) -> Void) {
        self.handler = open
    }

    @MainActor public func open(_ path: String) { handler(path) }
}

private struct StudioRunReportKey: EnvironmentKey {
    // `nonisolated(unsafe)` on a nil default, as `StrandRecordActionKey`: the value holds a
    // MainActor closure and so is not Sendable, and nil is never mutated.
    nonisolated(unsafe) static let defaultValue: StudioRunReportAction? = nil
}

extension EnvironmentValues {
    public var studioRunReport: StudioRunReportAction? {
        get { self[StudioRunReportKey.self] }
        set { self[StudioRunReportKey.self] = newValue }
    }
}

// MARK: - Colour and status

extension StudioRunTint {
    public var color: Color {
        switch self {
        case .neutral: return .gray
        case .blue: return .blue
        case .orange: return .orange
        case .green: return .green
        case .red: return .red
        case .yellow: return .yellow
        case .purple: return .purple
        case .secondary: return .secondary
        }
    }
}

/// A status as an icon and a word, in its colour.
public struct StudioRunStatusLabel: View {
    let status: StudioRunStatus
    var font: Font = .caption

    public init(status: StudioRunStatus, font: Font = .caption) {
        self.status = status
        self.font = font
    }

    public var body: some View {
        Label(StudioRunSemantics.label(status), systemImage: StudioRunSemantics.symbol(status))
            .font(font)
            .foregroundStyle(StudioRunSemantics.tint(status).color)
            .labelStyle(.titleAndIcon)
    }
}

// MARK: - The board's badge

/// The count of a strand's active runs, in the needs-you colour when one waits on its owner.
struct StudioRunBadgeView: View {
    let badge: StudioRunBadge

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: "desktopcomputer")
            Text(badge.text).monospacedDigit()
        }
        .font(.caption2.weight(.semibold))
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .foregroundStyle(badge.needsYou ? Color.white : Color.accentColor)
        .background(badge.needsYou ? StudioRunTint.orange.color : Color.accentColor.opacity(0.15),
                    in: .capsule)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(badge.accessibilityLabel)
    }
}

// MARK: - The Runs screen

/// The Runs list in a stack of its own, with Done: what the Strands toolbar and a strand's
/// Drafts block present. `strand` filters it to one strand.
public struct StudioRunsScreen: View {
    @Bindable var model: StudioRunsModel
    let strand: String?
    let onDone: () -> Void

    public init(model: StudioRunsModel, strand: String? = nil, onDone: @escaping () -> Void) {
        self.model = model
        self.strand = strand
        self.onDone = onDone
    }

    public var body: some View {
        NavigationStack {
            StudioRunsList(model: model, strand: strand)
                .navigationTitle(strand.map { "Runs: \($0)" } ?? "Runs")
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done", action: onDone)
                    }
                }
                .navigationDestination(for: StudioRunRoute.self) { route in
                    StudioRunDetailView(model: model.detailModel(for: route.runID))
                }
        }
    }
}

/// A run pushed from the list.
public struct StudioRunRoute: Hashable, Identifiable, Sendable {
    public let runID: String
    public var id: String { runID }
    public init(runID: String) { self.runID = runID }
}

/// The rows: live runs first, then the rest, newest first.
struct StudioRunsList: View {
    @Bindable var model: StudioRunsModel
    let strand: String?

    var body: some View {
        let runs = model.runs(strand: strand)
        List {
            if let error = model.loadError {
                Label(error, systemImage: "wifi.exclamationmark")
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
            if runs.isEmpty, model.hasLoaded {
                Text(strand == nil ? "No runs on this bridge." : "No runs for this project.")
                    .foregroundStyle(.secondary)
            } else if runs.isEmpty, model.loadError == nil {
                ProgressView().frame(maxWidth: .infinity)
            }
            ForEach(runs) { run in
                NavigationLink(value: StudioRunRoute(runID: run.runID)) {
                    StudioRunRow(run: run, now: model.currentDate)
                }
            }
        }
        .refreshable { await model.load() }
        // Every 15 seconds while the list is on screen, and never otherwise: the task ends
        // with the view.
        .task {
            while !Task.isCancelled {
                await model.load()
                try? await Task.sleep(for: .seconds(15))
            }
        }
        .studioRunAlert($model.alert)
    }
}

/// One run: its title, then its id, its strand, its status, how old it is, and when a paused
/// run resumes.
struct StudioRunRow: View {
    let run: StudioRun
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            if run.title != nil {
                Text(WorkSemantics.title(run))
                    .font(.body.weight(.semibold))
                    .lineLimit(2)
            }
            HStack(spacing: 6) {
                Text(run.runID)
                    .font(run.title == nil ? .body.monospaced().weight(.semibold)
                                           : .caption.monospaced())
                    .foregroundStyle(run.title == nil ? .primary : .secondary)
                    .lineLimit(1)
                if let strand = run.strand {
                    Text(strand)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                if let age = StudioRunSemantics.age(run.createdAt, now: now) {
                    Text(age)
                        .font(.caption2)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                }
            }
            HStack(spacing: 8) {
                StudioRunStatusLabel(status: run.status)
                if let resume = StudioRunSemantics.resumeLine(run, now: now) {
                    Text(resume)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else if run.status == .queued, let waiting = run.waitingFor {
                    Text("waiting for \(waiting)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            if run.status.isMoving, let line = run.lastLine, !line.isEmpty {
                Text(line)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else if run.title == nil, let text = run.itemText, !text.isEmpty {
                Text(text)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - One run

/// A run's detail in a stack of its own, with Done: what a `Run on Studio` opens.
public struct StudioRunDetailScreen: View {
    let model: StudioRunDetailModel
    let onDone: () -> Void

    public init(model: StudioRunDetailModel, onDone: @escaping () -> Void) {
        self.model = model
        self.onDone = onDone
    }

    public var body: some View {
        NavigationStack {
            StudioRunDetailView(model: model)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done", action: onDone)
                    }
                }
        }
    }
}

/// A run, question first: its title, the question it asks (or its report's first line), the
/// answer field, then the log tail. Run id, strand, step, repository, branch, round and times
/// are folded into a collapsed Details group. Resume (the empty continue) is offered on an
/// interrupted run only; on a `needs_you` run Send needs text.
public struct StudioRunDetailView: View {
    @Bindable var model: StudioRunDetailModel
    @Environment(\.studioRunReport) private var reportAction
    @State private var confirmsCancel = false
    @State private var showsDetails = false

    public init(model: StudioRunDetailModel) {
        self.model = model
    }

    /// Open with Details expanded (a screenshot of them, a test).
    public init(model: StudioRunDetailModel, showsDetails: Bool) {
        self.model = model
        self._showsDetails = State(initialValue: showsDetails)
    }

    public var body: some View {
        List {
            if let error = model.loadError {
                Label(error, systemImage: "wifi.exclamationmark")
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
            if let run = model.run {
                top(run)
                Section("Log") {
                    StudioRunLogView(lines: model.lines)
                        .frame(height: 280)
                        .listRowInsets(EdgeInsets(top: 4, leading: 8, bottom: 4, trailing: 8))
                }
                Section {
                    DisclosureGroup("Details", isExpanded: $showsDetails) {
                        details(run)
                    }
                }
                if model.canCancel {
                    Section {
                        Button(role: .destructive) {
                            confirmsCancel = true
                        } label: {
                            Label("Cancel run", systemImage: "stop.circle")
                        }
                    }
                }
            } else if model.loadError == nil {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .navigationTitle("Run")
        .refreshable { await model.refresh() }
        .task { await model.refresh() }
        // Every 5 seconds while the run can still change by itself; restarted when a reply
        // puts a finished run back in the queue.
        .task(id: model.keepsPolling) {
            guard model.keepsPolling else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                if Task.isCancelled { return }
                await model.refresh()
            }
        }
        .confirmationDialog("Cancel this run?", isPresented: $confirmsCancel,
                            titleVisibility: .visible) {
            Button("Cancel run", role: .destructive) {
                Task { await model.cancel() }
            }
            Button("Keep running", role: .cancel) {}
        } message: {
            Text("The agent is stopped and the round is reported as cancelled.")
        }
        .studioRunAlert($model.alert)
    }

    /// The first screen: the title, the status, the question or the report's first line, and
    /// the answer field. No machine, run id, branch or step here.
    @ViewBuilder
    private func top(_ run: StudioRun) -> some View {
        let now = model.currentDate
        Section {
            VStack(alignment: .leading, spacing: 6) {
                Text(model.title)
                    .font(.title3.weight(.semibold))
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    StudioRunStatusLabel(status: run.status, font: .subheadline)
                    if let resume = StudioRunSemantics.resumeLine(run, now: now) {
                        Text(resume).font(.subheadline).foregroundStyle(.secondary)
                    }
                }
                if let headline = model.headline {
                    Text(headline)
                        .font(.body)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                        .padding(.top, 2)
                }
                if run.status.isMoving, let line = run.lastLine, !line.isEmpty {
                    Text(line)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            .padding(.vertical, 2)
            if let path = run.latestReportPath {
                Button {
                    reportAction?.open(path)
                } label: {
                    Label("Open report", systemImage: "doc.text")
                }
                .disabled(reportAction == nil)
                .accessibilityHint("Opens the report in the Vault tab")
            }
        }
        Section(run.status == .needsYou ? "Your answer" : "Reply") { composer }
    }

    @ViewBuilder
    private func details(_ run: StudioRun) -> some View {
        let now = model.currentDate
        field("Run", run.runID, monospaced: true)
        if run.status == .queued, let waiting = run.waitingFor {
            field("Waiting for", waiting)
        }
        if let error = run.error, !error.isEmpty {
            Text(error).font(.footnote).foregroundStyle(.red)
        }
        if let strand = run.strand { field("Strand", strand) }
        if let item = run.itemID {
            field("Step", [item, run.itemText].compactMap { $0 }.joined(separator: " "))
        } else if let prompt = run.promptPath {
            field("Draft", prompt)
        }
        if let host = run.host, !host.isEmpty { field("Host", host) }
        field("Repository", run.repo)
        field("Branch", run.branch, monospaced: true)
        field("Round", "\(run.round)")
        if let started = StudioRunSemantics.clock(run.startedAt ?? run.createdAt, now: now) {
            field("Started", started)
        }
        if let updated = StudioRunSemantics.clock(run.updatedAt, now: now) {
            let age = StudioRunSemantics.age(run.updatedAt, now: now).map { " (\($0))" } ?? ""
            field("Updated", updated + age)
        }
        if let path = run.latestReportPath { field("Report", path) }
    }

    private func field(_ name: String, _ value: String, monospaced: Bool = false) -> some View {
        LabeledContent(name) {
            Text(value)
                .font(monospaced ? .footnote.monospaced() : .footnote)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        }
    }

    @ViewBuilder
    private var composer: some View {
        TextField(model.status == .needsYou ? "Your answer" : "Reply", text: $model.composer,
                  axis: .vertical)
            .lineLimit(1...6)
            .disabled(!model.isComposerEnabled)
        HStack {
            if model.showsResume {
                Button {
                    Task { await model.resume() }
                } label: {
                    Label("Resume", systemImage: "play.fill")
                }
                .buttonStyle(.bordered)
                .accessibilityHint("Sends \u{201C}\(StudioRunSemantics.resumeText)\u{201D}")
            }
            Spacer()
            if model.isSending { ProgressView().controlSize(.small) }
            Button {
                Task { await model.send() }
            } label: {
                Label("Send", systemImage: "arrow.up.circle.fill")
            }
            .buttonStyle(.borderedProminent)
            .disabled(!model.canSend)
        }
        if let reason = model.replyDisabledReason {
            Text(reason)
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

/// The log tail, monospaced, following new lines unless the reader has scrolled up.
struct StudioRunLogView: View {
    let lines: [String]
    @State private var following = true

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if lines.isEmpty {
                        Text("Nothing in the log yet.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(lines.indices, id: \.self) { i in
                        Text(lines[i])
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Color.clear.frame(height: 1).id(Self.end)
                }
                .font(.system(.caption2, design: .monospaced))
                .textSelection(.enabled)
            }
            .onScrollGeometryChange(for: Bool.self) { g in
                g.contentOffset.y + g.containerSize.height >= g.contentSize.height - 24
            } action: { _, atBottom in
                following = atBottom
            }
            .onChange(of: lines) { _, _ in
                guard following else { return }
                proxy.scrollTo(Self.end, anchor: .bottom)
            }
            .onAppear { proxy.scrollTo(Self.end, anchor: .bottom) }
        }
        .accessibilityLabel("Run log")
    }

    private static let end = "studio-run-log-end"
}

// MARK: - A strand's Drafts lines

/// The open checkbox lines of a strand, above its note: a runnable line offers `Run on
/// Studio` on a long press (a secondary click on the Mac), a line with a live run shows that
/// run's status instead, and every other line is just its words.
///
/// Nothing at all from a bridge that does not serve `drafts`, or for a strand with no open
/// line.
public struct StrandDraftsBlock: View {
    let strand: Strand
    @Bindable var runs: StudioRunsModel
    /// Called after a start, so the board's badge catches up.
    var onStarted: () -> Void = {}

    @State private var openedRun: StudioRunRoute?
    @State private var showsRuns = false
    @State private var startedLine: String?

    public init(strand: Strand, runs: StudioRunsModel, onStarted: @escaping () -> Void = {}) {
        self.strand = strand
        self.runs = runs
        self.onStarted = onStarted
    }

    private var open: [StrandDraft] {
        (strand.drafts ?? []).filter(\.isOpen)
    }

    public var body: some View {
        if !open.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Drafts")
                        .font(.caption)
                        .fontWeight(.semibold)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        showsRuns = true
                    } label: {
                        Label("Runs", systemImage: "desktopcomputer")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityHint("Shows this strand's Studio runs")
                }
                if let startedLine {
                    Label(startedLine, systemImage: "play.circle.fill")
                        .font(.caption)
                        .foregroundStyle(StudioRunTint.blue.color)
                }
                ForEach(open) { draft in
                    line(draft)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 8))
            .task { await runs.load() }
            .sheet(item: $openedRun) { route in
                StudioRunDetailScreen(model: runs.detailModel(for: route.runID)) {
                    openedRun = nil
                }
            }
            .sheet(isPresented: $showsRuns) {
                StudioRunsScreen(model: runs, strand: strand.slug) { showsRuns = false }
            }
            .studioRunAlert($runs.alert)
        }
    }

    @ViewBuilder
    private func line(_ draft: StrandDraft) -> some View {
        let live = runs.liveRun(strand: strand.slug, itemID: draft.id)
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "circle")
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text("\(Text(draft.id).bold()) \(draft.text)")
                .font(.subheadline)
                .lineLimit(3)
                .multilineTextAlignment(.leading)
            Spacer(minLength: 4)
            if runs.starting == draft.id {
                ProgressView().controlSize(.mini)
            } else if let live {
                Button {
                    openedRun = StudioRunRoute(runID: live.runID)
                } label: {
                    StudioRunStatusLabel(status: live.status, font: .caption2)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(live.runID), \(StudioRunSemantics.label(live.status))")
                .accessibilityHint("Opens the run")
            }
        }
        .contentShape(.rect)
        .contextMenu {
            if let live {
                Button {
                    openedRun = StudioRunRoute(runID: live.runID)
                } label: {
                    Label("Open run \(live.runID)", systemImage: "desktopcomputer")
                }
            } else if draft.studioRunnable {
                Button {
                    Task { await start(draft) }
                } label: {
                    Label("Run on Studio", systemImage: "play.circle")
                }
            }
        }
    }

    private func start(_ draft: StrandDraft) async {
        guard let started = await runs.start(strand: strand.slug, itemID: draft.id) else { return }
        startedLine = "Started \(started.runID): \(StudioRunSemantics.label(started.status))"
        onStarted()
        openedRun = StudioRunRoute(runID: started.runID)
    }
}

// MARK: - The alert

extension View {
    /// The bridge's refusal, verbatim, in an alert.
    func studioRunAlert(_ message: Binding<String?>) -> some View {
        alert("Run",
              isPresented: Binding(get: { message.wrappedValue != nil },
                                   set: { if !$0 { message.wrappedValue = nil } })) {
            Button("OK", role: .cancel) { message.wrappedValue = nil }
        } message: {
            Text(message.wrappedValue ?? "")
        }
    }
}

