import XCTest
import UIKit
@testable import Jesse

/// Document ASSEMBLY and COPY, away from any view.
///
/// This half proves the document is built correctly — one storage, blocks mapped,
/// cards recorded, typography carrying what used to be view structure — and that
/// `plainText(for:)` turns a character range into sensible pasteboard text. It
/// deliberately does NOT prove that a gesture can produce such a range; that is
/// `ReplySelectionTextViewTests`, at the text view.
@MainActor
final class MarkdownDocumentTests: XCTestCase {

    private func document() -> MarkdownDocument {
        MarkdownDocument.build(MarkdownReplyFixture.raw)
    }

    // MARK: - Assembly

    /// The whole reply is ONE storage — the property the fix turns on. Every block
    /// of the fixture has to be findable in it.
    func testWholeReplyIsOneStorage() {
        let text = document().attributed.string
        for expected in ["Weekly summary",
                         "You logged 14 meals",
                         "Three things stood out:",
                         "Breakfast was consistent",
                         "Log dinner before 21:00",
                         "the protein note",
                         "SELECT day, SUM(protein_g)",
                         "Protein",
                         "That is the whole picture."] {
            XCTAssertTrue(text.contains(expected), "missing from the document: \(expected)")
        }
    }

    /// Blocks are mapped in source order, with the kinds copy relies on.
    func testBlockMapMatchesTheSource() {
        let kinds = document().blocks.map(\.kind)
        XCTAssertEqual(kinds, [
            .heading,
            .paragraph,
            .paragraph,
            .listItem, .listItem, .listItem,
            .paragraph,
            .listItem, .listItem, .listItem,
            .paragraph,
            .code,
            .table,
            .paragraph,
        ])
    }

