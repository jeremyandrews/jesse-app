import AppKit
import SwiftUI

// One reply, or one user bubble, rendered as ONE selectable text view on the Mac.
//
// ── THE BUG THIS EXISTS TO FIX ────────────────────────────────────────────────
//
// The Mac rendered a reply as a `VStack` of SwiftUI `Text` views, one per heading, paragraph,
// list item, quote and code block, each with `.textSelection(.enabled)`. Each `Text` holds its
// own selection, so every block was a SELECTION ISLAND: a drag clamped at the end of its block
// and Select All reached one block. The phone had exactly this bug and fixed it in App 1.0
// (125) by composing the reply into one `NSAttributedString` (`MarkdownDocument`, which the
// Mac now compiles from the same source) and rendering it in one text view. This is that text
// view for AppKit: the same document, the same cards behind code and tables, the same copy.
//
// The user bubble and an expanded prompt fold render through it too, in the bubble's colours,
// so a pasted fence shows as a code card instead of raw backtick characters.

/// How a document view colours its text and cards, and whether it fills its width.
enum MacDocumentStyle: Equatable {
    /// A Jesse reply: label coloured text, filling the column.
    case reply
    /// A user bubble: white text on the tint, as wide as its text.
    case bubble

    var textColor: NSColor {
        switch self {
        case .reply: return .markdownLabel
        case .bubble: return .white
        }
    }

    var cardFill: NSColor {
        switch self {
        case .reply: return NSColor.secondaryLabelColor.withAlphaComponent(0.12)
        case .bubble: return NSColor.white.withAlphaComponent(0.18)
        }
    }

    /// The chip behind an inline code span: the card's colour family at the card's strength,
    /// a little stronger on the tint so it reads against it.
    var chipFill: NSColor {
        switch self {
        case .reply: return NSColor.secondaryLabelColor.withAlphaComponent(0.12)
        case .bubble: return NSColor.white.withAlphaComponent(0.22)
        }
    }

    var rule: NSColor {
        switch self {
        case .reply: return .separatorColor
        case .bubble: return NSColor.white.withAlphaComponent(0.4)
        }
    }

    var hugsContent: Bool { self == .bubble }
}

/// Instrumentation, the way `JesseThread.orderedTurns` counts its sorts: how often a document
/// is parsed and built, and how often one is laid out to answer SwiftUI's size question. Tests
/// read these to prove typing in the composer never rebuilds a reply.
enum MacDocumentCounters {
    static var builds = 0
    static var layouts = 0
}

/// A non editable, selectable text view that renders a `MarkdownDocument`: its text, the cards
/// behind its code blocks and tables, and its copy semantics.
///
/// TextKit 1 on purpose, for the reason the phone's `MarkdownDocumentTextView` gives: the cards
/// are drawn from line fragment geometry, which `NSLayoutManager` gives directly. Non
/// contiguous layout, so drawing a long reply lays out what is drawn.
final class MacDocumentTextView: NSTextView {
    /// The document currently rendered. Set through `apply(_:style:)`.
    private(set) var document: MarkdownDocument = .empty
    private(set) var style: MacDocumentStyle = .reply
    /// What Copy hands over when the WHOLE text is selected, in place of the document's plain
    /// text: the source exactly as it was sent, for a user bubble, including the part a folded
    /// long turn does not show. Nil for a reply, whose copy is always the document's.
    var wholeText: String?
    /// Where Copy writes: the general pasteboard in the app, a private one in tests.
    var copyPasteboard: NSPasteboard = .general

    /// The root of this view's TextKit 1 stack, which nothing else in the stack owns.
    private var ownedStorage: NSTextStorage?

    init() {
        let storage = NSTextStorage()
        let manager = NSLayoutManager()
        manager.allowsNonContiguousLayout = true
        storage.addLayoutManager(manager)
        let container = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        container.lineFragmentPadding = 0
        manager.addTextContainer(container)
        super.init(frame: .zero, textContainer: container)
        ownedStorage = storage

        isEditable = false
        isSelectable = true
        isRichText = true
        drawsBackground = false
        textContainerInset = .zero
        isVerticallyResizable = false
        isHorizontallyResizable = false
        isAutomaticLinkDetectionEnabled = false
        // Links come from `.link` attributes only, never from data detection.
        isAutomaticDataDetectionEnabled = false
        usesFontPanel = false
        allowsUndo = false
    }

