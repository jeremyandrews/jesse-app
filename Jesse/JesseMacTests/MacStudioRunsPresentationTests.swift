import XCTest
import AppKit
import SwiftUI
import Vision
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// The Studio run screens as the Mac PRESENTS them: in a real window, through the real sheets
/// and pushes, from each place the app opens them. Offscreen renders of the bare views
/// (`MacStudioRunsScreenshotTests`) never caught that the Runs sheet came up about 150 points
/// tall with nothing in it, because they gave the views a frame of their own.
///
/// What each test reads is what a person would see: the sheet's frame from AppKit, the list's
/// rows from its table, and the words drawn in the window, read back with Vision from the
/// window's own drawing. Clicks are events sent to the window. Nothing touches the screen or
/// asks for a permission: the window sits off every display and the app is never activated.
/// (An XCUITest could not run on the Studio: its screen is locked and its Automation Mode asks
/// for a password.)
///
/// Served by fakes from the bridge's own wire fixtures (`JesseKit/Tests/.../Fixtures`): the `Gd`
/// strand, whose Drafts lines GD12 (queued), GD10 (paused) and GD5 (needs you) each have a run.
@MainActor
final class MacStudioRunsPresentationTests: XCTestCase {

    private var window: NSWindow?
    private var defaults: UserDefaults!
    private let suite = "MacStudioRunsPresentationTests"

    override func setUp() async throws {
        try await super.setUp()
        defaults = UserDefaults(suiteName: suite)
        defaults.removePersistentDomain(forName: suite)
        defaults.set(TodaySegment.strands.rawValue, forKey: TodayViewPreferences.segmentKey)
        defaults.set(StudioRunFilter.all.rawValue, forKey: StudioRunFilter.storageKey)
    }

    override func tearDown() async throws {
        if let window {
            while let sheet = window.attachedSheet { window.endSheet(sheet) }
            window.contentView = nil
            window.close()
        }
        window = nil
        defaults.removePersistentDomain(forName: suite)
        try await super.tearDown()
    }

    // MARK: - The entry points

    /// The Strands toolbar's Runs button: the list comes up with its rows, and a row opens
    /// the run inside the sheet.
    func testTheRunsListFromTheToolbarShowsItsRowsAndOpensARun() async throws {
        let (strands, runs, client) = try models()
        let host = host(NavigationStack {
            TodayListView(model: TodayDashboardModel(makeClient: { NoDayClient() }),
                          strands: strands, strandOpener: StrandOpener(), runs: runs)
        })
        try await waitUntil("the board loaded") { runs.hasLoaded }

        runs.presentsList = true
        let sheet = try await sheet(of: host)
        assertSheetIsUsable(sheet)
        let table = try await table(in: sheet, rows: 4)
        assertRowIsHittable(table, row: 0, in: sheet)
        try await waitForText(["Needs me", "On the cluster", "No host", "Run the thing on the Studio"],
                              in: sheet)

        // The first row is GD12 (live first): open it.
        table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        try await waitForText(["On the cluster", "Queued", "Log", "Cancel run"], in: sheet)
        try await waitUntil("the detail read its run") { client.reads.contains("GD12") }
    }

