import XCTest
import AppKit
import SwiftUI
import SwiftData
import JesseCore
import JesseNetworking
import JesseSearch
import JesseVault
@testable import Jesse_Mac

/// Where the window's search fields actually are, and whether the sidebar toolbar fits.
///
/// The whole `MacShellView` is hosted the way the app's `WindowGroup` builds it, its SwiftUI
/// toolbar bridged into a real `NSToolbar` with the unified style, in a window ordered in
/// BEHIND everything and OFF every display, and never key. Ordering it in is what makes
/// `NSToolbar` lay its items out and decide which overflow; nothing appears on any screen,
/// nothing is captured from the screen, and no permission is asked for.
///
/// THE DEFECT THESE PIN. Inside the window's `TabView`, a sidebar `.searchable` is not drawn in
/// the sidebar: SwiftUI makes it a principal toolbar search item, which cannot be laid out
/// beside the tab picker, so it went into the toolbar's overflow menu and took every button
/// in the sidebar section with it, at every window and sidebar width. On that code the first
/// two tests below fail.
@MainActor
final class MacSearchVisibilityTests: XCTestCase {

    private var windows: [NSWindow] = []

    override func tearDown() async throws {
        for window in windows {
            window.orderOut(nil)
            window.contentViewController = nil
            window.toolbar = nil
            window.close()
        }
        windows = []
        try await super.tearDown()
    }

    // MARK: - Hosting