    override init(frame frameRect: NSRect, textContainer container: NSTextContainer?) {
        super.init(frame: frameRect, textContainer: container)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Install `document`. Callers decide whether a change is real before calling, because
    /// replacing the storage necessarily drops a live selection.
    func apply(_ document: MarkdownDocument, style: MacDocumentStyle) {
        self.document = document
        self.style = style
        textStorage?.setAttributedString(document.attributed)
        linkTextAttributes = style == .bubble
            ? [.foregroundColor: NSColor.white, .underlineStyle: NSUnderlineStyle.single.rawValue,
               .cursor: NSCursor.pointingHand]
            : [.foregroundColor: NSColor.linkColor, .cursor: NSCursor.pointingHand]
        needsDisplay = true
    }

    /// The size the document lays out to at `width`: its used rect, so a hugging bubble can be
    /// as wide as its widest line.
    func layoutSize(forWidth width: CGFloat) -> CGSize {
        guard let manager = layoutManager, let container = textContainer else { return .zero }
        if container.size.width != width {
            container.size = NSSize(width: width, height: CGFloat.greatestFiniteMagnitude)
        }
        manager.ensureLayout(for: container)
        var used = manager.usedRect(for: container)
        if style.hugsContent {
            // The used rect spans the container's width, so a bubble as wide as its text takes
            // the widest line's glyphs instead. A code or table card spans the bubble anyway.
            var widest: CGFloat = 0
            let glyphs = manager.glyphRange(for: container)
            manager.enumerateLineFragments(forGlyphRange: glyphs) { _, _, _, lineGlyphs, _ in
                let bounds = manager.boundingRect(forGlyphRange: lineGlyphs, in: container)
                widest = max(widest, bounds.maxX)
            }
            if document.decorations.contains(where: { $0.kind == .inlineCode }) {
                // Room for a chip that ends a line to reach past its last glyph.
                widest += MarkdownDocument.Metrics.chipPadding
            }
            if document.decorations.contains(where: { $0.kind.isCard }) { widest = width }
            used.size.width = min(widest, width)
        }
        // The container tracks the frame's width only when the frame CHANGES, so a probe at
        // another width would otherwise leave the text laid out for a width it is not drawn at.
        if bounds.width > 0, bounds.width != width {
            container.size = NSSize(width: bounds.width, height: CGFloat.greatestFiniteMagnitude)
        }
        return CGSize(width: ceil(used.width), height: ceil(used.height))
    }

    // MARK: - Decorations

    /// The cards go down with the background, so the selection highlight and the text are
    /// drawn over them.
    override func drawBackground(in rect: NSRect) {
        super.drawBackground(in: rect)
        drawDecorations()
    }

    private func drawDecorations() {
        guard !document.decorations.isEmpty else { return }
        let pad = MarkdownDocument.Metrics.cardPadding
        let radius = MarkdownDocument.Metrics.cardCornerRadius
        for decoration in document.decorations where decoration.kind.isCard {
            guard let body = usedRect(for: decoration.range) else { continue }
            let card = NSRect(x: 0, y: body.minY - pad, width: bounds.width,
                              height: body.height + pad * 2)
            style.cardFill.setFill()
            NSBezierPath(roundedRect: card, xRadius: radius, yRadius: radius).fill()

            // The rule under a table's header row.
            if case let .table(headerRange) = decoration.kind,
               let header = usedRect(for: headerRange) {
                let y = (header.maxY + MarkdownDocument.Metrics.rowSpacing / 2).rounded()
                let thickness = 1 / max(window?.backingScaleFactor ?? 2, 1)
                style.rule.setFill()
                NSRect(x: pad, y: y, width: max(bounds.width - pad * 2, 0), height: thickness).fill()
            }
        }

        // Chips after cards, so a span in a table cell sits on its card.
        let chipRadius = MarkdownDocument.Metrics.chipCornerRadius
        style.chipFill.setFill()
        for decoration in document.decorations where decoration.kind == .inlineCode {
            for chip in chipRects(for: decoration.range) {
                NSBezierPath(roundedRect: chip, xRadius: chipRadius, yRadius: chipRadius).fill()
            }
        }
    }

    /// The chips behind an inline code range, in view coordinates: one per line fragment the
    /// span occupies, as wide as its glyphs on that line plus `chipPadding` each side (kept
    /// inside the view), as tall as the line's used rect.
    func chipRects(for range: NSRange) -> [NSRect] {
        guard range.length > 0, let manager = layoutManager, let container = textContainer else {
            return []
        }
        let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        guard glyphs.length > 0 else { return [] }
        let pad = MarkdownDocument.Metrics.chipPadding
        var chips: [NSRect] = []
        manager.enumerateLineFragments(forGlyphRange: glyphs) { _, used, _, lineGlyphs, _ in
            let piece = NSIntersectionRange(lineGlyphs, glyphs)
            guard piece.length > 0 else { return }
            let ink = manager.boundingRect(forGlyphRange: piece, in: container)
            let minX = max(ink.minX - pad, 0)
            let maxX = min(ink.maxX + pad, container.size.width)
            guard maxX > minX else { return }
            chips.append(NSRect(x: minX + self.textContainerOrigin.x,
                                y: used.minY + self.textContainerOrigin.y,
                                width: maxX - minX, height: used.height))
        }
        return chips
    }

    /// The union of the USED line fragment rects for a character range, in view coordinates:
    /// used rects, because the card padding is real paragraph spacing and the card is drawn
    /// into it (see `MarkdownDocumentTextView.usedRect(for:)` on the phone).
    private func usedRect(for range: NSRange) -> NSRect? {
        guard range.length > 0, let manager = layoutManager else { return nil }
        let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        guard glyphs.length > 0 else { return nil }
        var union: NSRect?
        manager.enumerateLineFragments(forGlyphRange: glyphs) { _, used, _, _, _ in
            union = union.map { $0.union(used) } ?? used
        }
        guard var rect = union else { return nil }
        rect.origin.x += textContainerOrigin.x
        rect.origin.y += textContainerOrigin.y
        return rect
    }

    // MARK: - Copy

    /// The text a selection of `range` copies as: the document's plain text (blank lines back
    /// between blocks, the table rows' layout tab gone), or `wholeText` for the whole view.
    func copiedText(for range: NSRange) -> String {
        if let wholeText, range == NSRange(location: 0, length: (string as NSString).length) {
            return wholeText
        }
        return document.plainText(for: range)
    }

    /// Copy puts the selection's plain text on the pasteboard, and nothing else: no rich text,
    /// no role prefix. `NSTextView`'s own copy would hand over the backing string, whose blocks
    /// are separated by single newlines because the visible gap between them is paragraph
    /// spacing.
    override func copy(_ sender: Any?) {
        let range = selectedRange()
        guard range.length > 0 else { return }
        copyPasteboard.clearContents()
        copyPasteboard.setString(copiedText(for: range), forType: .string)
    }

    /// A drag of the selection, and the Services menu, carry the same text Copy does.
    override var writablePasteboardTypes: [NSPasteboard.PasteboardType] { [.string] }

    override func writeSelection(to pboard: NSPasteboard,
                                 types: [NSPasteboard.PasteboardType]) -> Bool {
        let range = selectedRange()
        guard range.length > 0 else { return false }
        pboard.declareTypes([.string], owner: nil)
        return pboard.setString(copiedText(for: range), forType: .string)
    }
}

/// SwiftUI wrapper for `MacDocumentTextView`.
///
/// It holds the raw text and builds the document itself, and only when the text, the style or
/// the font size really changed: SwiftUI re-evaluates a transcript row for reasons that have
/// nothing to do with it (a keystroke in the composer used to be one), and reassigning the
/// storage would drop a live selection. The comparison is of the source string, which is cheap
/// next to parsing and laying it out.
struct MacDocumentText: NSViewRepresentable {
    let text: String
    var style: MacDocumentStyle = .reply
    /// See `MacDocumentTextView.wholeText`.
    var wholeText: String? = nil