    /// A strand note's Drafts block: the live run's status opens that run, and the block's
    /// own Runs button opens the strand's list, from which a row opens.
    func testTheDraftsBlockOpensItsRunAndItsRunsList() async throws {
        let (strands, runs, client) = try models()
        let opener = StrandOpener()
        let host = host(NavigationStack { Text("Work").frame(maxWidth: .infinity, maxHeight: .infinity) }
            .strandNoteSheet(opener, day: nil, strands: strands, runs: runs,
                             onOpenLink: { _ in }, onSelectItem: { _ in })
            .task { await strands.load(); await runs.load() })
        try await waitUntil("the board loaded") { strands.snapshot != nil && runs.hasLoaded }

        opener.openedNote = .remote(slug: "Gd", title: "Gd", markdown: "# Gd\n\nThe note.")
        let note = try await sheet(of: host)
        // GD12's line wears its run's status, `Queued`: click it.
        let queued = try await waitForText(["Queued"], in: note)[0]
        click(note, at: queued)
        let run = try await sheet(of: note)
        assertSheetIsUsable(run)
        try await waitForText(["On the cluster", "Queued", "Log"], in: run)
        XCTAssertTrue(client.reads.contains("GD12"))

        click(note.attachedSheet!, at: try await waitForText(["Done"], in: run)[0])
        try await waitUntil("the run's sheet closed") { note.attachedSheet == nil }

        // The block's own Runs button.
        click(note, at: try await waitForText(["Runs"], in: note, exact: true)[0])
        let list = try await sheet(of: note)
        assertSheetIsUsable(list)
        let table = try await table(in: list, rows: 3)
        assertRowIsHittable(table, row: 0, in: list)
        try await waitForText(["On the cluster", "No host", "Run the thing on the Studio"], in: list)
        table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        try await waitForText(["On the cluster", "Queued", "Log"], in: list)
    }

    /// A Work board row, then the project view: a Moving row opens its run. The project view is
    /// pushed by an item binding, and a value link on top of it never pushed on the Mac.
    func testAProjectsMovingRowOpensItsRun() async throws {
        let (strands, runs, client) = try models()
        let host = host(NavigationStack {
            StrandsListView(model: strands, opener: StrandOpener(), runs: runs)
        })
        try await waitUntil("the board loaded") { strands.snapshot != nil && runs.hasLoaded }

        // The row reads as one line, `Gd  Paused: No host, 1 queued`.
        click(host, at: try await waitForText(["Gd Paused"], in: host)[0])
        let moving = try await waitForText(["Moving"], in: host, exact: true)[0]
        // GD12's title under Moving, not GD12 anywhere else.
        let found = try await waitForText(["On the cluster"], in: host, below: moving.midY)
        click(host, at: found[0])
        try await waitForText(["On the cluster", "Queued", "Log", "Cancel run"], in: host)
        try await waitUntil("the detail read its run") { client.reads.contains("GD12") }
        let after = try text(in: host)
        XCTAssertFalse(after.contains { $0.string == "Moving" },
                       "the project view is still on screen: the run did not open")
    }

    // MARK: - Screenshots against a bridge

    /// The pull request's screenshots, against a running bridge. Skipped unless the run asks
    /// for it: `TEST_RUNNER_HO6_BRIDGE=host,port,token` and `TEST_RUNNER_HO6_SHOTS_DIR` on the
    /// `xcodebuild test` line, pointed at a scratch bridge serving a COPY of a run store and of
    /// the vault's Strands notes, never the live one. Every call it makes is a read.
    /// `HO6_STRAND` names a strand whose Drafts block has a live run (`Goose`); `HO6_RUN` a run
    /// with a report and a log, opened from the list; `HO6_LOG_RUN` one with a long log.
    func testShotsAgainstABridge() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let raw = env["HO6_BRIDGE"], let dir = env["HO6_SHOTS_DIR"] else {
            throw XCTSkip("screenshots are taken only with HO6_BRIDGE and HO6_SHOTS_DIR set")
        }
        let parts = raw.split(separator: ",").map(String.init)
        let config = JesseConfig(host: parts[0], port: Int(parts[1]) ?? 0, token: parts[2])
        let strandSlug = env["HO6_STRAND"] ?? "Goose"
        let runID = try XCTUnwrap(env["HO6_RUN"])
        let logRunID = try XCTUnwrap(env["HO6_LOG_RUN"])
        let out = URL(fileURLWithPath: dir, isDirectory: true)
        try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

