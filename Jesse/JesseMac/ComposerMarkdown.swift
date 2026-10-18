import AppKit
import JesseMarkdown

// Live Markdown styling in the Mac composer: what Jeremy types is shown styled (code spans,
// fences, bold, italic) while every character stays exactly what he typed and what is sent.
//
// Two halves. `ComposerMarkdown` is a pure scanner, from the text to ranges and styles, with
// no view and no layout, so it can be tested as a table. `ComposerMarkdownStyler` puts those
// ranges on screen without touching the string:
//
//   * faces (monospaced, bold, italic) are `.font` attributes on the text storage. They do not
//     change `string`, the binding, or what is copied out (the view is plain text, so the
//     pasteboard gets the string alone), and they are set directly on the storage, outside
//     `shouldChangeText`, so they never enter the undo stack. TextKit 1 temporary attributes
//     cannot carry them: a temporary attribute may not change layout, and a font does.
//   * the dimmed markers are a temporary `.foregroundColor` on the layout manager, the drawing
//     only attribute that is exactly made for this; spell checking's own temporary attributes
//     are left alone because only that key is ever added or removed.
//   * the chip behind a code span and the tint behind a fence are drawn by the text view from
//     line fragment geometry, as the reply's document view draws its chips and cards.
//
// The fences follow `MarkdownFence`, the one rule the reply parser and the vault reader share,
// so the composer and the bubble it turns into agree on what is a code block.