    @Environment(\.openURL) private var openURL

    /// The identity of the document in the text view's storage.
    struct Key: Equatable {
        let text: String
        let style: MacDocumentStyle
        let bodySize: CGFloat
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var applied: Key?
        /// Laid out sizes by proposed width, for the document in `applied`.
        var sizes: [CGFloat: CGSize] = [:]
        var openURL: OpenURLAction?

        /// A link opens the way a SwiftUI `Text` link does, through the environment's
        /// `openURL`, so a vault wiki link reaches the app's own handler.
        func textView(_ textView: NSTextView, clickedOnLink link: Any, at charIndex: Int) -> Bool {
            let url = (link as? URL) ?? (link as? String).flatMap(URL.init(string:))
            guard let url, let openURL else { return false }
            openURL(url)
            return true
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> MacDocumentTextView {
        let view = MacDocumentTextView()
        view.delegate = context.coordinator
        install(into: view, coordinator: context.coordinator)
        return view
    }

    func updateNSView(_ view: MacDocumentTextView, context: Context) {
        install(into: view, coordinator: context.coordinator)
    }

    private func install(into view: MacDocumentTextView, coordinator: Coordinator) {
        coordinator.openURL = openURL
        view.wholeText = wholeText
        let key = Key(text: text, style: style,
                      bodySize: NSFont.preferredFont(forTextStyle: .body).pointSize)
        guard coordinator.applied != key else { return }
        coordinator.applied = key
        coordinator.sizes = [:]
        MacDocumentCounters.builds += 1
        view.apply(MarkdownDocument.build(text, color: style.textColor), style: style)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView view: MacDocumentTextView,
                      context: Context) -> CGSize? {
        let width = proposal.width.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
            ?? Self.unboundedWidth
        if let cached = context.coordinator.sizes[width] { return cached }
        MacDocumentCounters.layouts += 1
        let used = view.layoutSize(forWidth: width)
        let size = CGSize(width: style.hugsContent ? min(used.width, width) : width,
                          height: used.height)
        context.coordinator.sizes[width] = size
        return size
    }

    /// The width an unspecified proposal is answered at: wide enough that no line wraps, so
    /// the ideal size is the text's own.
    private static let unboundedWidth: CGFloat = 100_000
}

/// Where a long user turn folds: the head it shows, and how many lines the whole has.
///
/// A pasted log or file can be a megabyte, and laying all of it out to draw a bubble most of
/// which is off screen is the cost this avoids. Folded, the bubble shows the head and a
/// control that expands it in place; Select All then Copy still takes the whole text.
struct MacLongTextFold: Equatable {
    /// Past either of these a turn folds.
    static let maxLines = 200
    static let maxCharacters = 20_000