        let strands = StrandsModel(makeClient: { JesseBridgeClient(config: config) })
        let runs = StudioRunsModel(makeClient: { JesseBridgeClient(config: config) })
        let opener = StrandOpener()
        let host = host(NavigationStack {
            TodayListView(model: TodayDashboardModel(makeClient: { NoDayClient() }),
                          strands: strands, strandOpener: opener, runs: runs)
        }
        .strandNoteSheet(opener, day: nil, strands: strands, runs: runs,
                         onOpenLink: { _ in }, onSelectItem: { _ in })
        // As `MacTodayView` injects it, so `Open report` draws as it does in the app.
        .environment(\.studioRunReport, StudioRunReportAction { _ in }))
        try await waitUntil("the board loaded", timeout: 30) {
            strands.snapshot != nil && runs.hasLoaded
        }

        // The all strands list, each filter.
        runs.presentsList = true
        var listSheet = try await sheet(of: host)
        for filter in StudioRunFilter.barSegments {
            defaults.set(filter.rawValue, forKey: StudioRunFilter.storageKey)
            if filter == .archived {
                try await waitUntil("the archived runs", timeout: 30) { runs.hasLoadedArchived }
            }
            try await settle()
            try shoot(listSheet, out, "mac-runs-\(filter.rawValue).png")
        }
        defaults.set(StudioRunFilter.all.rawValue, forKey: StudioRunFilter.storageKey)
        try await settle()
        try shootComposite(host, out, "mac-window-runs-list.png")

        // A run from the list.
        let row = try XCTUnwrap(runs.runs(filter: .all, strand: nil).firstIndex { $0.runID == runID })
        let rows = try await table(in: listSheet, rows: runs.runs.count)
        rows.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        try await settle(3)
        try shoot(listSheet, out, "mac-run-from-list.png")
        runs.presentsList = false
        try await waitUntil("the list closed") { host.attachedSheet == nil }

        // A long log.
        runs.presentsList = true
        listSheet = try await sheet(of: host)
        let logRow = try XCTUnwrap(runs.runs(filter: .all, strand: nil).firstIndex { $0.runID == logRunID })
        try await table(in: listSheet, rows: runs.runs.count)
            .selectRowIndexes(IndexSet(integer: logRow), byExtendingSelection: false)
        try await settle(3)
        try shoot(listSheet, out, "mac-run-log.png")
        runs.presentsList = false
        try await waitUntil("the list closed") { host.attachedSheet == nil }

