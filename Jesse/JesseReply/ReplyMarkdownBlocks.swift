import Foundation
// RE-EXPORTED on purpose. `TableAlignment` and the three table functions used by the parser
// below used to be declared in the iOS app; they now live in JesseMarkdown so the vault reader
// can use the same rules instead of growing a second answer to "what is a delimiter row".
// Re-exporting means every existing caller, `MarkdownDocument` and the parser's own tests,
// keeps compiling unchanged.
@_exported import JesseMarkdown

// The block parser for Jesse's markdown replies, shared by the iPhone and the Mac.
//
// SwiftUI's `Text` only interprets *inline* markdown and collapses newlines, so a
// multi-paragraph or bulleted answer comes out as a wall of text. The reply is split into
// blocks here (headings, bullets, numbered items, fenced code, tables, paragraphs) and each
// platform renders the blocks: `MarkdownDocument` composes them into one selectable document,
// and each platform's streaming view draws them as lightweight views.
//
// Parsing is kept separate from rendering so the parser is unit-testable.

/// One renderable block of a markdown reply.
enum MarkdownBlock: Equatable {
    case heading(level: Int, text: String)
    case bullet(text: String)
    case numbered(number: Int, text: String)
    case code(String)
    case table(headers: [String], rows: [[String]], alignments: [TableAlignment])
    case paragraph(String)
}

/// Split raw markdown into blocks. Pure — no SwiftUI, fully testable.
///
/// Rules:
/// - `#`/`##`/`###` (1–3 `#` then a space) → heading.
/// - `- `, `* `, or `+ ` → bullet.
/// - `<digits>. ` → numbered item.
/// - A fence opens a verbatim code block and only its own closing fence ends it, by the
///   CommonMark rule `MarkdownFence` holds for both parsers: three or more backticks or
///   tildes open; the same character, at least as many, and nothing else on the line close.
///   An info string line (three backticks then `swift`) inside a block is content, a shorter
///   or a different run is content, and an unclosed fence runs to the end of the reply.
///   Everything between is one `.code` block with the fences removed.
/// - Otherwise, consecutive non-blank lines join into one paragraph; a blank
///   line ends the current paragraph.
func parseMarkdownBlocks(_ raw: String) -> [MarkdownBlock] {
    var blocks: [MarkdownBlock] = []

    // Buffer for the paragraph currently being accumulated.
    var paragraphLines: [String] = []
    func flushParagraph() {
        guard !paragraphLines.isEmpty else { return }
        blocks.append(.paragraph(paragraphLines.joined(separator: "\n")))
        paragraphLines.removeAll()
    }

    // The fence that opened the code block in progress, if one is open.
    var openFence: MarkdownFence?
    var codeLines: [String] = []

    // Preserve trailing empty lines so blank-line separation is detected.
    // Index-based so a table can look ahead to its delimiter row and consume
    // its data rows in one step.
    let lines = raw.components(separatedBy: "\n")

    var i = 0
    while i < lines.count {
        let line = lines[i]
        let trimmed = line.trimmingCharacters(in: .whitespaces)

        // Inside a fence, only its own closing fence means anything.
        if let fence = openFence {
            if fence.isClosed(by: line) {
                // Closing fence: emit the accumulated code, fences excluded.
                blocks.append(.code(codeLines.joined(separator: "\n")))
                codeLines.removeAll()
                openFence = nil
            } else {
                codeLines.append(line)
            }
            i += 1
            continue
        }

        // An opening fence takes priority over everything else outside one.
        if let fence = MarkdownFence.opening(line) {
            // End any paragraph in progress first.
            flushParagraph()
            openFence = fence
            i += 1
            continue
        }

        // Blank line: paragraph separator.
        if trimmed.isEmpty {
            flushParagraph()
            i += 1
            continue
        }

        // Heading: 1–3 leading '#' followed by a space.
        if let heading = parseHeading(trimmed) {
            flushParagraph()
            blocks.append(heading)
            i += 1
            continue
        }

        // Bullet: '- ', '* ', or '+ '.
        if let bulletText = parseBullet(trimmed) {
            flushParagraph()
            blocks.append(.bullet(text: bulletText))
            i += 1
            continue
        }

        // Numbered: leading digits, then '.', then a space.
        if let numbered = parseNumbered(trimmed) {
            flushParagraph()
            blocks.append(numbered)
            i += 1
            continue
        }

        // GFM pipe table: a row-looking line (contains '|') IMMEDIATELY followed
        // by a delimiter row (cells of only '-', ':', spaces, with at least one
        // '-'). A '|'-containing line *not* followed by a delimiter is just prose
        // with pipes and falls through to the paragraph branch below.
        if trimmed.contains("|"),
           i + 1 < lines.count,
           isTableDelimiterRow(lines[i + 1]) {
            flushParagraph()
            let headers = splitTableRow(trimmed)
            let alignments = parseTableAlignments(lines[i + 1])
            var rows: [[String]] = []
            // Consume following '|'-containing lines as data rows until a
            // blank or non-row line ends the table.
            var j = i + 2
            while j < lines.count {
                let rowLine = lines[j].trimmingCharacters(in: .whitespaces)
                guard !rowLine.isEmpty, rowLine.contains("|") else { break }
                rows.append(splitTableRow(rowLine))
                j += 1
            }
            blocks.append(.table(headers: headers, rows: rows, alignments: alignments))
            i = j
            continue
        }

        // Plain text line — part of the current paragraph.
        paragraphLines.append(trimmed)
        i += 1
    }

    // Close out anything still open.
    flushParagraph()
    if openFence != nil {
        // Unterminated fence — still surface its content as code.
        blocks.append(.code(codeLines.joined(separator: "\n")))
    }

    return blocks
}