    /// What the folded bubble shows: the first `maxLines` lines, cut back to the last whole
    /// line inside `maxCharacters` when those lines are longer than that.
    let head: String
    /// Lines in the whole text.
    let lineCount: Int
    /// Whether the text is long enough to fold at all.
    let isLong: Bool

    init(_ text: String) {
        let utf16 = text.utf16
        var lines = 1
        var headEnd: String.Index?
        var cutOnCharacters = false
        var index = utf16.startIndex
        var offset = 0
        while index != utf16.endIndex {
            if utf16[index] == 0x0A {
                if lines == Self.maxLines, headEnd == nil { headEnd = index }
                lines += 1
            }
            offset += 1
            if offset == Self.maxCharacters, headEnd == nil {
                headEnd = utf16.index(after: index)
                cutOnCharacters = true
            }
            index = utf16.index(after: index)
        }
        lineCount = lines
        isLong = lines > Self.maxLines || offset > Self.maxCharacters
        guard isLong, let end = headEnd else {
            head = text
            return
        }
        var cut = String(text[..<end])
        // A cut on the character limit goes back to the last whole line, when there is one.
        if cutOnCharacters, let lastNewline = cut.lastIndex(of: "\n") {
            cut = String(cut[..<lastNewline])
        }
        head = cut
    }
}

/// A user turn's text in its bubble: the document view in the bubble's colours, folded when
/// the turn is long.
struct MacUserTurnText: View {
    let text: String
    @State private var showAll = false

    var body: some View {
        let fold = MacLongTextFold(text)
        VStack(alignment: .leading, spacing: 6) {
            MacDocumentText(text: fold.isLong && !showAll ? fold.head : text,
                            style: .bubble, wholeText: text)
            if fold.isLong, !showAll {
                Button("Show all (\(fold.lineCount) lines)") { showAll = true }
                    .buttonStyle(.plain)
                    .font(.callout.weight(.semibold))
                    .underline()
                    .accessibilityHint("Shows the rest of this message")
            }
        }
    }
}