        // A strand note: its Drafts block's live run, then its Runs list.
        let strand = try XCTUnwrap(strands.snapshot?.strand(slug: strandSlug))
        opener.openedNote = .remote(slug: strand.slug, title: strand.title, markdown: "# \(strand.title)")
        let note = try await sheet(of: host)
        let queued = try await waitForText(["Queued"], in: note, timeout: 20)[0]
        click(note, at: queued)
        let runSheet = try await sheet(of: note)
        try await settle(3)
        try shoot(runSheet, out, "mac-run-from-drafts.png")
        click(runSheet, at: try await waitForText(["Done"], in: runSheet)[0])
        try await waitUntil("the run's sheet closed") { note.attachedSheet == nil }
        click(note, at: try await waitForText(["Runs"], in: note, exact: true)[0])
        let strandList = try await sheet(of: note)
        try await settle(3)
        try shoot(strandList, out, "mac-runs-one-strand.png")
    }

    private func settle(_ seconds: Double = 1.5) async throws {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end { await pause(100) }
    }

    private func image(of w: NSWindow) throws -> NSBitmapImageRep {
        let frame = try XCTUnwrap(w.contentView?.superview)
        let rep = try XCTUnwrap(frame.bitmapImageRepForCachingDisplay(in: frame.bounds))
        frame.cacheDisplay(in: frame.bounds, to: rep)
        return rep
    }

    private func shoot(_ w: NSWindow, _ dir: URL, _ name: String) throws {
        let png = try XCTUnwrap(try image(of: w).representation(using: .png, properties: [:]))
        try png.write(to: dir.appendingPathComponent(name))
    }

    /// The window with its sheet drawn where AppKit put it.
    private func shootComposite(_ w: NSWindow, _ dir: URL, _ name: String) throws {
        let base = try image(of: w)
        let size = w.frame.size
        let composite = NSImage(size: size)
        composite.lockFocus()
        base.draw(in: NSRect(origin: .zero, size: size))
        if let sheet = w.attachedSheet {
            let rep = try image(of: sheet)
            let origin = NSPoint(x: sheet.frame.minX - w.frame.minX,
                                 y: sheet.frame.minY - w.frame.minY)
            rep.draw(in: NSRect(origin: origin, size: sheet.frame.size))
        }
        composite.unlockFocus()
        let tiff = try XCTUnwrap(composite.tiffRepresentation)
        let png = try XCTUnwrap(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        try png.write(to: dir.appendingPathComponent(name))
    }

    // MARK: - Assertions

    /// A sheet a person can use, never the collapsed strip (520 by 149 points for the Runs
    /// list, 470 by 113 for a run) a sheet with only a `List` in it drew. Literal bounds rather
    /// than `StudioRunSheet.minSize`, so this reads the same against a build without it.
    private func assertSheetIsUsable(_ sheet: NSWindow, file: StaticString = #filePath,
                                     line: UInt = #line) {
        XCTAssertGreaterThanOrEqual(sheet.frame.height, 500, "sheet \(sheet.frame)",
                                    file: file, line: line)
        XCTAssertGreaterThanOrEqual(sheet.frame.width, 540, "sheet \(sheet.frame)",
                                    file: file, line: line)
    }

    /// The row's middle is inside the sheet and what is drawn there is the row, not the
    /// filter bar or the sheet's chrome over it.
    private func assertRowIsHittable(_ table: NSTableView, row: Int, in sheet: NSWindow,
                                     file: StaticString = #filePath, line: UInt = #line) {
        guard let frame = sheet.contentView?.superview else {
            return XCTFail("no frame view", file: file, line: line)
        }
        let rect = table.rect(ofRow: row)
        XCTAssertFalse(rect.isEmpty, "row \(row) has no rect", file: file, line: line)
        let mid = NSPoint(x: rect.midX, y: rect.midY)
        XCTAssertTrue(table.visibleRect.contains(mid), "row \(row) is scrolled out of sight",
                      file: file, line: line)
        let inFrame = frame.convert(mid, from: table)
        XCTAssertTrue(frame.bounds.contains(inFrame), "row \(row) is outside the sheet",
                      file: file, line: line)
        let hit = frame.hitTest(inFrame)
        XCTAssertTrue(hit?.isDescendant(of: table) ?? false,
                      "row \(row) is covered by \(hit.map { "\(type(of: $0))" } ?? "nothing")",
                      file: file, line: line)
    }

    // MARK: - Hosting, waiting, clicking, reading

    private func models() throws -> (StrandsModel, StudioRunsModel, FakeRunsClient) {
        let client = try FakeRunsClient()
        let board = try FakeStrandsClient()
        return (StrandsModel(makeClient: { board }), StudioRunsModel(makeClient: { client }), client)
    }

    /// A titled window off every display, holding the view.
    private func host<V: View>(_ view: V) -> NSWindow {
        let w = OffscreenWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 1_000, height: 860),
                         styleMask: [.titled, .closable, .resizable], backing: .buffered,
                         defer: false)
        w.isReleasedWhenClosed = false
        w.contentView = NSHostingView(rootView: view.defaultAppStorage(defaults))
        w.orderFront(nil)
        // Ordering a window in puts it back on a display; it goes back off before anything draws.
        w.setFrameOrigin(NSPoint(x: -20_000, y: -20_000))
        XCTAssertLessThan(w.frame.maxX, -10_000, "the test window must stay off every display")
        window = w
        return w
    }

    private func pause(_ ms: Int = 50) async {
        try? await Task.sleep(for: .milliseconds(ms))
        window?.contentView?.layoutSubtreeIfNeeded()
    }

    private func waitUntil(_ what: String, timeout: Double = 10,
                           _ condition: () -> Bool) async throws {
        let end = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < end else { throw Failure("timed out waiting for \(what)") }
            await pause()
        }
    }

    /// The sheet a window presents, once it has settled at its size.
    private func sheet(of parent: NSWindow) async throws -> NSWindow {
        try await waitUntil("a sheet on \(parent.title)") { parent.attachedSheet != nil }
        let sheet = parent.attachedSheet!
        var last = NSRect.zero
        for _ in 0..<40 where sheet.frame != last {
            last = sheet.frame
            try? await Task.sleep(for: .milliseconds(100))
        }
        return sheet
    }

    /// The list's table, once it holds `rows` rows.
    private func table(in sheet: NSWindow, rows: Int) async throws -> NSTableView {
        var found: NSTableView?
        try await waitUntil("a list of \(rows) rows") {
            found = sheet.contentView.flatMap { Self.table(in: $0) }
            return found?.numberOfRows == rows
        }
        return found!
    }

    private static func table(in view: NSView) -> NSTableView? {
        if let table = view as? NSTableView { return table }
        for sub in view.subviews { if let t = table(in: sub) { return t } }
        return nil
    }

    /// A left click on a line of text the window drew.
    private func click(_ w: NSWindow, at box: TextBox) {
        click(w, x: box.midX, yTop: box.midY)
    }

    private func click(_ w: NSWindow, x: CGFloat, yTop: CGFloat) {
        guard let frame = w.contentView?.superview else { return }
        let point = NSPoint(x: x, y: frame.bounds.height - yTop)
        func event(_ type: NSEvent.EventType) -> NSEvent? {
            NSEvent.mouseEvent(with: type, location: point, modifierFlags: [],
                               timestamp: ProcessInfo.processInfo.systemUptime,
                               windowNumber: w.windowNumber, context: nil, eventNumber: 0,
                               clickCount: 1, pressure: 1)
        }
        guard let down = event(.leftMouseDown), let up = event(.leftMouseUp) else { return }
        // The up is queued first: a control tracks the mouse from its down until it reads one.
        NSApp.postEvent(up, atStart: false)
        w.sendEvent(down)
    }

    struct TextBox {
        let string: String
        let rect: CGRect
        var midX: CGFloat { rect.midX }
        var midY: CGFloat { rect.midY }
    }

    /// The words drawn in a window, frame and title bar included, with where each line is
    /// (points, top left origin).
    private func text(in w: NSWindow) throws -> [TextBox] {
        guard let frame = w.contentView?.superview,
              let rep = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else { return [] }
        frame.cacheDisplay(in: frame.bounds, to: rep)
        guard let image = rep.cgImage else { return [] }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: image).perform([request])
        let size = frame.bounds.size
        return (request.results ?? []).compactMap { observation in
            guard let best = observation.topCandidates(1).first else { return nil }
            let box = observation.boundingBox
            return TextBox(string: best.string,
                           rect: CGRect(x: box.minX * size.width,
                                        y: (1 - box.maxY) * size.height,
                                        width: box.width * size.width,
                                        height: box.height * size.height))
        }
    }

    /// Wait until every string is drawn in the window; the boxes of each, in order.
    @discardableResult
    private func waitForText(_ strings: [String], in w: NSWindow, exact: Bool = false,
                             below: CGFloat = -1, timeout: Double = 10) async throws -> [TextBox] {
        let end = Date().addingTimeInterval(timeout)
        var seen: [TextBox] = []
        while true {
            seen = try text(in: w).filter { $0.rect.minY > below }
            let boxes = strings.compactMap { want in
                seen.first { exact ? $0.string == want : $0.string.contains(want) }
            }
            if boxes.count == strings.count { return boxes }
            guard Date() < end else {
                throw Failure("never drew \(strings); drew \(seen.map(\.string))")
            }
            try? await Task.sleep(for: .milliseconds(200))
        }
    }

    struct Failure: Error, CustomStringConvertible {
        let description: String
        init(_ description: String) { self.description = description }
    }
}