/// The composer's Markdown, as ranges to style. Pure.
nonisolated enum ComposerMarkdown {
    enum Style: Hashable {
        /// A fenced block, fence lines included: monospaced, on a full width tint.
        case fence
        /// An inline code span, backticks included: monospaced, on a chip.
        case code
        case bold
        case italic
        /// A backtick, asterisk, underscore or fence run: drawn in the secondary label colour.
        case marker
    }

    struct Span: Hashable {
        let range: NSRange
        let style: Style
    }

    static func scan(_ text: String) -> [Span] { scan(text as NSString) }

    /// Every span in `text`, ordered by location.
    static func scan(_ text: NSString) -> [Span] {
        var spans: [Span] = []
        var paragraph: NSRange?
        func flush() {
            if let range = paragraph { spans += inline(text, in: range) }
            paragraph = nil
        }
        var open: (fence: MarkdownFence, start: Int)?
        for line in lines(text) {
            let content = text.substring(with: line.content)
            if let current = open {
                if current.fence.isClosed(by: content) {
                    spans.append(Span(range: NSRange(location: current.start,
                                                     length: NSMaxRange(line.content) - current.start),
                                      style: .fence))
                    spans.append(contentsOf: fenceMarker(text, line: line.content, character: current.fence.character))
                    open = nil
                }
                continue
            }
            if let fence = MarkdownFence.opening(content) {
                flush()
                open = (fence, line.content.location)
                spans.append(contentsOf: fenceMarker(text, line: line.content, character: fence.character))
                continue
            }
            if content.trimmingCharacters(in: .whitespaces).isEmpty {
                flush()
                continue
            }
            paragraph = paragraph.map { NSUnionRange($0, line.content) } ?? line.content
        }
        flush()
        if let current = open {
            // An unclosed fence runs to the end, as it does in the reply.
            spans.append(Span(range: NSRange(location: current.start, length: text.length - current.start),
                              style: .fence))
        }
        return spans.sorted { ($0.range.location, $0.range.length) < ($1.range.location, $1.range.length) }
    }

    /// The range a restyle of `edited` must cover: the edit widened to whole lines, then to
    /// the paragraph those lines belong to (a code span or emphasis may cross a line break
    /// inside one), stopping at blank lines and at fence lines.
    static func paragraphRange(_ text: NSString, around edited: NSRange) -> NSRange {
        var range = text.lineRange(for: NSIntersectionRange(edited, NSRange(location: 0, length: text.length)))
        func isBoundary(_ line: NSRange) -> Bool {
            let content = text.substring(with: line).trimmingCharacters(in: .whitespacesAndNewlines)
            return content.isEmpty || MarkdownFence.opening(content) != nil
        }
        while range.location > 0 {
            let previous = text.lineRange(for: NSRange(location: range.location - 1, length: 0))
            if isBoundary(previous) { break }
            range = NSUnionRange(range, previous)
        }
        while NSMaxRange(range) < text.length {
            let next = text.lineRange(for: NSRange(location: NSMaxRange(range), length: 0))
            if isBoundary(next) { break }
            range = NSUnionRange(range, next)
        }
        return range
    }

    // MARK: - Lines

    private struct Line {
        /// The line without its terminator.
        let content: NSRange
    }

    private static func lines(_ text: NSString) -> [Line] {
        var result: [Line] = []
        var start = 0
        while start < text.length {
            var lineEnd = 0, contentsEnd = 0
            text.getLineStart(nil, end: &lineEnd, contentsEnd: &contentsEnd,
                              for: NSRange(location: start, length: 0))
            result.append(Line(content: NSRange(location: start, length: contentsEnd - start)))
            start = lineEnd
        }
        // A trailing newline opens one more, empty, line.
        if text.length == 0 || text.character(at: text.length - 1) == newline {
            result.append(Line(content: NSRange(location: text.length, length: 0)))
        }
        return result
    }

    /// The run of fence characters on a fence line, after its leading whitespace.
    private static func fenceMarker(_ text: NSString, line: NSRange, character: Character) -> [Span] {
        let unit = character == "`" ? backtick : tilde
        var index = line.location
        let end = NSMaxRange(line)
        while index < end, text.character(at: index) == space || text.character(at: index) == tab { index += 1 }
        let start = index
        while index < end, text.character(at: index) == unit { index += 1 }
        guard index > start else { return [] }
        return [Span(range: NSRange(location: start, length: index - start), style: .marker)]
    }

    // MARK: - Inline

    private static let backtick: unichar = 0x60
    private static let tilde: unichar = 0x7E
    private static let asterisk: unichar = 0x2A
    private static let underscore: unichar = 0x5F
    private static let backslash: unichar = 0x5C
    private static let newline: unichar = 0x0A
    private static let space: unichar = 0x20
    private static let tab: unichar = 0x09

    /// Code spans first, by CommonMark's rule (a run of N backticks closes on the next run of
    /// exactly N; an unmatched run is literal), then emphasis over what is left: a delimiter
    /// inside a code span is never emphasis.
    private static func inline(_ text: NSString, in range: NSRange) -> [Span] {
        var units = [unichar](repeating: 0, count: range.length)
        text.getCharacters(&units, range: range)
        let base = range.location
        let count = units.count
        var spans: [Span] = []
        var opaque = [Bool](repeating: false, count: count)

        func runLength(_ at: Int, _ unit: unichar) -> Int {
            var end = at
            while end < count, units[end] == unit { end += 1 }
            return end - at
        }

        var i = 0
        while i < count {
            let unit = units[i]
            if unit == backslash, i + 1 < count, isASCIIPunctuation(units[i + 1]) {
                opaque[i + 1] = true
                i += 2
                continue
            }
            guard unit == backtick else { i += 1; continue }
            let length = runLength(i, backtick)
            var j = i + length
            var close: Int?
            while j < count {
                if units[j] == backtick {
                    let other = runLength(j, backtick)
                    if other == length { close = j; break }
                    j += other
                } else {
                    j += 1
                }
            }
            guard let close else { i += length; continue }
            let end = close + length
            spans.append(Span(range: NSRange(location: base + i, length: end - i), style: .code))
            spans.append(Span(range: NSRange(location: base + i, length: length), style: .marker))
            spans.append(Span(range: NSRange(location: base + close, length: length), style: .marker))
            for k in i..<end { opaque[k] = true }
            i = end
        }

        spans += emphasis(units, opaque: opaque, base: base)
        return spans
    }

    /// CommonMark's delimiter run algorithm, cut down to `*` and `_`: flanking decides which
    /// runs may open and close, a closer matches the nearest compatible opener, two delimiters
    /// make bold and one makes italic.
    private static func emphasis(_ units: [unichar], opaque: [Bool], base: Int) -> [Span] {
        struct Run {
            let start: Int
            let length: Int
            let unit: unichar
            let canOpen: Bool
            let canClose: Bool
            /// Delimiters not yet used, taken from the inner end (an opener's right, a
            /// closer's left).
            var remaining: Int
            var consumedFromStart = 0
        }
        let count = units.count
        func isSpace(_ at: Int) -> Bool {
            guard at >= 0, at < count else { return true }
            let unit = units[at]
            return unit == space || unit == tab || unit == newline || unit == 0x0D
        }
        func isPunctuation(_ at: Int) -> Bool {
            guard at >= 0, at < count else { return false }
            return isASCIIPunctuation(units[at])
        }

        var runs: [Run] = []
        var i = 0
        while i < count {
            let unit = units[i]
            guard (unit == asterisk || unit == underscore), !opaque[i] else { i += 1; continue }
            var end = i
            while end < count, units[end] == unit, !opaque[end] { end += 1 }
            let before = i - 1, after = end
            let left = !isSpace(after) && (!isPunctuation(after) || isSpace(before) || isPunctuation(before))
            let right = !isSpace(before) && (!isPunctuation(before) || isSpace(after) || isPunctuation(after))
            let canOpen = unit == asterisk ? left : left && (!right || isPunctuation(before))
            let canClose = unit == asterisk ? right : right && (!left || isPunctuation(after))
            runs.append(Run(start: i, length: end - i, unit: unit, canOpen: canOpen,
                            canClose: canClose, remaining: end - i))
            i = end
        }

        var spans: [Span] = []
        var openers: [Int] = []
        for index in runs.indices {
            if runs[index].canClose {
                while runs[index].remaining > 0,
                      let position = openers.lastIndex(where: { runs[$0].unit == runs[index].unit
                          && runs[$0].remaining > 0 }) {
                    let opener = openers[position]
                    let use = runs[opener].remaining >= 2 && runs[index].remaining >= 2 ? 2 : 1
                    let openAt = runs[opener].start + runs[opener].remaining - use
                    let closeAt = runs[index].start + runs[index].consumedFromStart
                    spans.append(Span(range: NSRange(location: base + openAt, length: closeAt + use - openAt),
                                      style: use == 2 ? .bold : .italic))
                    spans.append(Span(range: NSRange(location: base + openAt, length: use), style: .marker))
                    spans.append(Span(range: NSRange(location: base + closeAt, length: use), style: .marker))
                    runs[opener].remaining -= use
                    runs[index].remaining -= use
                    runs[index].consumedFromStart += use
                    // Openers between this pair can no longer match anything past it.
                    openers.removeSubrange((position + 1)...)
                    if runs[opener].remaining == 0 { openers.remove(at: position) }
                }
            }
            if runs[index].remaining > 0, runs[index].canOpen { openers.append(index) }
        }
        return spans
    }

    private static func isASCIIPunctuation(_ unit: unichar) -> Bool {
        (0x21...0x2F).contains(unit) || (0x3A...0x40).contains(unit)
            || (0x5B...0x60).contains(unit) || (0x7B...0x7E).contains(unit)
    }
}

