import XCTest
import AppKit
import SwiftUI
import SwiftData
import JesseCore
@testable import Jesse_Mac

/// Screenshots of the real transcript views against the real reply fixture, rendered off
/// screen: an `NSHostingView` in a window placed off every display, drawn with
/// `cacheDisplay(in:to:)`. Nothing is captured from the screen and no permission is asked for.
///
/// A new file because none of the others renders images. Skipped unless the run asks for it
/// (`TEST_RUNNER_MT1_SCREENSHOTS=1` on the `xcodebuild` command line), so the gate writes no
/// files; the PNGs land in `build/screenshots/mt1/` at the top of the checkout.
@MainActor
final class MacTranscriptScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var previousDrafts: ComposerDraftStore?

    override func setUp() async throws {
        try await super.setUp()
        guard ProcessInfo.processInfo.environment["MT1_SCREENSHOTS"] == "1" else {
            throw XCTSkip("screenshots are taken only when MT1_SCREENSHOTS=1")
        }
    }

    override func tearDown() async throws {
        await MainActor.run {
            for window in windows {
                window.contentView = nil
                window.close()
            }
            windows = []
            if let previousDrafts { ComposerDraftStore.shared = previousDrafts }
        }
        try await super.tearDown()
    }

    private static let width: CGFloat = 1200

    private var directory: URL {
        // JesseMacTests/<this file> -> the checkout's top level.
        URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("build/screenshots/mt1", isDirectory: true)
    }

    /// The fixture's sections that carry every block shape in a screen's worth: the App 1.0
    /// (187) section (a numbered list, bullets, inline code and a table) and the fence shapes.
    private func screenReply() throws -> String {
        let fixture = try MacReplyFixture.text()
        let ns = fixture as NSString
        let section = ns.range(of: "## [App 1.0 (187)]")
        let sectionEnd = ns.range(of: "Debug build, iPhone simulator")
        let fences = ns.range(of: "### Fence shapes the changelog does not carry")
        guard section.location != NSNotFound, sectionEnd.location != NSNotFound,
              fences.location != NSNotFound else { throw CocoaError(.fileReadCorruptFile) }
        return ns.substring(with: NSRange(location: section.location,
                                          length: sectionEnd.location - section.location))
            + "\n" + ns.substring(from: fences.location)
    }

    /// Host `view` at the shot width in a window off every display, made key when asked.
    private func hosted<V: View>(_ view: V, height: CGFloat? = nil,
                                 key: Bool = false) -> NSHostingView<V> {
        let host = NSHostingView(rootView: view)
        let fitting = height ?? max(host.fittingSize.height, 100)
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: Self.width,
                                                  height: fitting),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        if key {
            // Key only when the test host is the active app, which macOS does not let an app
            // make itself with nobody at the keyboard: an unattended run draws the selection
            // in its inactive colour. The window stays off every display either way.
            window.makeKeyAndOrderFront(nil)
        }
        for _ in 0..<5 {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
            host.layoutSubtreeIfNeeded()
        }
        if height == nil {
            let settled = max(host.fittingSize.height, 100)
            window.setContentSize(NSSize(width: Self.width, height: settled))
            host.layoutSubtreeIfNeeded()
        }
        return host
    }

    private func write(_ view: NSView, _ name: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        view.layoutSubtreeIfNeeded()
        view.display()
        let bounds = view.bounds
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: bounds))
        view.cacheDisplay(in: bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = directory.appendingPathComponent(name)
        try png.write(to: url)
        print("MT1-SHOT \(url.path) \(Int(bounds.width))x\(Int(bounds.height))")
    }

    private func page<V: View>(_ content: V) -> some View {
        content
            .padding(20)
            .frame(width: Self.width, alignment: .topLeading)
            .background(Color(nsColor: .windowBackgroundColor))
    }

    // 1. A reply with the fenced blocks and inline code.
    func testShot1Reply() throws {
        let host = hosted(page(MacTurnBubble(turn: Turn(role: .jesse, text: try screenReply()))))
        try write(host, "1-reply-code.png")
    }

    // 2. The same reply, a selection from mid first paragraph into the code block, window key.
    func testShot2Selection() throws {
        let host = hosted(page(MacTurnBubble(turn: Turn(role: .jesse, text: try screenReply()))),
                          key: true)
        let view = try XCTUnwrap(MacReplySelectionTests.documentViews(in: host).first)
        let ns = view.string as NSString
        let start = ns.range(of: "on-device expansion")
        let end = ns.range(of: "print(blocks.count)")
        XCTAssertNotEqual(start.location, NSNotFound)
        XCTAssertNotEqual(end.location, NSNotFound)
        view.window?.makeFirstResponder(view)
        view.setSelectedRange(NSRange(location: start.location,
                                      length: end.location + 5 - start.location))
        print("MT1-SHOT window key: \(host.window?.isKeyWindow == true), app active: \(NSApp.isActive)")
        try write(host, "2-reply-selection.png")
    }

    // 3. A user bubble holding a pasted fence, and a folded long user turn.
    func testShot3UserBubbles() throws {
        let pasted = "This fails to compile, why?\n\n```swift\nlet blocks = parseMarkdownBlocks(reply)\nprint(blocks.count)\n```\n\nThe `blocks` value is never used."
        let fixture = try MacReplyFixture.text()
        let long = Array(repeating: fixture, count: 3).joined(separator: "\n\n")
        let host = hosted(page(VStack(alignment: .leading, spacing: 14) {
            MacTurnBubble(turn: Turn(role: .user, text: pasted))
            MacTurnBubble(turn: Turn(role: .user, text: long))
        }))
        try write(host, "3-user-bubbles.png")
    }

    // 4. The composer holding the 1 MB paste.
    func testShot4Composer() throws {
        struct Composer: View {
            @State var text: String
            var body: some View {
                ComposerTextView(text: $text, placeholder: "Message Jesse…", onSend: {})
                    .frame(maxWidth: .infinity)
                    .padding(8)
                    .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 8))
                    .padding(12)
            }
        }
        let host = hosted(Composer(text: try MacLargePasteTests.paste(ofSize: 1_000_000))
            .frame(width: Self.width).background(Color(nsColor: .windowBackgroundColor)))
        let composer = try XCTUnwrap(MacLargePasteTests.composerTextView(in: host))
        composer.scrollToEndOfDocument(nil)
        try write(host, "4-composer-1mb.png")
    }

    // 5. The whole detail view: transcript and composer.
    func testShot5DetailView() throws {
        let scratch = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacTranscriptScreenshotTests.\(UUID().uuidString)")
        previousDrafts = ComposerDraftStore.shared
        ComposerDraftStore.shared = ComposerDraftStore(writer: ComposerDraftFileWriter(root: scratch))
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask)
        context.insert(thread)
        let fixture = try MacReplyFixture.text()
        let texts: [(TurnRole, String)] = [
            (.user, "What changed in the reader and the search this week?"),
            (.jesse, try screenReply()),
            (.user, Array(repeating: fixture, count: 3).joined(separator: "\n\n")),
        ]
        for (index, (role, text)) in texts.enumerated() {
            let turn = Turn(role: role, text: text,
                            createdAt: Date(timeIntervalSince1970: 1_800_000_000 + Double(index)))
            turn.thread = thread
        }
        try context.save()
        let coordinator = MacCoordinator(configStore: MacTestFixtures.configured(),
                                         makeClient: { _ in MacFakeBridgeClient() },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let host = hosted(MacThreadDetailView(thread: thread)
            .environment(coordinator)
            .modelContext(context)
            .frame(width: Self.width, height: 900), height: 900)
        try write(host, "5-detail-view.png")
    }
}