/// A window that stays where it is put: AppKit moves a titled window back onto a display when
/// it is ordered in, and these windows belong off every display.
private final class OffscreenWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
        frameRect
    }
}

// MARK: - Fakes

private func fixture(_ name: String) throws -> Data {
    let url = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("JesseKit/Tests/JesseNetworkingTests/Fixtures/\(name).json")
    return try Data(contentsOf: url)
}

/// The bridge's own list (`studio-runs-list.json`), each run's record and the log fixture.
/// Every write refuses: none of these tests posts anything.
private final class FakeRunsClient: StudioRunsProviding, @unchecked Sendable {
    let list: StudioRunList
    let log: StudioRunLog
    private let lock = NSLock()
    private var _reads: [String] = []
    var reads: [String] { lock.withLock { _reads } }

    init() throws {
        list = try StudioRunList.decode(from: fixture("studio-runs-list"))
        log = try StudioRunLog.decode(from: fixture("studio-run-log"))
    }

    func listRuns(archived: StudioRunArchivedMode) async throws -> StudioRunList { list }
    func getRun(id: String) async throws -> StudioRun {
        lock.withLock { _reads.append(id) }
        guard let run = list.runs.first(where: { $0.runID == id }) else {
            throw URLError(.fileDoesNotExist)
        }
        return run
    }
    func runLog(id: String, tail: Int?) async throws -> StudioRunLog { log }
    func startRun(strand: String, itemID: String) async throws -> StudioRunStarted { throw refused }
    func replyToRun(id: String, text: String) async throws -> StudioRun { throw refused }
    func cancelRun(id: String) async throws -> StudioRun { throw refused }
    func archive(id: String) async throws -> StudioRun { throw refused }
    func unarchive(id: String) async throws -> StudioRun { throw refused }
    func lastLook() async throws -> WorkLastLook { WorkLastLook() }
    func markLastLook() async throws -> WorkLastLook { WorkLastLook() }
    private var refused: URLError { URLError(.cancelled) }
}