/// Puts `ComposerMarkdown`'s spans on the composer's screen. See the top of this file for
/// which mechanism carries what, and why none of them changes a character.
@MainActor
final class ComposerMarkdownStyler: NSObject, NSTextStorageDelegate {
    /// Past this many characters there is no live styling at all, so a big paste costs what
    /// it cost before styling existed.
    static let characterLimit = 20_000

    static var baseFont: NSFont { ComposerNSTextView.composerFont }
    /// The composer's own size, not the reply's 0.92: at the body size SF Mono lays out to the
    /// same 16 point line as SF, so a line of code never changes the composer's height.
    static var codeFont: NSFont {
        NSFont.monospacedSystemFont(ofSize: baseFont.pointSize, weight: .regular)
    }
    // Computed, and so resolved at draw time against the view's appearance. A stored copy of
    // a system colour with its alpha changed is fixed at the appearance current when it was
    // made, which drew the chips white on a light composer.
    static var chipFill: NSColor { NSColor.secondaryLabelColor.withAlphaComponent(0.12) }
    static var fenceFill: NSColor { NSColor.secondaryLabelColor.withAlphaComponent(0.06) }
    static var markerColor: NSColor { NSColor.secondaryLabelColor }

    weak var textView: NSTextView?

    /// The last scan, for drawing: code spans for chips, fences for the tint.
    private(set) var codeSpans: [NSRange] = []
    private(set) var fenceBlocks: [NSRange] = []
    /// Whether the storage holds any styling now, so dropping it past the limit is done once.
    private(set) var isStyled = false
    /// Instrumentation: the characters the last restyle reset and restyled. A test reads it to
    /// prove a keystroke restyles its paragraph, not the whole text.
    private(set) var lastRestyled = NSRange(location: 0, length: 0)