    /// A window AppKit may not pull back onto a display and that can never take key or main
    /// status: ordering it in makes the toolbar lay out, and changes nothing else.
    final class OffscreenWindow: NSWindow {
        override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
            frameRect
        }
        override var canBecomeKey: Bool { false }
        override var canBecomeMain: Bool { false }
    }

    struct Shell {
        let window: NSWindow
        let container: ModelContainer
        let commands: MacWindowCommands
    }

    static let offscreenOrigin = NSPoint(x: -20_000, y: -20_000)

    /// A vault model over a scratch folder of a few notes, so the Vault tab has something to
    /// show and nothing reads the folder this Mac holds.
    static func scratchVault() async throws -> VaultBrowserModel {
        let base = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacSearchVisibility.\(UUID().uuidString)", isDirectory: true)
        let root = base.appendingPathComponent("vault", isDirectory: true)
        let notes = [
            "Notes/Kiln.md": "# The kiln\n\nThe chamber floor cracked along the backseam in spring.",
            "Notes/Garden.md": "# Garden\n\nTomatoes went in on the south bed, basil beside them.",
            "Notes/Bridge deploy.md": "# Bridge deploy\n\nRebuild, restart the launchd job, check health.",
            "Notes/Reading list.md": "# Reading list\n\nThe Overstory, then the Le Guin essays.",
            "Daily/2026-11-10.md": "# Tuesday\n\nRan the morning routine, then cut the release.",
        ]
        for (path, text) in notes {
            let url = root.appendingPathComponent(path)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            try text.write(to: url, atomically: true, encoding: .utf8)
        }
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "MacSearchVisibility.\(UUID().uuidString)"))
        let folder = VaultFolder(defaults: defaults, key: "search.visibility.bookmark")
        try folder.adopt(url: root)
        let source = VaultIndexSource(folder: folder,
                                      container: base.appendingPathComponent("index", isDirectory: true))
        let model = VaultBrowserModel(source: source, debounce: .zero)
        await model.indexer.reindexNow()
        model.refresh()
        return model
    }

    /// The whole shell in an offscreen window, as the app's `WindowGroup` builds it.
    func hostShell(windowWidth: CGFloat, height: CGFloat = 1_000,
                   tab: MacShellView.Tab = .chats,
                   seed: (ModelContext) throws -> Void = { _ in }) async throws -> Shell {
        let container = try ModelContainer(
            for: jesseCurrentSchema,
            configurations: ModelConfiguration(schema: jesseCurrentSchema,
                                               isStoredInMemoryOnly: true))
        try seed(container.mainContext)
        try container.mainContext.save()
        // Paired, so New Chat and the morning routine are enabled as they are in use, but at
        // an address that refuses at once: nothing here reaches a real bridge.
        let config = MacConfigStore(config: JesseConfig(host: "127.0.0.1", port: 9, token: "tok"))
        let coordinator = MacCoordinator(configStore: config,
                                         makeClient: { _ in MacFakeBridgeClient() },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let commands = MacWindowCommands()
        let root = MacShellView(storeError: nil, vaultModel: try await Self.scratchVault(),
                                initialTab: tab, commands: commands,
                                searchExpander: NoExpansion())
            .environment(coordinator)
            .modelContainer(container)
        let controller = NSHostingController(rootView: root)
        controller.sceneBridgingOptions = [.toolbars, .title]
        let frame = NSRect(origin: Self.offscreenOrigin,
                           size: NSSize(width: windowWidth, height: height))
        let window = OffscreenWindow(contentRect: frame,
                                     styleMask: [.titled, .closable, .miniaturizable, .resizable,
                                                 .fullSizeContentView],
                                     backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.toolbarStyle = .unified
        window.contentViewController = controller
        window.setFrame(frame, display: false)
        windows.append(window)
        window.orderBack(nil)
        await settle(window)
        XCTAssertNil(window.screen, "the test window must never be on a display")
        return Shell(window: window, container: container, commands: commands)
    }

    /// Layout and the run loop, until the toolbar and the list have landed.
    func settle(_ window: NSWindow, rounds: Int = 30) async {
        for _ in 0..<rounds {
            try? await Task.sleep(for: .milliseconds(30))
            window.contentView?.superview?.layoutSubtreeIfNeeded()
        }
    }

    /// Settles until `condition` holds or about three seconds pass.
    func settle(_ window: NSWindow, until condition: @escaping () -> Bool) async {
        for _ in 0..<100 {
            if condition() { return }
            await settle(window, rounds: 1)
        }
    }

    // MARK: - The view tree

    static func all(_ view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap { all($0) }
    }

    static func frameViews(_ window: NSWindow) -> [NSView] {
        window.contentView?.superview.map { all($0) } ?? []
    }

    /// The navigation split view: the widest vertical `NSSplitView` with two or more panes.
    static func splitView(in window: NSWindow) -> NSSplitView? {
        frameViews(window).compactMap { $0 as? NSSplitView }
            .filter { $0.isVertical && $0.arrangedSubviews.count >= 2 }
            .max { $0.frame.width < $1.frame.width }
    }

    func setSidebarWidth(_ width: CGFloat, in window: NSWindow) async {
        guard let split = Self.splitView(in: window) else { return }
        split.setPosition(width, ofDividerAt: 0)
        await settle(window, rounds: 15)
    }

    static func sidebarWidth(in window: NSWindow) -> CGFloat {
        splitView(in: window)?.arrangedSubviews.first?.frame.width ?? 0
    }

    /// The sidebar's list: the table view whose frame starts in the sidebar column.
    static func sidebarTable(in window: NSWindow) -> NSTableView? {
        let width = sidebarWidth(in: window)
        return frameViews(window).compactMap { $0 as? NSTableView }.first { table in
            table.convert(table.bounds, to: nil).minX < width
        }
    }

    struct FieldReport: CustomStringConvertible {
        let field: NSSearchField
        let placeholder: String
        let inWindow: Bool
        let hidden: Bool
        let frameInWindow: NSRect
        let visibleRect: NSRect
        let toolbarItem: String?
        let inVisibleItems: Bool?

        /// The part of the field no ancestor clips. `visibleRect` alone is not bounded by the
        /// field's own bounds when no ancestor clips to its own.
        var unclipped: NSRect { visibleRect.intersection(field.bounds) }

        var description: String {
            "field '\(placeholder)': inWindow=\(inWindow) hidden=\(hidden) "
                + "frame=\(NSStringFromRect(frameInWindow)) visibleRect=\(NSStringFromRect(visibleRect)) "
                + "toolbarItem=\(toolbarItem ?? "none") inVisibleItems=\(inVisibleItems.map(String.init) ?? "n/a")"
        }
    }

    /// Every `NSSearchField` the window has, whether in its view tree or held by a toolbar item
    /// whose view is in the overflow (and so out of the window entirely).
    static func searchFields(in window: NSWindow) -> [FieldReport] {
        var views = frameViews(window)
        for item in window.toolbar?.items ?? [] {
            if let view = item.view { views += all(view) }
            if let search = item as? NSSearchToolbarItem { views += all(search.searchField) }
        }
        var seen = Set<ObjectIdentifier>()
        let visible = window.toolbar?.visibleItems ?? []
        return views.compactMap { $0 as? NSSearchField }
            .filter { seen.insert(ObjectIdentifier($0)).inserted }
            .map { field in
                var hidden = false
                var view: NSView? = field
                while let v = view { if v.isHidden { hidden = true }; view = v.superview }
                let item = window.toolbar?.items.first { item in
                    if let s = item as? NSSearchToolbarItem, s.searchField === field { return true }
                    if let v = item.view { return field.isDescendant(of: v) }
                    return false
                }
                let inWindow = field.window === window
                return FieldReport(
                    field: field,
                    placeholder: field.placeholderString ?? "",
                    inWindow: inWindow,
                    hidden: hidden,
                    frameInWindow: inWindow ? field.convert(field.bounds, to: nil) : .zero,
                    visibleRect: inWindow ? field.visibleRect : .zero,
                    toolbarItem: item?.itemIdentifier.rawValue,
                    inVisibleItems: item.map { i in visible.contains { $0 === i } })
            }
    }

    static func field(_ placeholder: String, in window: NSWindow) -> FieldReport? {
        searchFields(in: window).first { $0.placeholder == placeholder }
    }

    /// Items that occupy room in the toolbar. The flexible space and the split view's
    /// tracking separator are layout, not items anyone clicks.
    static func realItems(_ window: NSWindow) -> [NSToolbarItem] {
        (window.toolbar?.items ?? []).filter {
            !($0.itemIdentifier == .flexibleSpace || $0 is NSTrackingSeparatorToolbarItem)
        }
    }

    static func toolbarSummary(_ window: NSWindow) -> String {
        guard let toolbar = window.toolbar else { return "  no toolbar" }
        let visible = toolbar.visibleItems ?? []
        return toolbar.items.map { item in
            let shown = visible.contains { $0 === item } ? "visible " : "OVERFLOW"
            return "  [\(shown)] \(type(of: item)) '\(item.label)' (\(item.itemIdentifier.rawValue))"
        }.joined(separator: "\n")
    }

    /// Whether the caret is in `field`: the window's field editor is editing it.
    static func hasCaret(_ field: NSSearchField, in window: NSWindow) -> Bool {
        guard let editor = window.firstResponder as? NSText else { return false }
        return editor.delegate === field
    }

    /// Which conversation a sidebar row is: select it and read the window title, which the
    /// detail column sets to the conversation's title. (SwiftUI builds no accessibility tree
    /// for the rows in a test host, so their text cannot be read directly.)
    func conversationTitle(atRow row: Int, of table: NSTableView, in window: NSWindow) async -> String {
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        let before = window.title
        await settle(window) { window.title != before }
        await settle(window, rounds: 3)
        return window.title
    }

    // MARK: - Fixtures

    struct Conversation {
        let title: String
        let question: String
        let answer: String
        let daysAgo: Int
        var favorite = false
    }

    /// Fifteen conversations with the kind of titles and answers the sidebar really holds,
    /// spread over the last few weeks, two of them starred.
    static let conversations: [Conversation] = [
        Conversation(title: "Kiln firing schedule for the stoneware batch",
                     question: "When should I candle the stoneware?",
                     answer: "Candle overnight at 90 °C, then ramp to cone 6; the kiln sitter trips about nine hours in.",
                     daysAgo: 0, favorite: true),
        Conversation(title: "Bridge deploy checklist",
                     question: "What do I run after merging the bridge change?",
                     answer: "Pull, rebuild with cargo, restart the launchd job, then check the health endpoint.",
                     daysAgo: 0),
        Conversation(title: "Morning routine: Tuesday", question: "Start the day.",
                     answer: "Three meetings, the release cut at 14:00, and unloading the kiln after lunch.",
                     daysAgo: 0),
        Conversation(title: "Tomato blight on the south bed",
                     question: "The lower leaves have brown rings. Blight?",
                     answer: "Early blight, most likely. Strip the lower leaves and mulch so soil does not splash.",
                     daysAgo: 1),
        Conversation(title: "Rewriting the vault indexer in Rust",
                     question: "Is FTS5 enough for 5,000 notes?",
                     answer: "Yes. The index is about 40 MB and a ranked query returns in under 10 ms.",
                     daysAgo: 1, favorite: true),
        Conversation(title: "Trip to Lisbon in March", question: "Which neighbourhood for four nights?",
                     answer: "Príncipe Real: quiet at night, walkable to Bairro Alto and the miradouros.",
                     daysAgo: 2),
        Conversation(title: "Sourdough starter is sluggish", question: "It barely doubled in 12 hours.",
                     answer: "Feed at 1:5:5 with some rye and keep it near 26 °C for two days.",
                     daysAgo: 3),
        Conversation(title: "Quarterly planning notes", question: "Summarize what we agreed for Q1.",
                     answer: "Ship the Mac app's offline answers, retire the old sync path, hire one designer.",
                     daysAgo: 4),
        Conversation(title: "Heart rate variability trend", question: "Why is my HRV down this week?",
                     answer: "Three short nights and two late meals; the dip tracks sleep, not training load.",
                     daysAgo: 5),
        Conversation(title: "Swift 6 strict concurrency errors",
                     question: "Why does this closure need to be Sendable?",
                     answer: "It escapes into a Task that may run off the main actor, so its captures must be Sendable.",
                     daysAgo: 6),
        Conversation(title: "Gift ideas for Anna's birthday",
                     question: "Something for a potter who has everything?",
                     answer: "A wax resist set, a banding wheel, or a weekend raku workshop.",
                     daysAgo: 8),
        Conversation(title: "Replacing the bike chain", question: "How do I know when the chain is worn?",
                     answer: "Measure with a chain checker: past 0.5% stretch on 11 speed, replace it.",
                     daysAgo: 10),
        Conversation(title: "Reading list for winter", question: "Novels like The Overstory?",
                     answer: "Try Greenwood, The Hidden Life of Trees, and Ursula Le Guin's essays.",
                     daysAgo: 13),
        Conversation(title: "Tax documents for the accountant",
                     question: "What does she need from me this year?",
                     answer: "The annual statements, the pension letter and the receipts for the studio kiln.",
                     daysAgo: 17),
        Conversation(title: "Studio electrical upgrade",
                     question: "Can the garage circuit take a 7 kW kiln?",
                     answer: "Not as wired: it needs its own 32 A circuit; get the electrician to quote it.",
                     daysAgo: 24),
    ]

    static func seedConversations(_ context: ModelContext) {
        let now = Date.now
        for (index, c) in conversations.enumerated() {
            let when = now.addingTimeInterval(-Double(c.daysAgo) * 86_400 - Double(index) * 600)
            let thread = JesseThread(mode: .ask)
            thread.title = c.title
            thread.createdAt = when
            thread.updatedAt = when
            thread.conversationId = "conv-\(index)"
            thread.sessionId = "sess-\(index)"
            context.insert(thread)
            let question = Turn(role: .user, text: c.question, createdAt: when)
            let answer = Turn(role: .jesse, text: c.answer, createdAt: when.addingTimeInterval(20))
            context.insert(question)
            context.insert(answer)
            question.thread = thread
            answer.thread = thread
            if c.favorite { thread.setFavorite(true, now: when) }
        }
    }

    // MARK: - (1) The Chats search field is in the sidebar at every width

    func testTheChatsSearchFieldIsInTheSidebarAtEveryWidth() async throws {
        for windowWidth in [CGFloat(1_000), 1_600] {
            let shell = try await hostShell(windowWidth: windowWidth, seed: Self.seedConversations)
            let window = shell.window
            for requested in [CGFloat(240), 300, 420] {
                await setSidebarWidth(requested, in: window)
                let sidebar = Self.sidebarWidth(in: window)
                print("MS1-PROBE window=\(Int(windowWidth)) sidebar requested=\(Int(requested)) actual=\(sidebar)")
                print(Self.toolbarSummary(window))
                for f in Self.searchFields(in: window) { print("  \(f)") }
                let label = "window \(Int(windowWidth)), sidebar \(Int(requested))"

                let field = try XCTUnwrap(Self.field(SearchFieldDriving.prompt, in: window),
                                          "\(label): no conversation search field at all")
                if field.toolbarItem != nil {
                    XCTAssertEqual(field.inVisibleItems, true,
                                   "\(label): the search field is only in the toolbar's overflow menu")
                }
                XCTAssertTrue(field.inWindow, "\(label): the search field is not in the window")
                XCTAssertFalse(field.hidden, "\(label): the search field is hidden")
                let bounds = window.contentView?.bounds ?? .zero
                XCTAssertTrue(bounds.contains(field.frameInWindow),
                              "\(label): the field \(field.frameInWindow) is outside the window \(bounds)")
                XCTAssertLessThanOrEqual(field.frameInWindow.maxX, sidebar,
                                         "\(label): the field is not inside the sidebar column")
                XCTAssertGreaterThan(field.frameInWindow.width, 150,
                                     "\(label): the field is too narrow to type in")
                XCTAssertEqual(field.unclipped, field.field.bounds,
                               "\(label): part of the field is clipped")
            }
        }
    }

    // MARK: - (2) The sidebar toolbar fits at the narrowest sidebar

    func testEverySidebarToolbarItemIsLaidOutAtEveryWidth() async throws {
        for windowWidth in [CGFloat(1_000), 1_600] {
            let shell = try await hostShell(windowWidth: windowWidth)
            for requested in [CGFloat(240), 300, 420] {
                await setSidebarWidth(requested, in: shell.window)
                let label = "window \(Int(windowWidth)), sidebar \(Int(requested)) "
                    + "(actual \(Self.sidebarWidth(in: shell.window)))"
                let visible = shell.window.toolbar?.visibleItems ?? []
                let overflow = Self.realItems(shell.window).filter { item in
                    !visible.contains { $0 === item }
                }
                XCTAssertEqual(overflow.map(\.label), [],
                               "\(label): toolbar items in the overflow menu")
                for name in ["Refresh", MorningRoutine.dialogTitle, "New Chat"] {
                    XCTAssertTrue(visible.contains { $0.label == name },
                                  "\(label): '\(name)' is not laid out")
                }
            }
        }
    }

    /// A request for 240 lands at the split view's own minimum, a few points wider: the
    /// narrowest sidebar there is, and the one (2) has to fit. If it ever moves far from
    /// 240 the column's declared minimum has changed and (2) must be re-read.
    func testARequestFor240LandsAtTheNarrowestSidebar() async throws {
        let shell = try await hostShell(windowWidth: 1_000)
        await setSidebarWidth(240, in: shell.window)
        let narrowest = Self.sidebarWidth(in: shell.window)
        await setSidebarWidth(250, in: shell.window)
        print("MS1-PROBE a 240 request lands at \(narrowest); a 250 request at \(Self.sidebarWidth(in: shell.window))")
        XCTAssertGreaterThanOrEqual(narrowest, 240)
        XCTAssertLessThan(narrowest, 270)
        XCTAssertEqual(Self.sidebarWidth(in: shell.window), narrowest,
                       "nothing narrower than the first request is possible")
    }

    // MARK: - (3) Typing into the real field filters the sidebar

    func testTypingIntoTheFieldListsTheMatchingConversation() async throws {
        let shell = try await hostShell(windowWidth: 1_600, seed: Self.seedConversations)
        let window = shell.window
        let field = try XCTUnwrap(Self.field(SearchFieldDriving.prompt, in: window)).field
        let table = try XCTUnwrap(Self.sidebarTable(in: window), "no sidebar list")
        let before = table.numberOfRows
        XCTAssertGreaterThan(before, 3, "the seeded sidebar should list several conversations")

        // The caret by Command F, then typed through the window's field editor: the path a
        // keystroke takes.
        shell.commands.requestSearch()
        await settle(window) { Self.hasCaret(field, in: window) }
        let editor = try XCTUnwrap(window.firstResponder as? NSTextView,
                                   "the field did not take the caret")
        editor.insertText("blight", replacementRange: editor.selectedRange())
        await settle(window) { table.numberOfRows == 1 }

        XCTAssertEqual(field.stringValue, "blight")
        XCTAssertEqual(table.numberOfRows, 1,
                       "the sidebar should narrow to the one conversation that mentions blight")
        let title = await conversationTitle(atRow: 0, of: table, in: window)
        XCTAssertEqual(title, "Tomato blight on the south bed",
                       "the remaining row is not the matching conversation")
        table.deselectAll(nil)
        await settle(window, rounds: 5)

        // Clearing the field restores the list.
        editor.selectAll(nil)
        editor.deleteBackward(nil)
        await settle(window) { table.numberOfRows == before }
        XCTAssertEqual(table.numberOfRows, before)
    }

    // MARK: - (4) The Conversations commands

    func testTheConversationsCommandsFlipFavoritesAndArchiveTheSelection() async throws {
        let shell = try await hostShell(windowWidth: 1_600, seed: Self.seedConversations)
        let window = shell.window
        let commands = shell.commands
        let table = try XCTUnwrap(Self.sidebarTable(in: window))
        let state = try XCTUnwrap(commands.conversations, "the Chats tab published no menu state")
        XCTAssertFalse(state.favoritesShown)
        XCTAssertNil(state.selectionArchived, "nothing is selected yet")
        XCTAssertTrue(state.canStartChat)

        // Show Favorites (⇧⌘F): the scope flips, and the sidebar lists the two starred.
        let all = table.numberOfRows
        commands.file(.toggleFavorites)
        await settle(window) { commands.conversations?.favoritesShown == true && table.numberOfRows == 2 }
        XCTAssertEqual(commands.conversations?.favoritesShown, true)
        XCTAssertEqual(table.numberOfRows, 2, "the favorites scope lists the two starred conversations")
        commands.file(.toggleFavorites)
        await settle(window) { commands.conversations?.favoritesShown == false && table.numberOfRows == all }
        XCTAssertEqual(commands.conversations?.favoritesShown, false)
        XCTAssertEqual(table.numberOfRows, all)

        // Archive Selected (⇧⌘A): select a conversation's row, then archive it.
        var found = false
        for row in 0..<table.numberOfRows where table.rowView(atRow: row, makeIfNecessary: true) != nil {
            if await conversationTitle(atRow: row, of: table, in: window) == "Bridge deploy checklist" {
                found = true
                break
            }
        }
        XCTAssertTrue(found, "the seeded conversation is not listed")
        await settle(window) { commands.conversations?.selectionArchived != nil }
        XCTAssertEqual(commands.conversations?.selectionArchived, false,
                       "selecting a row enables Archive Selected")
        commands.file(.archiveSelected)
        await settle(window) { commands.conversations?.selectionArchived == true }

        let threads = try shell.container.mainContext.fetch(FetchDescriptor<JesseThread>())
        XCTAssertEqual(threads.filter(\.isArchived).map(\.title), ["Bridge deploy checklist"])
    }

    // MARK: - Command F

    func testCommandFPutsTheCaretInTheConversationSearch() async throws {
        let shell = try await hostShell(windowWidth: 1_600, seed: Self.seedConversations)
        let field = try XCTUnwrap(Self.field(SearchFieldDriving.prompt, in: shell.window)).field
        XCTAssertFalse(Self.hasCaret(field, in: shell.window))
        shell.commands.requestSearch()
        await settle(shell.window) { Self.hasCaret(field, in: shell.window) }
        XCTAssertTrue(Self.hasCaret(field, in: shell.window))
    }

    func testCommandFFromTheTodayTabShowsChatsAndFocusesItsSearch() async throws {
        let shell = try await hostShell(windowWidth: 1_600, tab: .today,
                                        seed: Self.seedConversations)
        XCTAssertEqual(shell.commands.tab, .today)
        shell.commands.requestSearch()
        await settle(shell.window) {
            shell.commands.tab == .chats
                && (Self.field(SearchFieldDriving.prompt, in: shell.window)
                        .map { Self.hasCaret($0.field, in: shell.window) } ?? false)
        }
        XCTAssertEqual(shell.commands.tab, .chats)
        let field = try XCTUnwrap(Self.field(SearchFieldDriving.prompt, in: shell.window)).field
        XCTAssertTrue(Self.hasCaret(field, in: shell.window))
    }

    // MARK: - (5) The Vault tab

    func testTheVaultSearchFieldIsOnScreenAndCommandFFocusesIt() async throws {
        for windowWidth in [CGFloat(1_000), 1_600] {
            let shell = try await hostShell(windowWidth: windowWidth, tab: .vault)
            print("MS1-PROBE vault window=\(Int(windowWidth))")
            print(Self.toolbarSummary(shell.window))
            for f in Self.searchFields(in: shell.window) { print("  \(f)") }
            let field = try XCTUnwrap(Self.field("Search every note", in: shell.window),
                                      "no vault search field")
            if field.toolbarItem != nil { XCTAssertEqual(field.inVisibleItems, true) }
            XCTAssertTrue(field.inWindow)
            XCTAssertFalse(field.hidden)
            let bounds = shell.window.contentView?.bounds ?? .zero
            XCTAssertTrue(bounds.contains(field.frameInWindow))
            XCTAssertEqual(field.unclipped, field.field.bounds)

            shell.commands.requestSearch()
            await settle(shell.window) { Self.hasCaret(field.field, in: shell.window) }
            XCTAssertEqual(shell.commands.tab, .vault, "Command F on the Vault tab stays there")
            XCTAssertTrue(Self.hasCaret(field.field, in: shell.window),
                          "Command F on the Vault tab focuses the vault's own search")
        }
    }

    // MARK: - The menu bar

    static func shortcut(_ item: NSMenuItem) -> String {
        guard !item.keyEquivalent.isEmpty else { return "" }
        var mods = ""
        let m = item.keyEquivalentModifierMask
        if m.contains(.control) { mods += "⌃" }
        if m.contains(.option) { mods += "⌥" }
        if m.contains(.shift) { mods += "⇧" }
        if m.contains(.command) { mods += "⌘" }
        return mods + item.keyEquivalent.uppercased()
    }

    /// The menus as the app built them in this host. The test host has no key window of its
    /// own making, so the Chats items read disabled; the shortcuts that are always attached
    /// show as they will in use. Archive Selected carries ⇧⌘A only while a conversation is
    /// selected.
    func testTheConversationsMenuIsBuiltBeforeOps() throws {
        let bar = try XCTUnwrap(NSApp.mainMenu)
        let titles = bar.items.map(\.title)
        print("MS1-MENUBAR \(titles)")
        let conversations = try XCTUnwrap(bar.item(withTitle: "Conversations")?.submenu)
        for item in conversations.items {
            print("MS1-MENU Conversations: \(item.isSeparatorItem ? "----" : item.title) "
                  + "\(Self.shortcut(item)) enabled=\(item.isEnabled)")
        }
        let shortcuts = Dictionary(conversations.items.filter { !$0.isSeparatorItem }
            .map { ($0.title, Self.shortcut($0)) }, uniquingKeysWith: { first, _ in first })
        XCTAssertEqual(shortcuts["Search Conversations"], "⌘F")
        // File > New Window holds ⌘N in the menu bar; New Chat answers ⌘N from its toolbar
        // button while the Chats tab is on screen, as it always has.
        XCTAssertEqual(shortcuts["New Chat"], "")
        XCTAssertEqual(bar.item(withTitle: "File")?.submenu?.items.first.map(Self.shortcut), "⌘N")
        XCTAssertEqual(shortcuts["Refresh"], "⌘R")
        XCTAssertEqual(shortcuts["Show Favorites"], "⇧⌘F")
        XCTAssertNotNil(shortcuts["Archive Selected"])

        let ops = try XCTUnwrap(bar.item(withTitle: "Ops")?.submenu)
        XCTAssertEqual(ops.items.first { $0.title.hasPrefix("Away Mode") }.map(Self.shortcut), "⇧⌘A",
                       "Away Mode keeps its shortcut")
        let conversationsIndex = try XCTUnwrap(titles.firstIndex(of: "Conversations"))
        let opsIndex = try XCTUnwrap(titles.firstIndex(of: "Ops"))
        XCTAssertLessThan(conversationsIndex, opsIndex,
                          "Conversations must be searched for ⇧⌘A before Ops")

        // Hiding the sidebar is still in the menu bar, with a shortcut, now that the toolbar
        // has no toggle for it.
        let everything = bar.items.compactMap(\.submenu).flatMap(\.items)
        let toggle = everything.first { $0.action == #selector(NSSplitViewController.toggleSidebar(_:)) }
        print("MS1-MENU sidebar toggle: \(toggle?.title ?? "none") \(toggle.map(Self.shortcut) ?? "")")
        XCTAssertNotNil(toggle, "nothing in the menu bar hides the sidebar")
        XCTAssertFalse(toggle.map(Self.shortcut)?.isEmpty ?? true, "the sidebar toggle has no shortcut")
    }

    /// Why Archive Selected attaches ⇧⌘A only while it can act: a DISABLED menu item still
    /// takes its shortcut, so an always attached one would take ⇧⌘A from Ops > Away Mode.
    func testADisabledMenuItemStillTakesItsShortcut() throws {
        final class Target: NSObject {
            var fired: [String] = []
            @objc func first(_ sender: Any?) { fired.append("first") }
            @objc func second(_ sender: Any?) { fired.append("second") }
        }
        let target = Target()
        let bar = NSMenu(title: "bar")
        bar.autoenablesItems = false
        for (title, action, enabled) in [("A", #selector(Target.first(_:)), false),
                                         ("B", #selector(Target.second(_:)), true)] {
            let top = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let menu = NSMenu(title: title)
            menu.autoenablesItems = false
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "a")
            item.keyEquivalentModifierMask = [.command, .shift]
            item.target = target
            item.isEnabled = enabled
            menu.addItem(item)
            top.submenu = menu
            bar.addItem(top)
        }
        let event = try XCTUnwrap(NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [.command, .shift], timestamp: 0,
            windowNumber: 0, context: nil, characters: "A", charactersIgnoringModifiers: "a",
            isARepeat: false, keyCode: 0))
        XCTAssertTrue(bar.performKeyEquivalent(with: event))
        XCTAssertEqual(target.fired, [], "the disabled item took the shortcut and did nothing")
    }

    // MARK: - Screenshots

    /// The PNGs for review, drawn from the window's frame view (toolbar, sidebar and detail),
    /// never from the screen. Only with `TEST_RUNNER_JESSE_SEARCH_SHOTS=1` on the
    /// `xcodebuild test` line; they land in `build/screenshots/ms1/`.
    func testScreenshots() async throws {
        guard ProcessInfo.processInfo.environment["JESSE_SEARCH_SHOTS"] == "1" else {
            throw XCTSkip("screenshots are drawn only with JESSE_SEARCH_SHOTS=1")
        }
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("build/screenshots/ms1", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        func shoot(_ window: NSWindow, _ name: String) throws {
            // Light: the cache draws the sidebar's material as plain white, under which the
            // dark appearance's white row text would vanish.
            window.appearance = NSAppearance(named: .aqua)
            window.contentView?.superview?.needsDisplay = true
            let frameView = try XCTUnwrap(window.contentView?.superview)
            frameView.layoutSubtreeIfNeeded()
            frameView.display()
            let rep = try XCTUnwrap(frameView.bitmapImageRepForCachingDisplay(in: frameView.bounds))
            frameView.cacheDisplay(in: frameView.bounds, to: rep)
            // The frame view's cache leaves the sidebar's list blank (its rows sit over the
            // sidebar material, which the cache draws opaque), so the list is cached on its
            // own and drawn over the same spot.
            if let table = Self.sidebarTable(in: window), let clip = table.enclosingScrollView,
               let listRep = clip.bitmapImageRepForCachingDisplay(in: clip.bounds),
               let context = NSGraphicsContext(bitmapImageRep: rep) {
                clip.cacheDisplay(in: clip.bounds, to: listRep)
                NSGraphicsContext.saveGraphicsState()
                NSGraphicsContext.current = context
                for row in 0..<table.numberOfRows {
                    guard let rowView = table.rowView(atRow: row, makeIfNecessary: true),
                          let rowRep = rowView.bitmapImageRepForCachingDisplay(in: rowView.bounds)
                    else { continue }
                    rowView.alphaValue = 1
                    rowView.layoutSubtreeIfNeeded()
                    rowView.cacheDisplay(in: rowView.bounds, to: rowRep)
                    let target = rowView.convert(rowView.bounds, to: frameView)
                    guard clip.convert(clip.bounds, to: frameView).intersects(target) else { continue }
                    rowRep.draw(in: target, from: .zero, operation: .sourceOver, fraction: 1,
                                respectFlipped: true, hints: nil)
                }
                NSGraphicsContext.restoreGraphicsState()
                print("MS1-SHOT rows composited: \(table.numberOfRows), first row alpha "
                      + "\(table.rowView(atRow: 0, makeIfNecessary: false)?.alphaValue ?? -1), "
                      + "layer opacity \(table.rowView(atRow: 0, makeIfNecessary: false)?.layer?.opacity ?? -1)")
            }
            let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
            let url = directory.appendingPathComponent(name)
            try png.write(to: url)
            print("MS1-SHOT \(url.path)")
        }

        let chats = try await hostShell(windowWidth: 1_600, seed: Self.seedConversations)
        chats.window.appearance = NSAppearance(named: .aqua)
        await setSidebarWidth(300, in: chats.window)
        try shoot(chats.window, "1-chats-ideal-sidebar.png")

        await setSidebarWidth(240, in: chats.window)
        try shoot(chats.window, "2-chats-narrowest-sidebar.png")

        await setSidebarWidth(300, in: chats.window)
        let field = try XCTUnwrap(Self.field(SearchFieldDriving.prompt, in: chats.window)).field
        chats.commands.requestSearch()
        await settle(chats.window) { Self.hasCaret(field, in: chats.window) }
        let editor = try XCTUnwrap(chats.window.firstResponder as? NSTextView)
        editor.insertText("kiln", replacementRange: editor.selectedRange())
        let table = try XCTUnwrap(Self.sidebarTable(in: chats.window))
        await settle(chats.window) { table.numberOfRows >= 3 && table.numberOfRows < 8 }
        await settle(chats.window, rounds: 10)
        print("MS1-SHOT live search rows: \(table.numberOfRows)")
        try shoot(chats.window, "3-live-search-kiln.png")

        let vault = try await hostShell(windowWidth: 1_600, tab: .vault)
        vault.window.appearance = NSAppearance(named: .aqua)
        await settle(vault.window)
        try shoot(vault.window, "4-vault-search.png")
    }
}