/// The bridge's own board (`strands-annotated.json`): the `Gd` strand with its Drafts lines.
private struct FakeStrandsClient: StrandsProviding {
    let board: StrandsSnapshot
    init() throws { board = try StrandsSnapshot.decode(from: fixture("strands-annotated")) }
    func getStrands(ifNoneMatch: String?) async throws -> StrandsFetchResult { .snapshot(board) }
    func getStrand(slug: String) async throws -> StrandDetail {
        StrandDetail(markdown: "# Gd", strand: board.strand(slug: slug))
    }
}

/// No day at all: these tests are about the Work segment.
private struct NoDayClient: TodayProviding {
    func getToday(ifNoneMatch: String?) async throws -> TodayFetchResult { throw URLError(.notConnectedToInternet) }
    func checkItem(id: String, checked: Bool, evidence: String?, at: Date, day: String?,
                   ifMatch: String) async throws -> TodayMutationResult { throw URLError(.cancelled) }
    func moveItem(id: String, op: TodayMoveOp, at: Date, day: String?,
                  ifMatch: String) async throws -> TodayMutationResult { throw URLError(.cancelled) }
    func postpone(id: String, deferred: Bool, at: Date, day: String?,
                  ifMatch: String) async throws -> TodayMutationResult { throw URLError(.cancelled) }
    func glance(id: String, at: Date, ifMatch: String) async throws -> TodayMutationResult { throw URLError(.cancelled) }
}
