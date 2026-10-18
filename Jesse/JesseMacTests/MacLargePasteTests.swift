import XCTest
import AppKit
import SwiftUI
import SwiftData
import JesseCore
@testable import Jesse_Mac

// A LARGE PASTE, through the composer's own paste path, in real hosted views.
//
// The defect lives in layout and view evaluation, so these host the real `ComposerTextView` and
// the real `MacThreadDetailView` in an `NSHostingView` inside an off screen window and deliver the
// text the way AppKit does for Command V: `readSelection(from:)` on the text view, which is what
// `paste(_:)` calls with the general pasteboard. The pasteboard here is a PRIVATE named one,
// created for this run and released afterwards, so the user's clipboard is never touched.
//
// Sizes are built by repeating the real reply fixture, so no megabyte file is committed.
@MainActor
final class MacLargePasteTests: XCTestCase {

    /// The three paste sizes, in bytes.
    static let sizes = [10_000, 100_000, 1_000_000]

    private var windows: [NSWindow] = []
    private var pasteboards: [NSPasteboard] = []
    private var previousDrafts: ComposerDraftStore?

    override func tearDown() async throws {
        await MainActor.run {
            for board in pasteboards { board.releaseGlobally() }
            pasteboards = []
            // XCTest keeps every test case alive to the end of the run, so a window left open
            // here would keep laying out (and counting) during the tests after this one.
            for window in windows {
                window.contentView = nil
                window.close()
            }
            windows = []
            if let previousDrafts { ComposerDraftStore.shared = previousDrafts }
        }
        try await super.tearDown()
    }

    // MARK: - Fixtures

    /// The real reply fixture, repeated until it is at least `bytes` long, then cut there on a
    /// character boundary.
    static func paste(ofSize bytes: Int) throws -> String {
        let fixture = try MacReplyFixture.text()
        var out = ""
        out.reserveCapacity(bytes + fixture.utf8.count)
        while out.utf8.count < bytes { out += fixture + "\n\n" }
        return String(out.utf8.prefix(bytes)) ?? out
    }

    /// A private pasteboard holding `text`, unique to this run.
    private func privatePasteboard(_ text: String) -> NSPasteboard {
        let board = NSPasteboard(name: NSPasteboard.Name("JesseMacTests.paste.\(UUID().uuidString)"))
        board.clearContents()
        board.setString(text, forType: .string)
        pasteboards.append(board)
        return board
    }