    /// Character edits since the last restyle: their union (current coordinates), where the
    /// first one began, and the net change in length.
    private var pending: NSRange?
    private var pendingStart = Int.max
    private var pendingDelta = 0

    nonisolated func textStorage(_ textStorage: NSTextStorage,
                                 didProcessEditing editedMask: NSTextStorageEditActions,
                                 range editedRange: NSRange, changeInLength delta: Int) {
        guard editedMask.contains(.editedCharacters) else { return }
        // The storage is this view's own and is only edited on the main thread.
        let length = textStorage.length
        MainActor.assumeIsolated {
            if let previous = pending {
                let start = min(previous.location, length)
                let clamped = NSRange(location: start, length: min(NSMaxRange(previous), length) - start)
                pending = NSUnionRange(clamped, editedRange)
            } else {
                pending = editedRange
            }
            pendingStart = min(pendingStart, editedRange.location)
            pendingDelta += delta
        }
    }

    /// Bring the styling up to date with the text. Called after every change to it.
    func restyle() {
        guard let textView, let storage = textView.textStorage,
              let manager = textView.layoutManager else { return }
        // Marked text (an input method mid composition) is restyled once it is committed.
        if textView.hasMarkedText() { return }
        let text = storage.mutableString
        defer { pending = nil; pendingStart = .max; pendingDelta = 0 }

        if text.length > Self.characterLimit {
            if isStyled { clear(storage, manager) }
            codeSpans = []
            fenceBlocks = []
            lastRestyled = NSRange(location: 0, length: 0)
            textView.needsDisplay = true
            return
        }

        let spans = ComposerMarkdown.scan(text)
        let fences = spans.filter { $0.style == .fence }.map(\.range)
        let full = NSRange(location: 0, length: text.length)
        var region: NSRange
        if !isStyled || pending == nil {
            region = full
        } else {
            region = ComposerMarkdown.paragraphRange(text, around: pending!)
            // A fence the edit opened, closed or moved restyles whole.
            let shifted = Set(fenceBlocks.map { shift($0) })
            for fence in Set(fences).symmetricDifference(shifted) {
                region = NSUnionRange(region, NSIntersectionRange(fence, full))
            }
        }

        storage.beginEditing()
        storage.addAttribute(.font, value: Self.baseFont, range: region)
        for span in spans where span.style == .bold || span.style == .italic {
            let hit = NSIntersectionRange(span.range, region)
            guard hit.length > 0 else { continue }
            let trait: NSFontTraitMask = span.style == .bold ? .boldFontMask : .italicFontMask
            storage.enumerateAttribute(.font, in: hit) { value, run, _ in
                let font = (value as? NSFont) ?? Self.baseFont
                storage.addAttribute(.font, value: NSFontManager.shared.convert(font, toHaveTrait: trait),
                                     range: run)
            }
        }
        for span in spans where span.style == .code || span.style == .fence {
            let hit = NSIntersectionRange(span.range, region)
            if hit.length > 0 { storage.addAttribute(.font, value: Self.codeFont, range: hit) }
        }
        storage.endEditing()

        manager.removeTemporaryAttribute(.foregroundColor, forCharacterRange: region)
        for span in spans where span.style == .marker {
            let hit = NSIntersectionRange(span.range, region)
            if hit.length > 0 {
                manager.addTemporaryAttribute(.foregroundColor, value: Self.markerColor,
                                              forCharacterRange: hit)
            }
        }

        codeSpans = spans.filter { $0.style == .code }.map(\.range)
        fenceBlocks = fences
        isStyled = true
        lastRestyled = region
        textView.needsDisplay = true
    }

