import XCTest
import SwiftUI
@testable import JesseVault
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

// THE EDITOR OPENS WHERE THE READER WAS, AND THE READER COMES BACK TO WHERE THE EDIT WAS.
//
// Every claim here is one of three conversions, and each is asserted against text rather
// than a screen: a file line to a `NSRange` in the editor's own text, a block to the file
// lines it came from, and the editor's closing line back to a block of the reloaded note.
// The last group puts a real text view under `open(_:at:)`, which is the only part of this
// that a pure function cannot prove.
@MainActor
final class VaultEditorStartTests: XCTestCase {

    /// The substring a range selects.
    private func text(_ range: NSRange, in text: String) -> String {
        (text as NSString).substring(with: range)
    }

    // MARK: - Line to range

    func testLineOneIsTheTop() {
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 1), in: "a\nb\n"),
                       NSRange(location: 0, length: 0))
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 0), in: "a\nb\n"),
                       NSRange(location: 0, length: 0), "below 1 is the top, never a crash")
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 3), in: ""),
                       NSRange(location: 0, length: 0), "an empty note has one place to be")
    }

    func testAMiddleLineStartsAfterTheBreakAboveIt() {
        let note = "alpha\nbeta\ngamma\ndelta"
        let range = VaultNotePosition.range(for: .caret(line: 3), in: note)
        XCTAssertEqual(range, NSRange(location: 11, length: 0))
        XCTAssertTrue((note as NSString).substring(from: range.location).hasPrefix("gamma"))
    }

    func testTheLastLineWithAndWithoutATrailingNewline() {
        let bare = "alpha\nbeta\ngamma"
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 3), in: bare).location, 11)
        let ended = "alpha\nbeta\ngamma\n"
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 3), in: ended).location, 11)
        // The empty line after a final newline is a line: the caret goes to the very end.
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 4), in: ended).location, 17)
    }

    /// The note shrank on disk between the reader and the editor.
    func testALinePastTheEndClampsToTheLastLine() {
        let bare = "alpha\nbeta\ngamma"
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 400), in: bare).location, 11)
        XCTAssertEqual(VaultNotePosition.range(for: .select(lines: 300...400), in: bare),
                       NSRange(location: 11, length: 5))
        let ended = "alpha\nbeta\n"
        XCTAssertEqual(VaultNotePosition.range(for: .caret(line: 400), in: ended).location, 11)
    }

    /// UTF-16, not characters: 🏛 is two units and "è" (precomposed) one, and a count of
    /// `Character`s would land short by one per emoji.
    func testEmojiAndAccentsBeforeTheLineAreCountedInUTF16() {
        let note = "Però 🏛🏛\nCaffè è pronto\ntarget line"
        let range = VaultNotePosition.range(for: .caret(line: 3), in: note)
        XCTAssertEqual(range.location, (("Però 🏛🏛\nCaffè è pronto\n") as NSString).length)
        XCTAssertNotEqual(range.location, "Però 🏛🏛\nCaffè è pronto\n".count,
                          "a character count would be wrong here, which is the point")
        XCTAssertTrue((note as NSString).substring(from: range.location).hasPrefix("target"))
    }

    /// "\r\n" is one `Character`, so a character-wise split never sees a CRLF break. The
    /// text view keeps the file's CR (see `testTheTextViewKeepsCRLF`), so the editor's text
    /// has them and the line starts must step over them.
    func testCRLFLinesStartAfterTheLF() {
        let note = "alpha\r\nbeta\r\ngamma\r\n"
        let range = VaultNotePosition.range(for: .caret(line: 2), in: note)
        XCTAssertEqual(range.location, 7)
        XCTAssertTrue((note as NSString).substring(from: range.location).hasPrefix("beta"))
        // A selection of a CRLF line stops before its CR.
        let selected = VaultNotePosition.range(for: .select(lines: 2...3), in: note)
        XCTAssertEqual(text(selected, in: note), "beta\r\ngamma")
    }

    func testTheCaretLineRoundTrips() {
        let note = "---\ntitle: x\n---\n# Head\n\nPara 🏛 one\nstill para\n"
        for line in 1...8 {
            let offset = VaultNotePosition.range(for: .caret(line: line), in: note).location
            XCTAssertEqual(VaultNotePosition.line(atOffset: offset, in: note), line)
        }
        XCTAssertEqual(VaultNotePosition.line(atOffset: 10_000, in: note), 8,
                       "past the end is on the last line")
        let crlf = "a\r\nb\r\nc"
        XCTAssertEqual(VaultNotePosition.line(atOffset: 6, in: crlf), 3)
        XCTAssertEqual(VaultNotePosition.line(atOffset: 4, in: crlf), 2)
    }

    // MARK: - Block lines are file lines

    private let withFrontmatter = """
        ---
        title: Arch
        tags: [a, b]
        ---
        # The arch

        First paragraph, which is
        soft wrapped over two lines.

        ## Second heading

        - one
        - two
        """

    /// `VaultNoteBlock.line` counts the frontmatter, so the reader's line and the editor's
    /// line are the same number and no offset is applied anywhere.
    func testABlockLineIsAFileLineWithFrontmatter() throws {
        let document = VaultNoteDocument.parse(path: "A.md", text: withFrontmatter)
        XCTAssertFalse(document.frontmatter.isEmpty)
        let heading = try XCTUnwrap(document.blocks.first {
            if case .heading(2) = $0.kind { return true }
            return false
        })
        XCTAssertEqual(heading.line, 10)
        let range = VaultNotePosition.range(for: .caret(line: heading.line), in: withFrontmatter)
        XCTAssertTrue((withFrontmatter as NSString).substring(from: range.location)
                        .hasPrefix("## Second heading"))
        // The raw view's lines are the same file lines.
        XCTAssertEqual(document.rawLines[heading.line - 1], "## Second heading")
    }

    func testABlockLineIsAFileLineWithoutFrontmatter() throws {
        let note = "# The arch\n\nOne.\n\n## Second\n\nTwo.\n"
        let document = VaultNoteDocument.parse(path: "A.md", text: note)
        XCTAssertTrue(document.frontmatter.isEmpty)
        for block in document.blocks {
            let range = VaultNotePosition.range(for: .caret(line: block.line), in: note)
            let rest = (note as NSString).substring(from: range.location)
            XCTAssertEqual(rest.split(separator: "\n").first.map(String.init),
                           document.rawLines[block.line - 1])
        }
    }

    /// Edit on the reader opens at the top block's first line; the first block reads as
    /// the top of the file, so an unscrolled note opens with its frontmatter showing.
    func testTheReaderAnchorMapsToAFileLine() throws {
        let document = VaultNoteDocument.parse(path: "A.md", text: withFrontmatter)
        let second = document.blocks[1]
        XCTAssertEqual(VaultNoteReaderView.line(forAnchor: "block-\(second.id)", in: document),
                       second.line)
        XCTAssertEqual(VaultNoteReaderView.line(forAnchor: "block-0", in: document), 1)
        XCTAssertEqual(VaultNoteReaderView.line(forAnchor: "raw-7", in: document), 7)
        XCTAssertNil(VaultNoteReaderView.line(forAnchor: "block-999", in: document))
    }

    // MARK: - Edit here selects exactly the block's lines

    private let mixed = """
        # Notes

        - a list item that
          continues on a second line
        - the next item

        | Col | Other |
        | --- | ----- |
        | a   | b     |
        | c   | d     |

        ```swift
        let x = 1

        let y = 2
        ```

        Last paragraph.
        """

    private func block(_ document: VaultNoteDocument,
                       _ match: (VaultNoteBlock.Kind) -> Bool) throws -> VaultNoteBlock {
        try XCTUnwrap(document.blocks.first { match($0.kind) })
    }

    private func selection(of block: VaultNoteBlock, in document: VaultNoteDocument,
                           note: String) throws -> String {
        let lines = try XCTUnwrap(document.sourceLines(ofBlock: block.id))
        return text(VaultNotePosition.range(for: .select(lines: lines), in: note), in: note)
    }

    func testEditHereSelectsAListItemsLines() throws {
        let document = VaultNoteDocument.parse(path: "A.md", text: mixed)
        let item = try block(document) { if case .bullet = $0 { return true }; return false }
        XCTAssertEqual(try selection(of: item, in: document, note: mixed),
                       "- a list item that\n  continues on a second line")
    }

    func testEditHereSelectsATablesLines() throws {
        let document = VaultNoteDocument.parse(path: "A.md", text: mixed)
        let table = try block(document) { if case .table = $0 { return true }; return false }
        XCTAssertEqual(try selection(of: table, in: document, note: mixed),
                       "| Col | Other |\n| --- | ----- |\n| a   | b     |\n| c   | d     |")
    }

    /// The blank line INSIDE the fence is the block's own; only trailing blanks go.
    func testEditHereSelectsAFencedCodeBlocksLines() throws {
        let document = VaultNoteDocument.parse(path: "A.md", text: mixed)
        let code = try block(document) { $0 == .code }
        XCTAssertEqual(try selection(of: code, in: document, note: mixed),
                       "```swift\nlet x = 1\n\nlet y = 2\n```")
    }

    func testEditHereOnTheLastBlockStopsAtItsText() throws {
        let note = mixed + "\n\n"
        let document = VaultNoteDocument.parse(path: "A.md", text: note)
        let last = try XCTUnwrap(document.blocks.last)
        XCTAssertEqual(try selection(of: last, in: document, note: note), "Last paragraph.")
    }

    // MARK: - Coming back

    /// The reader returns to the block the caret is IN, resolved on the RELOADED document:
    /// the save added three lines above the caret, so its line number is the new note's.
    func testTheReturnTargetResolvesOnTheReloadedDocument() throws {
        let before = "# Top\n\nIntro.\n\n## Target\n\nBody line one\nbody line two\n\n## After\n"
        let after = "# Top\n\nIntro.\n\nAdded one.\n\nAdded two.\n\n## Target\n\nBody line one\nbody line two\n\n## After\n"
        let reloaded = VaultNoteDocument.parse(path: "A.md", text: after)
        // The caret on "body line two", as the editor reports it from its own text.
        let offset = (after as NSString).range(of: "body line two").location + 3
        let line = VaultNotePosition.line(atOffset: offset, in: after)
        XCTAssertEqual(line, 12)
        let anchor = try XCTUnwrap(VaultNoteReaderView.returnAnchor(forLine: line, in: reloaded,
                                                                     raw: false))
        let target = try XCTUnwrap(reloaded.blocks.first { anchor == "block-\($0.id)" })
        XCTAssertEqual(target.kind, .paragraph)
        XCTAssertEqual(target.line, 11, "the paragraph's first line, in the reloaded note")
        XCTAssertTrue(target.text.hasPrefix("Body line one"))
        // `blockID(forLine:)` (at or after) would have gone past the paragraph.
        XCTAssertNotEqual(VaultNoteDocument.blockID(forLine: line, in: reloaded.blocks), target.id)
        // And against the OLD document the same line is somewhere else entirely.
        let stale = VaultNoteDocument.parse(path: "A.md", text: before)
        XCTAssertNotEqual(VaultNoteReaderView.returnAnchor(forLine: line, in: stale, raw: false),
                          anchor)
    }

    func testTheReturnTargetInRawIsTheLineClamped() {
        let document = VaultNoteDocument.parse(path: "A.md", text: "a\nb\nc")
        XCTAssertEqual(VaultNoteReaderView.returnAnchor(forLine: 2, in: document, raw: true),
                       "raw-2")
        XCTAssertEqual(VaultNoteReaderView.returnAnchor(forLine: 90, in: document, raw: true),
                       "raw-3")
    }

    /// A caret in the frontmatter comes back to the first block, never to nothing.
    func testAFrontmatterLineReturnsToTheFirstBlock() {
        let document = VaultNoteDocument.parse(path: "A.md", text: withFrontmatter)
        XCTAssertEqual(VaultNoteDocument.blockID(containingLine: 2, in: document.blocks),
                       document.blocks.first?.id)
        XCTAssertNil(VaultNoteDocument.blockID(containingLine: 2, in: []))
    }

    @MainActor
    func testTheModelCarriesItsStartAndReportsTheCaretLine() async {
        let writer = FakeNoteWriter(text: "one\ntwo\nthree\n")
        let directory = VaultFixture.makeDirectory()
        defer { VaultFixture.cleanUp(directory) }
        let model = VaultNoteEditorModel(path: "A.md", start: .caret(line: 2), writer: writer,
                                         stash: VaultEditStash(directory: directory))
        await model.load()
        XCTAssertEqual(model.start, .caret(line: 2))
        XCTAssertEqual(model.caretLine(for: NSRange(location: 9, length: 0)), 3)
        XCTAssertEqual(model.caretLine(for: NSRange(location: 0, length: 0)), 1)
    }

    // MARK: - A real text view

    private var longNote: String {
        (1...300).map { "Line \($0) of a long note, long enough to be a real line." }
            .joined(separator: "\n") + "\n"
    }

    #if os(iOS)

    @MainActor
    private func hosted(_ text: String) -> (UIWindow, UITextView) {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 375, height: 667))
        let controller = UIViewController()
        let view = UITextView(frame: controller.view.bounds)
        view.font = .monospacedSystemFont(ofSize: UIFont.systemFontSize, weight: .regular)
        view.textContainerInset = UIEdgeInsets(top: 8, left: 8, bottom: 8, right: 8)
        view.text = text
        controller.view.addSubview(view)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        view.layoutIfNeeded()
        return (window, view)
    }

    /// The line goes to the TOP of the view, where the keyboard cannot cover it.
    @MainActor
    func testOpeningPutsTheLineAtTheTopOfAUITextView() throws {
        let (window, view) = hosted(longNote)
        defer { window.isHidden = true }
        let range = VaultPlainTextEditor.open(view, at: .caret(line: 150))
        // A turn of the run loop and a layout pass, as the app gets before anyone looks: the
        // line has to STAY where it was put, not only be there for one frame.
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        view.layoutIfNeeded()
        XCTAssertEqual(view.selectedRange, range)
        XCTAssertTrue(view.text.suffix(from: view.text.index(
            view.text.startIndex, offsetBy: range.location)).hasPrefix("Line 150 "))
        let caret = view.caretRect(for: try XCTUnwrap(view.selectedTextRange).start)
        let visibleTop = view.contentOffset.y + view.adjustedContentInset.top
        XCTAssertGreaterThanOrEqual(caret.minY, visibleTop - 1, "the line is on screen")
        XCTAssertLessThan(caret.minY - visibleTop, view.bounds.height / 4,
                          "and near the top, not at the bottom edge where the keyboard is")
    }

    @MainActor
    func testOpeningSelectsTheLinesInAUITextView() {
        let (window, view) = hosted(longNote)
        defer { window.isHidden = true }
        let range = VaultPlainTextEditor.open(view, at: .select(lines: 40...41))
        XCTAssertEqual(view.selectedRange, range)
        XCTAssertEqual((view.text as NSString).substring(with: range),
                       "Line 40 of a long note, long enough to be a real line.\n"
                       + "Line 41 of a long note, long enough to be a real line.")
    }

    /// The editor's text is the text view's text, and the text view keeps the file's CRs,
    /// so a line computed from the model's text is a line in the view.
    @MainActor
    func testTheTextViewKeepsCRLF() {
        let (window, view) = hosted("a\r\nb\r\nc")
        defer { window.isHidden = true }
        XCTAssertEqual(view.text, "a\r\nb\r\nc")
    }

    #elseif os(macOS)

    /// The same claim on the Mac, in a window, through the same scroll view the editor
    /// builds. Runs on a macOS 26 test host only.
    @MainActor
    func testOpeningPutsTheLineAtTheTopOfAnNSTextView() throws {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 400),
                              styleMask: [.titled], backing: .buffered, defer: false)
        // A programmatically created window RELEASES ITSELF ON CLOSE, which under ARC is
        // an over-release: the test still holds it. It does not fail here — it corrupts
        // the process and blows up later, in `objc_autoreleasePoolPop` at the end of some
        // unrelated test, which is what `swift test` reported as a signal 11 in
        // `VaultBrowserModelTests` or `StrandMenuTests` depending on the run. Own it here
        // instead. `ComposerTextViewTests` carries the same line for the same reason.
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let made = NSTextView.scrollableTextView()
        let view = try XCTUnwrap(made.documentView as? NSTextView)
        made.documentView = nil
        let scroll = VaultPlainTextEditor.StartingScrollView()
        scroll.documentView = view
        window.contentView = scroll
        view.string = longNote
        window.layoutIfNeeded()
        let range = VaultPlainTextEditor.open(view, in: scroll, at: .caret(line: 150))
        XCTAssertEqual(view.selectedRange(), range)
        let top = try XCTUnwrap(VaultPlainTextEditor.lineTop(at: range.location,
                                                             layout: view.textLayoutManager))
        let visible = scroll.contentView.bounds
        XCTAssertGreaterThanOrEqual(top + view.textContainerOrigin.y, visible.minY - 1)
        XCTAssertLessThan(top + view.textContainerOrigin.y - visible.minY, visible.height / 4)
    }

    #endif
}