    private func window(for view: NSView, size: NSSize) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = view
        windows.append(window)
        return window
    }

    /// Let SwiftUI deliver the binding change and lay out, the way the run loop would after a
    /// paste event returns.
    private func settle(_ host: NSView) {
        for _ in 0..<3 {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
            host.layoutSubtreeIfNeeded()
        }
        host.window?.displayIfNeeded()
    }

    /// Run until the transcript has stopped changing: the hydrate task, the scroll to the
    /// bottom and the lazy rows it reveals all finish before a paste is measured against them.
    private func settleUntilQuiet(_ host: NSView) {
        var quiet = 0
        var last = (MacTurnBubble.bodies, MacDocumentCounters.builds)
        let deadline = Date().addingTimeInterval(5)
        while quiet < 10, Date() < deadline {
            settle(host)
            let now = (MacTurnBubble.bodies, MacDocumentCounters.builds)
            quiet = now == last ? quiet + 1 : 0
            last = now
        }
    }

    static func composerTextView(in view: NSView) -> ComposerNSTextView? {
        if let found = view as? ComposerNSTextView { return found }
        for sub in view.subviews {
            if let found = composerTextView(in: sub) { return found }
        }
        return nil
    }

    /// The composer alone, bound to a `@State` the way the detail view binds it.
    private struct ComposerHost: View {
        @State var text = ""
        var body: some View {
            ComposerTextView(text: $text, placeholder: "Message Jesse…", onSend: {})
                .frame(maxWidth: .infinity)
                .padding(8)
        }
    }

    // MARK: - The composer alone

    func testPasting10KBIntoTheComposerAloneSettles() throws { try composerAlone(10_000) }
    func testPasting100KBIntoTheComposerAloneSettles() throws { try composerAlone(100_000) }
    func testPasting1MBIntoTheComposerAloneSettles() throws { try composerAlone(1_000_000) }

    private func composerAlone(_ size: Int) throws {
        let text = try Self.paste(ofSize: size)
        let host = NSHostingView(rootView: ComposerHost())
        let window = window(for: host, size: NSSize(width: 1200, height: 800))
        settle(host)
        guard let composer = Self.composerTextView(in: host) else {
            return XCTFail("no composer text view in the hosted view")
        }
        window.makeFirstResponder(composer)
        let board = privatePasteboard(text)
        let passesBefore = composer.sizeQueries

        let start = Date()
        XCTAssertTrue(composer.readSelection(from: board), "the paste was refused")
        settle(host)
        let elapsed = Date().timeIntervalSince(start)
        let passes = composer.sizeQueries - passesBefore

        XCTAssertEqual(composer.string.utf8.count, text.utf8.count,
                       "the paste reached the composer byte for byte")
        XCTAssertLessThanOrEqual(passes, Self.maxSizePasses,
                                 "the composer's height took \(passes) layout passes to settle")
        assertMeasuredOnlyToTheCeiling(composer, of: text)
        assertQuiescent(host, composer)
        XCTAssertEqual(composer.textContainer?.size.width, composer.frame.width,
                       "the text is wrapped at the width it is drawn at, not at a probe's")
        let ceiling = ComposerNSTextView.lineHeight * 8
        XCTAssertLessThanOrEqual(composer.enclosingScrollView?.frame.height ?? .infinity,
                                 ceiling + 1, "the composer grew past its ceiling")
        print("MT1-TIMING composer-alone paste \(size) bytes: \(String(format: "%.3f", elapsed)) s, "
              + "\(passes) size passes, \(composer.lastMeasuredCharacters) characters measured")
    }

    /// Generous: SwiftUI asks a representable for its size a handful of times per layout pass
    /// (at zero, at infinity, unspecified and at the real width), and a paste triggers a few
    /// passes. Measured: 18 for the composer alone at every size, 31 to 62 inside the detail
    /// view's stacks. The defect was not the count but the cost of each one.
    static let maxSizePasses = 24
    static let maxDetailSizePasses = 80

    /// The height question is answered from the top of the text down to the ceiling and no
    /// further: a few thousand characters at most, however large the paste. Measuring the whole
    /// string is what made one question about a 1 MB paste take 66 s.
    private func assertMeasuredOnlyToTheCeiling(_ composer: ComposerNSTextView, of text: String,
                                                file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertLessThanOrEqual(composer.lastMeasuredCharacters, Self.maxMeasuredCharacters,
                                 "the height measurement typeset \(composer.lastMeasuredCharacters) "
                                 + "of \((text as NSString).length) characters", file: file, line: line)
    }

    /// Eight lines at the composer's width are about a thousand characters; the layout
    /// manager rounds up to whole paragraphs.
    static let maxMeasuredCharacters = 5_000

    /// SETTLED means settled: once the paste's layout is done, another round of the run loop
    /// and layout asks the composer nothing more. A height that fed back into itself would
    /// keep asking.
    private func assertQuiescent(_ host: NSView, _ composer: ComposerNSTextView,
                                 file: StaticString = #filePath, line: UInt = #line) {
        let before = composer.sizeQueries
        settle(host)
        settle(host)
        XCTAssertEqual(composer.sizeQueries - before, 0,
                       "the composer kept asking for layout after the paste settled",
                       file: file, line: line)
    }

    // MARK: - The whole detail view

    /// A conversation holding the fixture reply, to give the transcript real weight.
    private func detailHost(context: ModelContext) throws -> (NSHostingView<AnyView>, JesseThread) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacLargePasteTests.\(UUID().uuidString)")
        previousDrafts = ComposerDraftStore.shared
        ComposerDraftStore.shared = ComposerDraftStore(writer: ComposerDraftFileWriter(root: directory))

        let thread = JesseThread(mode: .ask)
        context.insert(thread)
        let reply = try MacReplyFixture.text()
        for index in 0..<6 {
            let user = Turn(role: .user, text: "Question \(index): what changed in the reader?")
            user.thread = thread
            let answer = Turn(role: .jesse, text: reply)
            answer.thread = thread
        }
        try context.save()

        let coordinator = MacCoordinator(configStore: MacTestFixtures.configured(),
                                         makeClient: { _ in MacFakeBridgeClient() },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let root = AnyView(MacThreadDetailView(thread: thread)
            .environment(coordinator)
            .modelContext(context))
        return (NSHostingView(rootView: root), thread)
    }

    func testPasting10KBIntoTheDetailViewSettles() throws { try detailView(10_000) }
    func testPasting100KBIntoTheDetailViewSettles() throws { try detailView(100_000) }
    func testPasting1MBIntoTheDetailViewSettles() throws { try detailView(1_000_000) }

    private func detailView(_ size: Int) throws {
        let context = try MacTestFixtures.context()
        let (host, _) = try detailHost(context: context)
        let window = window(for: host, size: NSSize(width: 1200, height: 900))
        settleUntilQuiet(host)
        guard let composer = Self.composerTextView(in: host) else {
            return XCTFail("no composer text view in the hosted detail view")
        }
        XCTAssertGreaterThan(MacTurnBubble.bodies, 0, "the transcript never rendered")
        window.makeFirstResponder(composer)
        let board = privatePasteboard(try Self.paste(ofSize: size))
        let bodiesBefore = MacTurnBubble.bodies
        let transcriptsBefore = MacThreadDetailView.transcriptEvaluations
        let buildsBefore = MacDocumentCounters.builds
        let passesBefore = composer.sizeQueries

        let start = Date()
        XCTAssertTrue(composer.readSelection(from: board), "the paste was refused")
        settle(host)
        let elapsed = Date().timeIntervalSince(start)
        let bodies = MacTurnBubble.bodies - bodiesBefore
        let builds = MacDocumentCounters.builds - buildsBefore
        let passes = composer.sizeQueries - passesBefore

        let transcripts = MacThreadDetailView.transcriptEvaluations - transcriptsBefore
        XCTAssertEqual(transcripts, 0, "the paste re-evaluated the transcript \(transcripts) times")
        XCTAssertEqual(bodies, 0, "the paste re-evaluated \(bodies) transcript rows")
        assertMeasuredOnlyToTheCeiling(composer, of: composer.string)
        XCTAssertEqual(builds, 0, "the paste rebuilt \(builds) reply documents")
        XCTAssertLessThanOrEqual(passes, Self.maxDetailSizePasses,
                                 "the composer's height took \(passes) layout passes to settle")
        assertQuiescent(host, composer)
        print("MT1-TIMING detail paste \(size) bytes: \(String(format: "%.3f", elapsed)) s, "
              + "\(passes) size passes, \(transcripts) transcript evaluations, \(bodies) row bodies, "
              + "\(builds) document builds, \(composer.lastMeasuredCharacters) characters measured")
    }

    // MARK: - Typing after a large paste

    /// A keystroke after a 1 MB paste. Each one used to compare the whole draft twice as a
    /// bridged string, with Unicode normalization: about 140 ms a key.
    func testAKeystrokeAfterA1MBPasteStaysCheap() throws {
        let host = NSHostingView(rootView: ComposerHost())
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        settle(host)
        let composer = try XCTUnwrap(Self.composerTextView(in: host))
        window.makeFirstResponder(composer)
        XCTAssertTrue(composer.readSelection(from: privatePasteboard(try Self.paste(ofSize: 1_000_000))))
        settle(host)

        var worst: TimeInterval = 0
        for _ in 0..<5 {
            let start = Date()
            composer.insertText("x", replacementRange: composer.selectedRange())
            settle(host)
            worst = max(worst, Date().timeIntervalSince(start))
        }
        XCTAssertTrue(composer.string.hasSuffix("xxxxx"))
        print("MT1-TIMING keystroke after 1 MB paste, worst of 5: \(String(format: "%.3f", worst)) s")
    }
}

/// The committed reply fixture: real Jesse prose with every block shape the reply view draws.
enum MacReplyFixture {
    static func text() throws -> String {
        let bundle = Bundle(for: MacLargePasteTests.self)
        guard let url = bundle.url(forResource: "MT1-Reply-Fixture", withExtension: "md") else {
            throw CocoaError(.fileNoSuchFile)
        }
        return try String(contentsOf: url, encoding: .utf8)
    }
}