    /// A range from before the pending edits, in today's coordinates: a range holding the
    /// first edit grows or shrinks with it, a range after it moves.
    private func shift(_ range: NSRange) -> NSRange {
        guard pendingStart != .max else { return range }
        if range.location <= pendingStart, pendingStart <= NSMaxRange(range) {
            return NSRange(location: range.location, length: max(range.length + pendingDelta, 0))
        }
        if range.location > pendingStart {
            return NSRange(location: max(range.location + pendingDelta, 0), length: range.length)
        }
        return range
    }

    /// Drop every face and dimmed marker, touching only the runs that carry one.
    private func clear(_ storage: NSTextStorage, _ manager: NSLayoutManager) {
        let full = NSRange(location: 0, length: storage.length)
        var styled: [NSRange] = []
        storage.enumerateAttribute(.font, in: full, options: .longestEffectiveRangeNotRequired) { value, run, _ in
            if (value as? NSFont) != Self.baseFont { styled.append(run) }
        }
        if !styled.isEmpty {
            storage.beginEditing()
            for run in styled { storage.addAttribute(.font, value: Self.baseFont, range: run) }
            storage.endEditing()
        }
        manager.removeTemporaryAttribute(.foregroundColor, forCharacterRange: full)
        isStyled = false
    }

    // MARK: - Drawing

    /// Draw the fence tints and code chips that intersect `rect`, under the text.
    func drawBackground(in rect: NSRect) {
        guard let textView, let manager = textView.layoutManager,
              let container = textView.textContainer else { return }
        let origin = textView.textContainerOrigin
        let visible = manager.glyphRange(forBoundingRect: rect.offsetBy(dx: -origin.x, dy: -origin.y),
                                         in: container)
        let visibleCharacters = manager.characterRange(forGlyphRange: visible, actualGlyphRange: nil)
        Self.fenceFill.setFill()
        for fence in fenceBlocks where NSIntersectionRange(fence, visibleCharacters).length > 0 {
            for line in fenceRects(for: fence) { line.fill() }
        }
        Self.chipFill.setFill()
        let radius = MarkdownDocument.Metrics.chipCornerRadius
        for span in codeSpans where NSIntersectionRange(span, visibleCharacters).length > 0 {
            for chip in chipRects(for: span) {
                NSBezierPath(roundedRect: chip, xRadius: radius, yRadius: radius).fill()
            }
        }
    }

    /// One chip per line fragment of a code span, its glyphs plus the chip padding, in view
    /// coordinates. The same geometry as the reply's chips.
    func chipRects(for range: NSRange) -> [NSRect] {
        guard let textView, let manager = textView.layoutManager,
              let container = textView.textContainer, range.length > 0 else { return [] }
        let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        let pad = MarkdownDocument.Metrics.chipPadding
        let origin = textView.textContainerOrigin
        var chips: [NSRect] = []
        manager.enumerateLineFragments(forGlyphRange: glyphs) { _, used, _, lineGlyphs, _ in
            let piece = NSIntersectionRange(lineGlyphs, glyphs)
            guard piece.length > 0 else { return }
            let ink = manager.boundingRect(forGlyphRange: piece, in: container)
            let minX = max(ink.minX - pad, 0)
            let maxX = min(ink.maxX + pad, container.size.width)
            guard maxX > minX else { return }
            chips.append(NSRect(x: minX + origin.x, y: used.minY + origin.y,
                                width: maxX - minX, height: used.height))
        }
        return chips
    }

    /// The full width rects behind a fence's lines, in view coordinates.
    func fenceRects(for range: NSRange) -> [NSRect] {
        guard let textView, let manager = textView.layoutManager, range.length > 0 else { return [] }
        let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        let origin = textView.textContainerOrigin
        var rects: [NSRect] = []
        manager.enumerateLineFragments(forGlyphRange: glyphs) { line, _, _, _, _ in
            rects.append(NSRect(x: 0, y: line.minY + origin.y, width: textView.bounds.width,
                                height: line.height))
        }
        return rects
    }
}
