import XCTest
import AppKit
import SwiftUI
import JesseCore
@testable import Jesse_Mac

/// Selection and copy at the REAL text view, driven through the real transcript row.
///
/// The Mac's mirror of the phone's `ReplySelectionTextViewTests`. These render `MacTurnBubble`
/// through an `NSHostingView` in an off screen window, find the text views AppKit actually
/// created, and drive `setSelectedRange`, `selectAll(_:)` and `copy(_:)` on them: the same
/// actions a drag, Command A and Command C reach.
///
/// WHY THESE FAIL AGAINST THE ORIGINAL: the old renderer drew every block as its own SwiftUI
/// `Text`, so there was no text view holding two blocks at all: no `MacDocumentTextView`
/// exists, the cross paragraph range cannot be formed, and Select All reaches one block.
@MainActor
final class MacReplySelectionTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var pasteboards: [NSPasteboard] = []

    override func tearDown() async throws {
        await MainActor.run {
            for board in pasteboards { board.releaseGlobally() }
            pasteboards = []
            for window in windows {
                window.contentView = nil
                window.close()
            }
            windows = []
        }
        try await super.tearDown()
    }

    // MARK: - Fixtures

    /// The phone's purpose built reply: a heading, three paragraphs (the first two source
    /// lines), a bullet list, a numbered list with a link, a code block with meaningful leading
    /// whitespace and a table, with the exact text each selection must copy. The same document
    /// builder renders it on both platforms, so the expected text is the same.
    enum Reply {
        static let raw = """
        # Weekly summary

        You logged 14 meals this week 🍎 and hit protein on five days.
        The gap was Thursday, when dinner went unlogged.

        Three things stood out:

        - Breakfast was consistent, oats and yoghurt 🥣
        - Lunch drifted later each day
        - Dinner protein was the weak point

        Do these next week:

        1. Log dinner before 21:00
        2. Add a protein source at lunch
        3. Read [the protein note](https://example.com/protein)

        Here is the query I used:

        ```sql
        SELECT day, SUM(protein_g)
          FROM meals
         GROUP BY day
        ```

        | Day | Protein | Met |
        | --- | ------: | :-: |
        | Mon | 128 g | ✅ |
        | Tue | 96 g | ❌ |

        That is the whole picture.
        """

        static let expectedFullCopy = """
        Weekly summary

        You logged 14 meals this week 🍎 and hit protein on five days.
        The gap was Thursday, when dinner went unlogged.

        Three things stood out:

        •\tBreakfast was consistent, oats and yoghurt 🥣
        •\tLunch drifted later each day
        •\tDinner protein was the weak point

        Do these next week:

        1.\tLog dinner before 21:00
        2.\tAdd a protein source at lunch
        3.\tRead the protein note

        Here is the query I used:

        SELECT day, SUM(protein_g)
          FROM meals
         GROUP BY day

        Day\tProtein\tMet
        Mon\t128 g\t✅
        Tue\t96 g\t❌

        That is the whole picture.
        """

        static let expectedCrossParagraphCopy = """
        14 meals this week 🍎 and hit protein on five days.
        The gap was Thursday, when dinner went unlogged.

        Three things stood out:

        •\tBreakfast was consistent, oats and yoghurt 🥣
        •\tLunch drifted later each day
        •\tDinner protein was the weak point

        Do these
        """
    }

    private func host<V: View>(_ view: V, width: CGFloat = 1200) -> NSHostingView<V> {
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: 900),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        host.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        host.layoutSubtreeIfNeeded()
        return host
    }

    private func reply(_ text: String) -> some View {
        MacTurnBubble(turn: Turn(role: .jesse, text: text)).frame(width: 800)
    }

    static func documentViews(in view: NSView) -> [MacDocumentTextView] {
        var found: [MacDocumentTextView] = []
        if let text = view as? MacDocumentTextView { found.append(text) }
        for sub in view.subviews { found.append(contentsOf: documentViews(in: sub)) }
        return found
    }

    /// Copy through the view's own `copy(_:)`, onto a private pasteboard.
    private func copied(from view: MacDocumentTextView) -> String? {
        let board = NSPasteboard(name: NSPasteboard.Name("JesseMacTests.copy.\(UUID().uuidString)"))
        pasteboards.append(board)
        board.clearContents()
        view.copyPasteboard = board
        view.copy(nil)
        return board.string(forType: .string)
    }

    // MARK: - The structural fix

    func testACompletedReplyIsOneTextView() throws {
        let views = Self.documentViews(in: host(reply(try MacReplyFixture.text())))
        XCTAssertEqual(views.count, 1,
                       "a completed reply must be one selection surface, not \(views.count) islands")
    }

    // MARK: - Selection

    func testASelectionFromTheFirstParagraphIntoTheThirdCrossesTheList() throws {
        let view = try XCTUnwrap(Self.documentViews(in: host(reply(Reply.raw))).first)
        let ns = view.string as NSString
        let start = ns.range(of: "14 meals")
        let anchor = ns.range(of: "Do these next week")
        XCTAssertNotEqual(start.location, NSNotFound)
        XCTAssertNotEqual(anchor.location, NSNotFound,
                          "the third paragraph is not in the SAME text view: selection islands")
        guard start.location != NSNotFound, anchor.location != NSNotFound else { return }
        let end = anchor.location + ("Do these" as NSString).length

        view.setSelectedRange(NSRange(location: start.location, length: end - start.location))
        XCTAssertEqual(view.selectedRange().length, end - start.location,
                       "the text view refused a range spanning three paragraphs")
        XCTAssertEqual(copied(from: view), Reply.expectedCrossParagraphCopy)
    }

    func testSelectAllCoversTheWholeReply() throws {
        let view = try XCTUnwrap(Self.documentViews(in: host(reply(Reply.raw))).first)
        view.window?.makeFirstResponder(view)
        view.selectAll(nil)
        XCTAssertEqual(view.selectedRange(), NSRange(location: 0, length: (view.string as NSString).length))
        XCTAssertEqual(copied(from: view), Reply.expectedFullCopy)
    }

    func testSelectAllOnTheRealFixtureTakesEveryBlockKind() throws {
        let fixture = try MacReplyFixture.text()
        let view = try XCTUnwrap(Self.documentViews(in: host(reply(fixture))).first)
        view.selectAll(nil)
        let copy = try XCTUnwrap(copied(from: view))
        XCTAssertEqual(copy, view.document.plainText(for: NSRange(location: 0, length: (view.string as NSString).length)))
        XCTAssertTrue(copy.contains("Query\tMain thread per keystroke, before\tAfter"), "the table header")
        XCTAssertTrue(copy.contains("cargo test --workspace\n```"), "the tilde fence's body")
        XCTAssertTrue(copy.contains("tail -f ~/Library/Logs/jesse-bridge.log"), "the unclosed fence")
        XCTAssertTrue(copy.contains("1.\tElevation was read only"), "the numbered list")
    }

    func testTableAndCodeParticipateInOneRange() throws {
        let view = try XCTUnwrap(Self.documentViews(in: host(reply(Reply.raw))).first)
        let ns = view.string as NSString
        let code = ns.range(of: "SELECT day")
        let cell = ns.range(of: "❌")
        XCTAssertNotEqual(code.location, NSNotFound, "code is not in the document")
        XCTAssertNotEqual(cell.location, NSNotFound, "a table cell is not in the document")
        view.setSelectedRange(NSRange(location: code.location,
                                      length: cell.location + cell.length - code.location))
        let copy = try XCTUnwrap(copied(from: view))
        XCTAssertTrue(copy.hasPrefix("SELECT day, SUM(protein_g)\n  FROM meals\n GROUP BY day"),
                      "code lost its whitespace on the way to the pasteboard")
        XCTAssertTrue(copy.hasSuffix("Day\tProtein\tMet\nMon\t128 g\t✅\nTue\t96 g\t❌"),
                      "table cells did not copy as a tab separated grid: \(copy)")
    }

    func testAnUnrelatedUpdateKeepsTheSelectionAndBuildsNothing() throws {
        let host = host(Harness(text: Reply.raw, unrelated: 0))
        let view = try XCTUnwrap(Self.documentViews(in: host).first)
        let word = (view.string as NSString).range(of: "Thursday")
        view.setSelectedRange(word)
        let builds = MacDocumentCounters.builds

        host.rootView = Harness(text: Reply.raw, unrelated: 1)
        host.layoutSubtreeIfNeeded()

        let after = try XCTUnwrap(Self.documentViews(in: host).first)
        XCTAssertEqual(after.selectedRange(), word, "an unrelated SwiftUI update dropped the selection")
        XCTAssertEqual(MacDocumentCounters.builds, builds, "an unrelated update rebuilt the reply")
    }

    private struct Harness: View {
        let text: String
        let unrelated: Int
        var body: some View {
            VStack(alignment: .leading) {
                MacTurnBubble(turn: Turn(role: .jesse, text: text))
                Text("unrelated \(unrelated)")
            }
            .frame(width: 800)
        }
    }

    // MARK: - Rendering

    /// No backtick from a fence or an inline code span reaches the rendered string: outside a
    /// code block there is no backtick at all, and each code block holds its body only.
    func testNoFenceOrCodeSpanBacktickIsRendered() throws {
        let fixture = try MacReplyFixture.text()
        let view = try XCTUnwrap(Self.documentViews(in: host(reply(fixture))).first)
        let document = view.document
        let ns = document.attributed.string as NSString
        for block in document.blocks where block.kind != .code {
            let text = ns.substring(with: block.range)
            XCTAssertFalse(text.contains("`"), "a backtick rendered outside code: \(text.prefix(120))")
        }
        let code = document.blocks.filter { $0.kind == .code }.map { ns.substring(with: $0.range) }
        // The only backticks left are the ones the tilde and four backtick fences QUOTE.
        for body in code where body.contains("`") {
            XCTAssertTrue(body == "cargo test --workspace\n```" || body == "```swift\nlet quoted = true\n```",
                          "a fence leaked into a code block: \(body)")
        }
        // An inline code span is set in a monospaced face, its backticks gone.
        let span = ns.range(of: "HKWorkout.duration")
        XCTAssertNotEqual(span.location, NSNotFound)
        let font = try XCTUnwrap(document.attributed.attribute(.font, at: span.location,
                                                                effectiveRange: nil) as? NSFont)
        XCTAssertTrue(font.fontDescriptor.symbolicTraits.contains(.monoSpace),
                      "an inline code span is not monospaced")
        XCTAssertEqual(document.decorations.filter { $0.kind == .code }.count, code.count,
                       "every code block sits on a card")
    }

    func testAWikiLinkStaysALink() throws {
        let view = try XCTUnwrap(Self.documentViews(in: host(reply("See [[todo-list/Strands/Strands-System]] now."))).first)
        let ns = view.string as NSString
        let at = ns.range(of: "Strands-System")
        XCTAssertNotEqual(at.location, NSNotFound, "the wiki link's text is not rendered: \(view.string)")
        guard at.location != NSNotFound else { return }
        XCTAssertNotNil(view.textStorage?.attribute(.link, at: at.location, effectiveRange: nil),
                        "a wiki link lost its link on the Mac")
    }

    // MARK: - The user bubble

    func testAPastedFenceInAUserBubbleIsACodeCard() throws {
        let turn = Turn(role: .user, text: "Why does this fail?\n\n```swift\nlet x: Int = \"1\"\n```")
        let view = try XCTUnwrap(Self.documentViews(in: host(MacTurnBubble(turn: turn).frame(width: 800))).first)
        XCTAssertFalse(view.string.contains("`"), "the bubble shows raw backticks: \(view.string)")
        XCTAssertEqual(view.document.decorations.count, 1, "the fence is not a code card")
        XCTAssertEqual(view.style, .bubble)
    }

    /// A short message makes a short bubble: the text view is as wide as its text, not the
    /// column. (The layout manager's used rect spans the container, so this is measured.)
    func testAShortUserBubbleIsAsWideAsItsText() throws {
        let turn = Turn(role: .user, text: "What changed in the reader?")
        let view = try XCTUnwrap(Self.documentViews(in: host(MacTurnBubble(turn: turn).frame(width: 800))).first)
        XCTAssertGreaterThan(view.frame.width, 50)
        XCTAssertLessThan(view.frame.width, 400, "a one line bubble took \(view.frame.width) pt")
    }

    func testALongUserTurnFoldsAndSelectAllStillCopiesAllOfIt() throws {
        let text = (1...450).map { "Line \($0) of a pasted log." }.joined(separator: "\n")
        let host = host(MacTurnBubble(turn: Turn(role: .user, text: text)).frame(width: 800))
        let view = try XCTUnwrap(Self.documentViews(in: host).first)
        XCTAssertTrue(view.string.contains("Line 200 of"), "the head stops short of 200 lines")
        XCTAssertFalse(view.string.contains("Line 201 of"), "the fold did not fold")
        XCTAssertEqual(MacLongTextFold(text).lineCount, 450)
        view.selectAll(nil)
        XCTAssertEqual(copied(from: view), text, "Select All then Copy on a folded turn must take all of it")
    }

    func testTheFoldCutsALongLineOnTheCharacterLimit() {
        let fold = MacLongTextFold(String(repeating: "word ", count: 10_000))
        XCTAssertTrue(fold.isLong)
        XCTAssertLessThanOrEqual(fold.head.utf16.count, MacLongTextFold.maxCharacters)
        let short = MacLongTextFold("one\ntwo")
        XCTAssertFalse(short.isLong)
        XCTAssertEqual(short.head, "one\ntwo")
    }

    // MARK: - Streaming

    @Observable
    final class Stream {
        var text = ""
    }

    private struct StreamHost: View {
        let stream: Stream
        var body: some View {
            MacStreamingBubble(text: stream.text, activity: "").frame(width: 800)
        }
    }

    /// The 1 MB fixture streamed in 2 KB chunks: the partial is parsed at most once per
    /// renderer interval, never once per chunk, and the last text is rendered in the end.
    func testStreamingAMegabyteParsesAtTheRendererCadenceAndCatchesUp() throws {
        let megabyte = try MacLargePasteTests.paste(ofSize: 1_000_000)
        let stream = Stream()
        let host = host(StreamHost(stream: stream))
        let parsesBefore = MacStreamingMarkdown.parses
        let chunk = 2_000
        var offset = megabyte.startIndex
        let start = Date()
        var chunks = 0
        while offset < megabyte.endIndex {
            let next = megabyte.index(offset, offsetBy: chunk, limitedBy: megabyte.endIndex)
                ?? megabyte.endIndex
            stream.text += megabyte[offset..<next]
            offset = next
            chunks += 1
            RunLoop.current.run(until: Date().addingTimeInterval(0.001))
            host.layoutSubtreeIfNeeded()
        }
        let elapsed = Date().timeIntervalSince(start)
        // The catch up: one more render past the cooldown shows the final text.
        RunLoop.current.run(until: Date().addingTimeInterval(MacStreamingMarkdown.interval * 3))
        host.layoutSubtreeIfNeeded()
        let parses = MacStreamingMarkdown.parses - parsesBefore
        let allowed = Int(elapsed / MacStreamingMarkdown.interval) + 3
        XCTAssertLessThanOrEqual(parses, allowed,
                                 "\(parses) parses for \(chunks) chunks in \(elapsed) s")
        XCTAssertLessThan(parses, chunks, "the stream parsed on every chunk")
        XCTAssertTrue(Self.documentViews(in: host).isEmpty,
                      "the streaming partial must not pay for the document view")
        print("MT1-TIMING stream 1 MB in 2 KB chunks: \(chunks) chunks, \(String(format: "%.3f", elapsed)) s, "
              + "\(parses) parses, \(String(format: "%.1f", elapsed * 1000 / Double(chunks))) ms per chunk")
    }

    // MARK: - First render, timed

    func testFirstRenderTimings() throws {
        let fixture = try MacReplyFixture.text()
        var start = Date()
        _ = host(reply(fixture))
        print("MT1-TIMING first render fixture reply: \(String(format: "%.3f", Date().timeIntervalSince(start))) s")

        let megabyte = try MacLargePasteTests.paste(ofSize: 1_000_000)
        start = Date()
        _ = host(MacTurnBubble(turn: Turn(role: .user, text: megabyte)).frame(width: 800))
        print("MT1-TIMING first render 1 MB user turn: \(String(format: "%.3f", Date().timeIntervalSince(start))) s")
    }
}