private func parseHeading(_ line: String) -> MarkdownBlock? {
    var level = 0
    let chars = Array(line)
    while level < chars.count && chars[level] == "#" { level += 1 }
    guard level >= 1, level <= 3, level < chars.count, chars[level] == " " else {
        return nil
    }
    let text = String(chars[(level + 1)...]).trimmingCharacters(in: .whitespaces)
    return .heading(level: level, text: text)
}

private func parseBullet(_ line: String) -> String? {
    for marker in ["- ", "* ", "+ "] where line.hasPrefix(marker) {
        return String(line.dropFirst(marker.count)).trimmingCharacters(in: .whitespaces)
    }
    return nil
}

private func parseNumbered(_ line: String) -> MarkdownBlock? {
    let chars = Array(line)
    var i = 0
    while i < chars.count && chars[i].isNumber { i += 1 }
    guard i > 0, i + 1 < chars.count, chars[i] == ".", chars[i + 1] == " " else {
        return nil
    }
    guard let number = Int(String(chars[0..<i])) else { return nil }
    let text = String(chars[(i + 2)...]).trimmingCharacters(in: .whitespaces)
    return .numbered(number: number, text: text)
}

// `isTableDelimiterRow`, `parseTableAlignments` and `splitTableRow` were here. They are
// now `JesseMarkdown`'s, re-exported by the import at the top of this file, and the table
// branch of `parseMarkdownBlocks` above calls them exactly as it always did. What did NOT
// move is this parser's own assembly of a `.table` block — its ragged rows stay ragged and
// its own tests still pin that.

/// Coalesces the markdown parse of a *growing* string (a live stream's partial
/// reply) to ~10 Hz. The naive `MarkdownText(partial)` re-parses the whole string
/// on every delta, so an N-delta stream parses O(N²) characters. This caps the
/// parse to at most once per `interval`: between parses it returns the last result,
/// so frequent deltas are cheap and the expensive `parseMarkdownBlocks` runs ≤10×/s
/// regardless of delta rate. The newest text always wins — the next tick past the
/// interval parses whatever the current text is — and the finished turn's persisted
/// Turn renders the complete text, so nothing is ever shown stale for long.
@MainActor
final class MarkdownStreamRenderer {
    /// ~10 Hz: at most one parse per 100 ms while deltas stream in. Also the
    /// TimelineView tick interval the streaming view drives this with. `nonisolated`
    /// so it can seed the init default argument (evaluated off the main actor).
    nonisolated static let interval: TimeInterval = 0.1

    private let interval: TimeInterval
    private let parse: @MainActor (String) -> [MarkdownBlock]
    private var cached: [MarkdownBlock] = []
    private var cachedSource: String?
    private var lastParseAt: Date?

    /// `parse` is injectable so a test can count calls; it defaults to the real
    /// `parseMarkdownBlocks`, resolved in the init body (a non-nil default argument
    /// would be formed off the main actor under MainActor-default isolation).
    init(interval: TimeInterval = MarkdownStreamRenderer.interval,
         parse: (@MainActor (String) -> [MarkdownBlock])? = nil) {
        self.interval = interval
        self.parse = parse ?? parseMarkdownBlocks
    }

    /// The blocks to render for `text` at `now`. Re-parses only when the text
    /// changed AND at least `interval` has elapsed since the last parse; otherwise
    /// returns the cached blocks unchanged (so the view diff is a no-op and no parse
    /// runs). Unchanged text never re-parses.
    /// Slack on the interval comparison so a publish arriving *at* the cadence the cap asks
    /// for is not rejected by floating-point drift. `RunCoordinator` publishes `partialText`
    /// exactly one `interval` apart, and without this a third of those land a fraction of a
    /// femtosecond early (`0.4 - 0.3 == 0.09999999999999998`) and are suppressed — each one
    /// then costing the streaming view a catch-up re-render for no reason.
    private static let boundarySlack: TimeInterval = 1e-9

    func blocks(for text: String, now: Date) -> [MarkdownBlock] {
        if text == cachedSource { return cached }
        if let last = lastParseAt,
           now.timeIntervalSince(last) < interval - Self.boundarySlack {
            return cached
        }
        cached = parse(text)
        cachedSource = text
        lastParseAt = now
        return cached
    }

    /// Whether `text` is what the cached blocks were parsed from — i.e. whether the last
    /// `blocks(for:now:)` call actually rendered it, or the cadence cap served the previous
    /// text instead.
    ///
    /// This is what lets the streaming view hold NO clock: it renders on each publish and
    /// arms a single catch-up re-render only when a publish was suppressed, instead of
    /// keeping a ticking timeline alive for the whole turn on the chance that one was.
    func hasRendered(_ text: String) -> Bool { text == cachedSource }
}
