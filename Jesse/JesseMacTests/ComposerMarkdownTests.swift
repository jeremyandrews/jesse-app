import XCTest
import AppKit
import SwiftUI
import SwiftData
import JesseCore
@testable import Jesse_Mac

/// Live Markdown styling in the composer: the scanner as a table, then the real composer
/// hosted off screen, proving the styling is display only and the caret stays in view.
///
/// A new file because `ComposerMarkdown` is a new source file with no test home, and the
/// composer's existing suites (`ComposerTextViewTests`, `ComposerKeyActionTests`) are meant to
/// pass unchanged as the proof that nothing they cover moved.
@MainActor
final class ComposerMarkdownTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var pasteboards: [NSPasteboard] = []
    private var previousDrafts: ComposerDraftStore?

    override func tearDown() async throws {
        await MainActor.run {
            for board in pasteboards { board.releaseGlobally() }
            pasteboards = []
            for window in windows {
                window.contentView = nil
                window.close()
            }
            windows = []
            if let previousDrafts { ComposerDraftStore.shared = previousDrafts }
        }
        try await super.tearDown()
    }

    // MARK: - The scanner

    /// Each span as `style:text@location`, in scan order.
    private func described(_ text: String) -> [String] {
        let ns = text as NSString
        return ComposerMarkdown.scan(text).map {
            "\($0.style):\(ns.substring(with: $0.range))@\($0.range.location)"
        }
    }

    func testTheScannerTable() {
        let table: [(String, [String], String)] = [
            ("a `b` c", ["code:`b`@2", "marker:`@2", "marker:`@4"], "single backtick span"),
            ("x ``a ` b`` y", ["code:``a ` b``@2", "marker:``@2", "marker:``@9"],
             "double backtick span holding a single backtick"),
            ("a ` b", [], "an unclosed backtick is literal"),
            ("```\nx `y` z\n```", ["fence:```\nx `y` z\n```@0", "marker:```@0", "marker:```@12"],
             "a backtick inside a fence is not a span"),
            ("```\ncode\n```", ["fence:```\ncode\n```@0", "marker:```@0", "marker:```@9"],
             "three backtick fence"),
            ("````\n```\nstill\n````", ["fence:````\n```\nstill\n````@0", "marker:````@0", "marker:````@15"],
             "a shorter run inside a four backtick fence is content"),
            ("~~~\ncode\n~~~", ["fence:~~~\ncode\n~~~@0", "marker:~~~@0", "marker:~~~@9"],
             "tilde fence"),
            ("```\n```swift\nbody\n```", ["fence:```\n```swift\nbody\n```@0", "marker:```@0", "marker:```@18"],
             "an info string line cannot close a fence"),
            ("```\nunclosed\nrest", ["fence:```\nunclosed\nrest@0", "marker:```@0"],
             "an unclosed fence runs to the end"),
            ("a **bold** b", ["bold:**bold**@2", "marker:**@2", "marker:**@8"], "bold"),
            ("*one* and _two_", ["italic:*one*@0", "marker:*@0", "marker:*@4",
                                 "italic:_two_@10", "marker:_@10", "marker:_@14"], "italic, both markers"),
            ("`**not bold**` and `_x_`", ["code:`**not bold**`@0", "marker:`@0", "marker:`@13",
                                          "code:`_x_`@19", "marker:`@19", "marker:`@23"],
             "emphasis markers inside a code span are not emphasis"),
            ("**a `code` b**", ["bold:**a `code` b**@0", "marker:**@0", "code:`code`@4", "marker:`@4",
                                "marker:`@9", "marker:**@12"], "a code span inside bold"),
            ("snake_case_name", [], "an underscore inside a word is not emphasis"),
            ("2 * 3 * 4", [], "a spaced asterisk is not emphasis"),
        ]
        for (input, expected, name) in table {
            XCTAssertEqual(described(input).sorted(), expected.sorted(), name)
        }
    }

    func testTheReportedMessageScansAsTheBubbleRendersIt() throws {
        let message = try MT3Fixture.message()
        XCTAssertEqual(described(message).sorted(), [
            "code:`TEST`@11", "marker:`@11", "marker:`@16",
            "fence:```\nTest test test\n```@40", "marker:```@40", "marker:```@59",
        ].sorted())
    }

    // MARK: - The real composer

    /// The draft as the app holds it: an observed object the binding reads and writes.
    @Observable
    final class Draft {
        var text = ""
        var sent: [String] = []
    }

    private struct Host: View {
        let draft: Draft
        var body: some View {
            ComposerTextView(text: Binding(get: { draft.text }, set: { draft.text = $0 }),
                             placeholder: "Message Jesse…",
                             onSend: { draft.sent.append(draft.text) })
                .frame(maxWidth: .infinity)
                .padding(8)
        }
    }

    private func hosted<V: View>(_ view: V, size: NSSize = NSSize(width: 1200, height: 400))
        -> NSHostingView<V> {
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: NSRect(origin: NSPoint(x: -20_000, y: -20_000), size: size),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        settle(host)
        return host
    }

    private func settle(_ host: NSView) {
        for _ in 0..<4 {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
            host.layoutSubtreeIfNeeded()
        }
        host.window?.displayIfNeeded()
    }

    private func composer(in host: NSView) throws -> ComposerNSTextView {
        let view = try XCTUnwrap(MacLargePasteTests.composerTextView(in: host))
        host.window?.makeFirstResponder(view)
        return view
    }

    private func board(_ text: String) -> NSPasteboard {
        let board = NSPasteboard(name: NSPasteboard.Name("JesseMacTests.md.\(UUID().uuidString)"))
        board.clearContents()
        board.setString(text, forType: .string)
        pasteboards.append(board)
        return board
    }

    /// Typed character by character and pasted whole, the fixture arrives byte for byte in
    /// the text view, the binding and the send; the styling it shows changes none of them.
    func testStylingNeverChangesTheText() throws {
        let fixture = try MT3Fixture.message()
        let draft = Draft()
        let host = hosted(Host(draft: draft))
        let view = try composer(in: host)

        for character in fixture {
            view.insertText(String(character), replacementRange: view.selectedRange())
        }
        settle(host)
        XCTAssertEqual(view.string, fixture)
        XCTAssertEqual(draft.text, fixture)
        XCTAssertFalse(view.markdownStyler.codeSpans.isEmpty, "nothing was styled, so nothing was proven")
        let code = (view.string as NSString).range(of: "`TEST`")
        let font = view.textStorage?.attribute(.font, at: code.location + 1, effectiveRange: nil) as? NSFont
        XCTAssertTrue(font?.fontDescriptor.symbolicTraits.contains(.monoSpace) == true)

        // What a drag, Copy or Services hands out is the string alone.
        view.selectAll(nil)
        let out = board("")
        XCTAssertTrue(view.writeSelection(to: out, types: view.writablePasteboardTypes))
        XCTAssertEqual(out.string(forType: .string), fixture)
        XCTAssertNil(out.data(forType: .rtf), "the plain text composer put rich text on the pasteboard")

        // Pasted whole over the selection, then sent with Return.
        XCTAssertTrue(view.readSelection(from: board(fixture)))
        settle(host)
        XCTAssertEqual(view.string, fixture)
        XCTAssertEqual(draft.text, fixture)
        view.keyDown(with: NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
                                            timestamp: 0, windowNumber: 0, context: nil,
                                            characters: "\r", charactersIgnoringModifiers: "\r",
                                            isARepeat: false, keyCode: ComposerKeyCode.return)!)
        XCTAssertEqual(draft.sent.last.map { Data($0.utf8) }, Data(fixture.utf8))
    }

    /// What a reader sees, read off the pixels: the typed span sits on a chip that differs
    /// from the background beside it, and the fence's line from the line above it, in the light
    /// and the dark appearance whatever the app's own appearance is.
    func testTheTypedSpanAndFenceAreVisiblyStyled() throws {
        for appearance in [NSAppearance.Name.aqua, .darkAqua] {
            let draft = Draft()
            // An opaque page, so a pixel says what a reader sees rather than an alpha.
            let host = hosted(Host(draft: draft).background(Color(nsColor: .windowBackgroundColor)))
            host.window?.appearance = NSAppearance(named: appearance)
            let view = try composer(in: host)
            type(try MT3Fixture.message(), into: view, host: host)
            let rendered = try MT3Fixture.render(host, name: "composer-\(appearance.rawValue).png",
                                                 appearance: appearance)
            let ns = view.string as NSString
            let span = try XCTUnwrap(MT3Fixture.glyphRect(of: ns.range(of: "TEST"), in: view, relativeTo: host))
            let inside = try XCTUnwrap(rendered.background(in: span))
            let beside = try XCTUnwrap(rendered.background(in: NSRect(
                x: span.maxX + 12, y: span.minY, width: 40, height: span.height)))
            XCTAssertGreaterThanOrEqual(MT3Fixture.difference(inside, beside), MT3Fixture.visibleDifference,
                                        "\(appearance.rawValue): no chip behind the typed span (\(inside) vs \(beside))")
            let body = try XCTUnwrap(MT3Fixture.glyphRect(of: ns.range(of: "Test test test"), in: view, relativeTo: host))
            let fence = try XCTUnwrap(rendered.background(in: NSRect(x: body.maxX + 20, y: body.minY,
                                                                     width: 200, height: body.height)))
            let above = try XCTUnwrap(rendered.background(in: NSRect(x: body.maxX + 20, y: span.minY,
                                                                     width: 200, height: span.height)))
            XCTAssertGreaterThanOrEqual(MT3Fixture.difference(fence, above), 4,
                                        "\(appearance.rawValue): no tint behind the fence (\(fence) vs \(above))")
            print("MT3-PIXELS composer \(appearance.rawValue): chip \(inside) beside \(beside), fence \(fence) above \(above)")
        }
    }

    /// Undo and redo step over the edits exactly as on `main`, where the same sequence was run
    /// against the unstyled composer: the typed run and the paste after it are one undo step
    /// there too, and styling adds no step and leaves nothing behind to undo.
    func testUndoStepsOverEditsNotStyling() throws {
        let draft = Draft()
        let host = hosted(Host(draft: draft))
        let view = try composer(in: host)
        let undo = try XCTUnwrap(view.undoManager)

        view.insertText("Formatting `TEST`", replacementRange: view.selectedRange())
        settle(host)
        view.setSelectedRange(NSRange(location: (view.string as NSString).length, length: 0))
        XCTAssertTrue(view.readSelection(from: board(" and **bold**")))
        settle(host)
        XCTAssertEqual(view.string, "Formatting `TEST` and **bold**")

        undo.undo()
        settle(host)
        XCTAssertEqual(view.string, "", "undo did not step back exactly as on main")
        XCTAssertEqual(draft.text, "")
        XCTAssertFalse(undo.canUndo, "styling left an undo step behind")
        undo.redo()
        settle(host)
        XCTAssertEqual(view.string, "Formatting `TEST` and **bold**")
        XCTAssertEqual(draft.text, "Formatting `TEST` and **bold**")
        XCTAssertFalse(undo.canRedo)
        XCTAssertTrue(view.markdownStyler.isStyled, "redo left the text unstyled")
    }

    /// A keystroke restyles its own paragraph; one that opens a fence restyles the fence.
    func testARestyleCoversTheEditedParagraphAndAnyFenceItOpens() throws {
        let draft = Draft()
        let host = hosted(Host(draft: draft))
        let view = try composer(in: host)
        let first = "First paragraph with `code` in it."
        view.insertText(first + "\n\nSecond", replacementRange: view.selectedRange())
        view.insertText(" more", replacementRange: view.selectedRange())
        let restyled = view.markdownStyler.lastRestyled
        XCTAssertGreaterThanOrEqual(restyled.location, (first as NSString).length,
                                    "typing in the second paragraph restyled the first")

        view.setSelectedRange(NSRange(location: 0, length: 0))
        view.insertText("```\n", replacementRange: view.selectedRange())
        XCTAssertEqual(view.markdownStyler.lastRestyled.location, 0)
        XCTAssertEqual(NSMaxRange(view.markdownStyler.lastRestyled), (view.string as NSString).length,
                       "an opened fence that runs to the end did not restyle to the end")
        XCTAssertTrue(view.markdownStyler.codeSpans.isEmpty, "a span inside the new fence is still a span")
    }

    /// Past the limit there is no styling at all, and what there was is gone.
    func testPastTheLimitStylingIsDropped() throws {
        let draft = Draft()
        let host = hosted(Host(draft: draft))
        let view = try composer(in: host)
        view.insertText("Some `code` and **bold**\n", replacementRange: view.selectedRange())
        XCTAssertTrue(view.markdownStyler.isStyled)
        let big = String(repeating: "plain words and more `words`\n", count: 800)
        XCTAssertTrue(view.readSelection(from: board(big)))
        settle(host)
        XCTAssertGreaterThan((view.string as NSString).length, ComposerMarkdownStyler.characterLimit)
        XCTAssertFalse(view.markdownStyler.isStyled)
        XCTAssertTrue(view.markdownStyler.codeSpans.isEmpty)
        let storage = try XCTUnwrap(view.textStorage)
        storage.enumerateAttribute(.font, in: NSRange(location: 0, length: storage.length)) { value, _, stop in
            if (value as? NSFont) != ComposerNSTextView.composerFont {
                XCTFail("a styled face survived past the limit: \(String(describing: value))")
                stop.pointee = true
            }
        }
    }

    // MARK: - The caret stays in view

    /// The rect of the caret's line, in the text view.
    private func caretLineRect(_ view: ComposerNSTextView) throws -> NSRect {
        let manager = try XCTUnwrap(view.layoutManager)
        let container = try XCTUnwrap(view.textContainer)
        manager.ensureLayout(for: container)
        let caret = view.selectedRange().location
        var rect: NSRect
        if caret == (view.string as NSString).length, !manager.extraLineFragmentRect.isEmpty {
            rect = manager.extraLineFragmentRect
        } else {
            let glyph = manager.glyphIndexForCharacter(at: max(caret - 1, 0))
            rect = manager.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
        }
        rect.origin.x += view.textContainerOrigin.x
        rect.origin.y += view.textContainerOrigin.y
        return rect
    }

    private func key(_ characters: String, keyCode: UInt16 = 0,
                     _ modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0,
                         windowNumber: 0, context: nil, characters: characters,
                         charactersIgnoringModifiers: characters, isARepeat: false, keyCode: keyCode)!
    }

    /// Type `text` the way a person does: each character a key press through `keyDown`, each
    /// line break Shift plus Return, the run loop and layout settling between key presses.
    private func type(_ text: String, into view: ComposerNSTextView, host: NSView) {
        for character in text {
            if character == "\n" {
                view.keyDown(with: key("\r", keyCode: ComposerKeyCode.return, .shift))
            } else {
                view.keyDown(with: key(String(character)))
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.001))
            host.layoutSubtreeIfNeeded()
        }
        settle(host)
    }

    private func lines(_ count: Int) -> String {
        (1...count).map { "Line \($0) of the message, with `code` in it" }.joined(separator: "\n")
    }

    private func assertCaretVisible(_ view: ComposerNSTextView, _ name: String) throws {
        let caret = try caretLineRect(view)
        let visible = view.visibleRect
        XCTAssertTrue(visible.insetBy(dx: -1, dy: -1).contains(NSRect(x: visible.minX, y: caret.minY,
                                                                        width: 1, height: caret.height)),
                      "\(name): the caret line \(caret) is outside the visible rect \(visible)")
        let ceiling = ComposerNSTextView.lineHeight * 8
        XCTAssertLessThanOrEqual(view.enclosingScrollView?.frame.height ?? .infinity, ceiling + 1,
                                 "\(name): the composer grew past its ceiling")
    }

    /// The composer alone: three, five and ten lines typed, the caret on the last.
    func testTheCaretLineStaysVisibleInTheComposer() throws {
        for count in [3, 5, 10] {
            let draft = Draft()
            let host = hosted(Host(draft: draft))
            let view = try composer(in: host)
            type(lines(count), into: view, host: host)
            XCTAssertEqual(view.string, lines(count))
            try assertCaretVisible(view, "\(count) lines typed")

            // The same text pasted whole into an empty composer.
            view.selectAll(nil)
            view.delete(nil)
            settle(host)
            XCTAssertTrue(view.readSelection(from: board(lines(count))))
            settle(host)
            try assertCaretVisible(view, "\(count) lines pasted")
        }
    }

    /// The detail view, the smallest real view that stages attachments, with and without a
    /// staged image.
    func testTheCaretLineStaysVisibleWithAStagedAttachment() throws {
        for withImage in [false, true] {
            for count in [3, 5, 10] {
                let (host, view) = try detailComposer()
                if withImage {
                    XCTAssertTrue(view.readSelection(from: try imageBoard()), "the image was not staged")
                    settle(host)
                }
                type(lines(count), into: view, host: host)
                try assertCaretVisible(view, "\(count) lines typed, image \(withImage)")
                view.selectAll(nil)
                view.delete(nil)
                settle(host)
                XCTAssertTrue(view.readSelection(from: board(lines(count))))
                settle(host)
                try assertCaretVisible(view, "\(count) lines pasted, image \(withImage)")
            }
        }
    }

    func detailComposer() throws -> (NSView, ComposerNSTextView) {
        if previousDrafts == nil { previousDrafts = ComposerDraftStore.shared }
        let scratch = FileManager.default.temporaryDirectory
            .appendingPathComponent("ComposerMarkdownTests.\(UUID().uuidString)")
        ComposerDraftStore.shared = ComposerDraftStore(writer: ComposerDraftFileWriter(root: scratch))
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask)
        context.insert(thread)
        try context.save()
        let coordinator = MacCoordinator(configStore: MacTestFixtures.configured(),
                                         makeClient: { _ in MacFakeBridgeClient() },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let host = hosted(MacThreadDetailView(thread: thread)
            .environment(coordinator)
            .modelContext(context)
            .frame(width: 1200, height: 700), size: NSSize(width: 1200, height: 700))
        return (host, try composer(in: host))
    }

    /// A private pasteboard holding a small PNG.
    func imageBoard() throws -> NSPasteboard {
        let rep = try XCTUnwrap(NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 64, pixelsHigh: 48,
                                                 bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                                 isPlanar: false, colorSpaceName: .deviceRGB,
                                                 bytesPerRow: 0, bitsPerPixel: 0))
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSColor.systemOrange.setFill()
        NSRect(x: 0, y: 0, width: 64, height: 48).fill()
        NSGraphicsContext.restoreGraphicsState()
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let board = NSPasteboard(name: NSPasteboard.Name("JesseMacTests.image.\(UUID().uuidString)"))
        board.clearContents()
        board.setData(png, forType: .png)
        pasteboards.append(board)
        return board
    }

    // MARK: - A large paste inside a fence (MT5)

    /// Real code, `MacThreadDetailView.swift` (backticks, asterisks and underscores included),
    /// repeated or cut to `length` UTF-16 units (less only where that would split a character).
    static func code(length: Int) throws -> String {
        let source = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("JesseMac/MacThreadDetailView.swift")
        let file = try String(contentsOf: source, encoding: .utf8) as NSString
        let out = NSMutableString()
        while out.length < length {
            out.append(file.substring(to: min(file.length, length - out.length)))
        }
        // Never cut inside a composed character: back off to the boundary at or before it.
        let last = out.rangeOfComposedCharacterSequence(at: length - 1)
        return out.substring(to: NSMaxRange(last) <= length ? length : last.location)
    }

    /// A slice of this repository's CHANGELOG that holds fenced blocks of its own.
    static func changelogWithFences(length: Int) throws -> String {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let log = try String(contentsOf: root.appendingPathComponent("CHANGELOG.md"), encoding: .utf8) as NSString
        let first = log.range(of: "\n```")
        let start = first.location == NSNotFound ? 0 : max(first.location - 2_000, 0)
        let slice = log.substring(with: NSRange(location: start, length: min(length, log.length - start)))
        return slice
    }

    /// The composer opened on an empty fence, caret on its middle line.
    private func emptyFence() throws -> (NSView, ComposerNSTextView, Draft) {
        let draft = Draft()
        let host = hosted(Host(draft: draft).background(Color(nsColor: .windowBackgroundColor)),
                          size: NSSize(width: 1200, height: 400))
        let view = try composer(in: host)
        view.insertText("```\n\n```", replacementRange: view.selectedRange())
        view.setSelectedRange(NSRange(location: 4, length: 0))
        view.breakUndoCoalescing()
        settle(host)
        return (host, view, draft)
    }

    /// One draw of what `view` shows, the way the window draws it.
    private func draw(_ view: NSView, _ rect: NSRect) {
        guard let rep = view.bitmapImageRepForCachingDisplay(in: rect) else {
            return XCTFail("no bitmap to draw into")
        }
        view.cacheDisplay(in: rect, to: rep)
    }

    private static func ms(_ duration: Duration) -> Double {
        Double(duration.components.seconds) * 1_000 + Double(duration.components.attoseconds) / 1e15
    }

    /// Paste into the empty fence, settle, draw the top and the end, type a key, delete it.
    /// Returns (paste plus settle plus draw, keystroke plus restyle plus draw) in milliseconds.
    @discardableResult
    private func pasteIntoFence(_ paste: String, label: String) throws -> (Double, Double) {
        let (host, view, draft) = try emptyFence()
        let clock = ContinuousClock()
        let pasted = clock.measure {
            XCTAssertTrue(view.readSelection(from: board(paste)), "\(label): the paste was refused")
            settle(host)
            draw(view, view.visibleRect)
        }
        let expected = "```\n" + paste + "\n```"
        XCTAssertEqual(Data(view.string.utf8), Data(expected.utf8), "\(label): the text view changed the paste")
        XCTAssertEqual(Data(draft.text.utf8), Data(expected.utf8), "\(label): the binding changed the paste")

        view.scrollRangeToVisible(NSRange(location: (view.string as NSString).length, length: 0))
        draw(view, view.visibleRect)
        view.scrollRangeToVisible(NSRange(location: 4, length: 0))
        view.setSelectedRange(NSRange(location: 4, length: 0))
        settle(host)

        view.breakUndoCoalescing()
        let keyed = clock.measure {
            view.insertText("x", replacementRange: view.selectedRange())
            host.layoutSubtreeIfNeeded()
            draw(view, view.visibleRect)
        }
        XCTAssertEqual((view.string as NSString).length, (expected as NSString).length + 1)
        // Taken back with Delete, not Undo: without real events the test host's undo manager
        // groups the fence, the paste and the key as one step (see `testUndoStepsOverEditsNotStyling`).
        view.deleteBackward(nil)
        settle(host)
        XCTAssertEqual(Data(view.string.utf8), Data(expected.utf8), "\(label): deleting the key did not restore the paste")
        XCTAssertEqual(Data(draft.text.utf8), Data(expected.utf8), "\(label): the binding did not follow")
        let result = (Self.ms(pasted), Self.ms(keyed))
        print("MT5-TIMING \(label): paste+settle+draw \(String(format: "%.1f", result.0)) ms, "
              + "key+restyle+draw \(String(format: "%.1f", result.1)) ms, styled \(view.markdownStyler.isStyled)")
        return result
    }

    /// Every size, inside a fence: paste, draw the top and the end, one key, delete it. Sizes are
    /// the composer's whole length after the paste, fences included, so 19,999 is styled and
    /// 20,001 is not.
    func testALargePasteInsideAFenceStaysWithinBudget() throws {
        for size in [2_000, 10_000, 19_999, 20_001, 100_000, 1_000_000] {
            let (paste, key) = try pasteIntoFence(try Self.code(length: size - 8), label: "fence \(size)")
            switch size {
            case ...19_999:
                XCTAssertLessThan(paste, 300, "\(size): paste, settle and draw took \(paste) ms")
                XCTAssertLessThan(key, 50, "\(size): a keystroke took \(key) ms")
            default:
                XCTAssertLessThan(paste, 1_000, "\(size): paste, settle and draw took \(paste) ms")
            }
        }
        // A paste whose own text opens and closes fences.
        try pasteIntoFence(try Self.changelogWithFences(length: 15_000), label: "changelog 15000")
    }

    /// A paste that takes the composer past the limit drops the styling; a delete that brings
    /// it back under restores it.
    func testCrossingTheLimitAndComingBackRestyles() throws {
        let (host, view, draft) = try emptyFence()
        let paste = try Self.code(length: 20_001 - 8)
        XCTAssertTrue(view.readSelection(from: board(paste)))
        settle(host)
        XCTAssertFalse(view.markdownStyler.isStyled)
        view.setSelectedRange(NSRange(location: 4, length: 2))
        view.deleteBackward(nil)
        settle(host)
        draw(view, view.visibleRect)
        XCTAssertEqual((view.string as NSString).length, 19_999)
        XCTAssertEqual(draft.text, view.string)
        XCTAssertTrue(view.markdownStyler.isStyled, "back under the limit, the fence was not restyled")
        XCTAssertFalse(view.markdownStyler.fenceBlocks.isEmpty)
    }

    /// Drawing the fence tint lays out what is on screen, not the whole fence: under the
    /// limit, one draw of the top used to lay out all 19,999 characters.
    func testDrawingAFenceLaysOutOnlyWhatIsOnScreen() throws {
        let (host, view, _) = try emptyFence()
        XCTAssertTrue(view.readSelection(from: board(try Self.code(length: 19_999 - 8))))
        settle(host)
        XCTAssertTrue(view.markdownStyler.isStyled)
        let manager = try XCTUnwrap(view.layoutManager)
        let length = (view.string as NSString).length
        view.scrollRangeToVisible(NSRange(location: 0, length: 0))
        manager.invalidateLayout(forCharacterRange: NSRange(location: 0, length: length),
                                 actualCharacterRange: nil)
        draw(view, view.visibleRect)
        let laid = manager.firstUnlaidCharacterIndex()
        XCTAssertLessThan(laid, length / 4,
                          "one draw of the top of the fence laid out \(laid) of \(length) characters")
    }

    // MARK: - MT5 screenshots

    /// Where the MT5 PNGs land: `build/screenshots/mt5/` at the top of the checkout, written
    /// only when the run asks (`TEST_RUNNER_MT5_SCREENSHOTS=1`).
    private func shoot(_ host: NSView, _ name: String) throws {
        host.layoutSubtreeIfNeeded()
        host.display()
        let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        guard ProcessInfo.processInfo.environment["MT5_SCREENSHOTS"] == "1" else { return }
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("build/screenshots/mt5", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        try png.write(to: directory.appendingPathComponent(name))
    }

    func testMT5Screenshots() throws {
        let (host, view, _) = try emptyFence()
        XCTAssertTrue(view.readSelection(from: board(try Self.code(length: 19_999 - 8))))
        settle(host)
        view.scrollRangeToVisible(NSRange(location: 0, length: 0))
        settle(host)
        try shoot(host, "1-fence-19999-top.png")
        view.scrollRangeToVisible(NSRange(location: (view.string as NSString).length, length: 0))
        settle(host)
        try shoot(host, "2-fence-19999-end.png")

        let (bigHost, bigView, _) = try emptyFence()
        XCTAssertTrue(bigView.readSelection(from: board(try Self.code(length: 100_000 - 8))))
        settle(bigHost)
        bigView.scrollRangeToVisible(NSRange(location: 0, length: 0))
        settle(bigHost)
        try shoot(bigHost, "3-fence-100000-top.png")

        if previousDrafts == nil { previousDrafts = ComposerDraftStore.shared }
        let scratch = FileManager.default.temporaryDirectory
            .appendingPathComponent("ComposerMarkdownTests.\(UUID().uuidString)")
        ComposerDraftStore.shared = ComposerDraftStore(writer: ComposerDraftFileWriter(root: scratch))
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask)
        context.insert(thread)
        let block = try Self.code(length: 4_000)
        let user = Turn(role: .user, text: "Why does this hang when I paste it?\n\n```swift\n" + block + "\n```")
        user.thread = thread
        let answer = Turn(role: .jesse, text: try MT3Fixture.reply())
        answer.thread = thread
        try context.save()
        let coordinator = MacCoordinator(configStore: MacTestFixtures.configured(),
                                         makeClient: { _ in MacFakeBridgeClient() },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let detail = hosted(MacThreadDetailView(thread: thread)
            .environment(coordinator)
            .modelContext(context)
            .frame(width: 1200, height: 900)
            .background(Color(nsColor: .windowBackgroundColor)), size: NSSize(width: 1200, height: 900))
        let composer = try composer(in: detail)
        composer.insertText("Here is the fix, `measuredTextHeight` now **measures off to the side**:\n```swift\nlet height = measuredTextHeight(forWidth: 240)\n```",
                            replacementRange: composer.selectedRange())
        for _ in 0..<20 { settle(detail) }
        try shoot(detail, "4-detail-view.png")
    }
}