    /// Block ranges are non-overlapping, in order, and separated by exactly the one
    /// newline the builder emits — the invariant `plainText(for:)` assumes.
    func testBlockRangesTileTheDocument() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        var cursor = 0
        for (index, block) in doc.blocks.enumerated() {
            XCTAssertEqual(block.range.location, cursor, "gap or overlap before \(block.kind)")
            cursor = block.range.location + block.range.length
            if index < doc.blocks.count - 1 {
                XCTAssertEqual(ns.substring(with: NSRange(location: cursor, length: 1)), "\n")
                cursor += 1
            }
        }
        XCTAssertEqual(cursor, ns.length)
    }

    /// The code block and the table each get a card, and the table's card knows
    /// which row is its header (that is where the rule is drawn).
    func testCardsAreRecordedForCodeAndTable() {
        let doc = document()
        XCTAssertEqual(doc.decorations.count, 2)

        let ns = doc.attributed.string as NSString
        guard doc.decorations.count == 2 else { return }

        XCTAssertEqual(doc.decorations[0].kind, .code)
        XCTAssertTrue(ns.substring(with: doc.decorations[0].range).hasPrefix("SELECT day"))

        guard case let .table(headerRange) = doc.decorations[1].kind else {
            return XCTFail("second decoration is not a table")
        }
        XCTAssertEqual(ns.substring(with: headerRange), "\tDay\tProtein\tMet")
    }

    /// Code keeps every leading space. The old renderer put code in its own text
    /// view with no indent; here it sits in a shared document with a head indent,
    /// and the indent must be paragraph style, never inserted characters.
    func testCodeWhitespaceIsVerbatim() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        let code = doc.blocks.first { $0.kind == .code }
        XCTAssertNotNil(code)
        XCTAssertEqual(code.map { ns.substring(with: $0.range) },
                       "SELECT day, SUM(protein_g)\n  FROM meals\n GROUP BY day")
    }

    /// Inline emphasis, links and list markers survive into the document as real
    /// attributes and real text.
    func testInlineStylingSurvives() {
        let doc = document()
        let ns = doc.attributed.string as NSString

        // The link run carries a `.link`, and its visible text is the label.
        let linkRange = ns.range(of: "the protein note")
        XCTAssertNotEqual(linkRange.location, NSNotFound)
        let link = doc.attributed.attribute(.link, at: linkRange.location, effectiveRange: nil)
        XCTAssertNotNil(link, "the link run lost its .link attribute")

        // List markers are text, so they select and copy with their item.
        XCTAssertTrue(ns.contains("•\tBreakfast"))
        XCTAssertTrue(ns.contains("1.\tLog dinner"))

        // The heading is set in a heavier font than body text.
        let headingFont = doc.attributed.attribute(.font, at: 0, effectiveRange: nil) as? UIFont
        XCTAssertNotNil(headingFont)
        XCTAssertGreaterThan(headingFont?.pointSize ?? 0,
                             UIFont.preferredFont(forTextStyle: .body).pointSize)
    }

    /// Block spacing is typography, not blank lines: the gap after a block lives on
    /// the paragraph style of its LAST line only. If it were on every line, a code
    /// block would grow a block-gap between each line of code.
    func testBlockSpacingIsOnLastLinesOnly() {
        let doc = document()
        let ns = doc.attributed.string as NSString

        func spacing(atOffsetOf needle: String) -> CGFloat? {
            let at = ns.range(of: needle).location
            guard at != NSNotFound else { return nil }
            let style = doc.attributed.attribute(.paragraphStyle, at: at,
                                                 effectiveRange: nil) as? NSParagraphStyle
            return style?.paragraphSpacing
        }

        XCTAssertEqual(spacing(atOffsetOf: "SELECT day"), 0,
                       "an interior code line must not carry the block gap")
        XCTAssertEqual(spacing(atOffsetOf: " GROUP BY day"),
                       MarkdownDocument.Metrics.cardPadding + MarkdownDocument.Metrics.blockSpacing,
                       "the last code line carries the card padding plus the block gap")
        XCTAssertEqual(spacing(atOffsetOf: "Three things stood out:"),
                       MarkdownDocument.Metrics.blockSpacing)
    }

    /// Table columns become tab stops, one per column, honouring the delimiter
    /// row's alignments — including the first column, which is why every row starts
    /// with a tab.
    func testTableColumnsBecomeTabStops() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        let at = ns.range(of: "\tDay\tProtein\tMet").location
        XCTAssertNotEqual(at, NSNotFound, "table rows must start with a layout tab")

        let style = doc.attributed.attribute(.paragraphStyle, at: at,
                                             effectiveRange: nil) as? NSParagraphStyle
        XCTAssertEqual(style?.tabStops.count, 3)
        XCTAssertEqual(style?.tabStops.map(\.alignment), [.left, .right, .center],
                       "`| --- | ------: | :-: |` is leading, trailing, centre")
    }

    // MARK: - Copy

    func testCopyOfTheWholeDocument() {
        let doc = document()
        let all = NSRange(location: 0, length: (doc.attributed.string as NSString).length)
        XCTAssertEqual(doc.plainText(for: all), MarkdownReplyFixture.expectedFullCopy)
    }

    /// The case the whole change exists for: one range, three paragraphs and a
    /// bullet list, copied as text a person can paste.
    func testCopyAcrossParagraphs() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        let start = ns.range(of: MarkdownReplyFixture.crossParagraphStart).location
        let anchor = ns.range(of: MarkdownReplyFixture.crossParagraphEndAnchor).location
        XCTAssertNotEqual(start, NSNotFound)
        XCTAssertNotEqual(anchor, NSNotFound)
        let end = anchor + (MarkdownReplyFixture.crossParagraphEndPrefix as NSString).length

        XCTAssertEqual(doc.plainText(for: NSRange(location: start, length: end - start)),
                       MarkdownReplyFixture.expectedCrossParagraphCopy)
    }

    func testCopyOfASingleWord() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        let word = ns.range(of: MarkdownReplyFixture.singleWord)
        XCTAssertNotEqual(word.location, NSNotFound)
        XCTAssertEqual(doc.plainText(for: word), MarkdownReplyFixture.singleWord)
    }

    /// Table rows lose their layout tab on the way to the pasteboard, and keep the
    /// separators between cells.
    func testCopyOfATableIsTabSeparatedWithoutTheLayoutTab() {
        let doc = document()
        guard let table = doc.blocks.first(where: { $0.kind == .table }) else {
            return XCTFail("no table block")
        }
        XCTAssertEqual(doc.plainText(for: table.range),
                       "Day\tProtein\tMet\nMon\t128 g\t✅\nTue\t96 g\t❌")
    }

    /// Copy puts the selection on the pasteboard and nothing else — no role label,
    /// no timestamp, no "Jesse said".
    func testCopyAddsNoMetadata() {
        let doc = document()
        let ns = doc.attributed.string as NSString
        let all = NSRange(location: 0, length: ns.length)
        let copied = doc.plainText(for: all)
        for unwanted in ["Jesse", "Assistant", "http://", "https://"] {
            XCTAssertFalse(copied.contains(unwanted),
                           "copy leaked \(unwanted) into the pasteboard text")
        }
    }

    /// An empty selection copies nothing rather than the whole reply.
    func testEmptySelectionCopiesNothing() {
        XCTAssertEqual(document().plainText(for: NSRange(location: 4, length: 0)), "")
    }

    // MARK: - Dynamic Type

    /// The fonts are resolved into the storage, so a Dynamic Type change has to
    /// produce a NEW document — nothing in an `NSAttributedString` rescales itself.
    /// Same characters, same structure, larger type, and tab stops that moved with
    /// it because they are measured from the cells as rendered.
    func testAnAccessibilitySizeProducesTheSameTextSetLarger() {
        func build(_ category: UIContentSizeCategory) -> MarkdownDocument {
            MarkdownDocument.build(MarkdownReplyFixture.raw,
                                   traits: UITraitCollection(preferredContentSizeCategory: category))
        }
        let normal = build(.large)
        let accessible = build(.accessibilityExtraExtraExtraLarge)

        XCTAssertEqual(accessible.attributed.string, normal.attributed.string)
        XCTAssertEqual(accessible.blocks, normal.blocks)
        XCTAssertEqual(accessible.decorations, normal.decorations)

        func bodyPointSize(_ doc: MarkdownDocument) -> CGFloat {
            let at = (doc.attributed.string as NSString).range(of: "You logged").location
            let font = doc.attributed.attribute(.font, at: at, effectiveRange: nil) as? UIFont
            return font?.pointSize ?? 0
        }
        XCTAssertGreaterThan(bodyPointSize(accessible), bodyPointSize(normal) * 1.5)

        func secondColumnStop(_ doc: MarkdownDocument) -> CGFloat {
            let at = (doc.attributed.string as NSString).range(of: "\tDay\tProtein\tMet").location
            let style = doc.attributed.attribute(.paragraphStyle, at: at,
                                                 effectiveRange: nil) as? NSParagraphStyle
            return style?.tabStops.dropFirst().first?.location ?? 0
        }
        XCTAssertGreaterThan(secondColumnStop(accessible), secondColumnStop(normal),
                             "table columns must be measured at the size they are drawn at")
    }

    // MARK: - Inline code chips (MT3)

    /// The MT3 report's fixtures, read from the Mac test bundle's source folder (the simulator
    /// reads the checkout directly). The files end in a newline; the text sent did not.
    private func mt3(_ name: String) throws -> String {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("JesseMacTests/Fixtures/\(name).md")
        var text = try String(contentsOf: url, encoding: .utf8)
        if text.hasSuffix("\n") { text.removeLast() }
        return text
    }

    /// Every inline code run gets exactly one `.inlineCode` decoration covering exactly its
    /// text, in the label colour and in white; the fenced block's card is unchanged.
    func testEveryInlineCodeRunHasOneChipDecoration() throws {
        let message = try mt3("MT3-Message")
        let reply = try mt3("MT3-Reply")
        let prose = try mt3("MT3-Changelog-Paragraph")
        for color in [UIColor.label, UIColor.white] {
            for (text, spans) in [(message, ["TEST"]), (reply, ["TEST"]),
                                  (prose, ["McpSet::MessagesBuildPlacesInboundKubernetesRybbitTag1PlexHouseHealthdbClockifyRsRunnRs",
                                           "…+house+healthdb+clockify-rs+runn-rs", "capability-map.toml",
                                           "upstream-pod", "golden/capability-args.json", "direct", "+runn-rs"])] {
                let doc = MarkdownDocument.build(text, color: color)
                let ns = doc.attributed.string as NSString
                let chips = doc.decorations.filter { $0.kind == .inlineCode }
                XCTAssertEqual(chips.map { ns.substring(with: $0.range) }, spans)
            }
        }
        let doc = MarkdownDocument.build(message)
        let cards = doc.decorations.filter { $0.kind.isCard }
        XCTAssertEqual(cards.count, 1)
        XCTAssertEqual(cards.first.map { (doc.attributed.string as NSString).substring(with: $0.range) },
                       "Test test test")
        XCTAssertFalse(doc.attributed.string.contains("`"), "a backtick reached the rendered text")
    }

    /// At the real text view, a span draws one chip per line fragment it occupies: the long
    /// span in the changelog paragraph wraps at 400 points and gets one chip per line.
    func testAChipIsDrawnPerLineFragment() throws {
        let view = MarkdownDocumentTextView()
        view.frame = CGRect(x: 0, y: 0, width: 400, height: 2000)
        view.apply(MarkdownDocument.build(try mt3("MT3-Changelog-Paragraph")))
        view.layoutManager.ensureLayout(for: view.textContainer)
        var wrapped = false
        for chip in view.document.decorations where chip.kind == .inlineCode {
            let glyphs = view.layoutManager.glyphRange(forCharacterRange: chip.range,
                                                       actualCharacterRange: nil)
            var fragments = 0
            view.layoutManager.enumerateLineFragments(forGlyphRange: glyphs) { _, _, _, _, _ in
                fragments += 1
            }
            XCTAssertEqual(view.chipRects(for: chip.range).count, fragments)
            if fragments > 1 { wrapped = true }
        }
        XCTAssertTrue(wrapped, "no span wrapped at 400 points, so the per line case is untested")
    }

    /// A chip is drawing, never text: copy across a paragraph holding inline code is the
    /// source text with its backticks gone, exactly as before chips existed.
    func testCopyAcrossInlineCodeIsUnchanged() throws {
        let doc = MarkdownDocument.build(try mt3("MT3-Reply"))
        let all = NSRange(location: 0, length: (doc.attributed.string as NSString).length)
        XCTAssertEqual(doc.plainText(for: all),
                       "Your formatting came through as Markdown:\n\n•\tTEST arrived as inline code.\n•\t\"Test test test\" arrived as a fenced code block.")
    }
}
