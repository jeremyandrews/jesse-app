import Foundation

// WHERE THE READER WAS, SAID IN THE ONE UNIT BOTH SCREENS SHARE.
//
// The reader draws blocks and the editor draws characters, so neither can hand the other a
// position in its own terms. Both know the WHOLE FILE LINE: a block's `line` is counted
// from the top of the file, frontmatter included (the parser is started at the line after
// the frontmatter's closing fence), and a raw line is a file line by construction. So the
// reader says "line 40", and the editor turns 40 into a `NSRange` against ITS OWN TEXT,
// never against the reader's parsed copy, which may be minutes old.
//
// Over UTF-16, because `NSRange` is UTF-16 and a count of `Character`s is wrong by one for
// every emoji and every "\r\n" (a single grapheme) before the line. A line break is a
// "\n"; a CRLF file's "\r" stays at the end of its line, which is where a caret at the
// start of the NEXT line is unaffected by it.

/// Where the editor opens: at a line, or with some lines selected.
public enum VaultEditorStart: Equatable, Sendable {
    /// A caret at the start of this 1-based file line.
    case caret(line: Int)
    /// These 1-based file lines selected, first character to last, line break excluded.
    case select(lines: ClosedRange<Int>)

    /// The line the editor scrolls to the top.
    public var firstLine: Int {
        switch self {
        case .caret(let line): return line
        case .select(let lines): return lines.lowerBound
        }
    }
}

/// Line and UTF-16 arithmetic over one text. Pure.
public enum VaultNotePosition {
    private static let newline = UInt16(0x0A)

    /// The UTF-16 offset where 1-based `line` starts.
    ///
    /// BEST EFFORT, never a crash: a line below 1 is the top, and a line past the end (the
    /// note shrank on disk between the reader and the editor) is the last line's start.
    public static func offset(ofLine line: Int, in text: String) -> Int {
        guard line > 1 else { return 0 }
        var current = 1
        var lastStart = 0
        for (offset, unit) in text.utf16.enumerated() where unit == newline {
            current += 1
            lastStart = offset + 1
            if current == line { return lastStart }
        }
        return lastStart
    }

    /// The UTF-16 offset where the line holding `start` ends, before its break (and before a
    /// CRLF's "\r").
    static func endOfLine(from start: Int, in text: String) -> Int {
        let units = text.utf16
        var index = units.index(units.startIndex, offsetBy: min(start, units.count))
        var offset = start
        while index != units.endIndex, units[index] != newline {
            units.formIndex(after: &index)
            offset += 1
        }
        if offset > start, units[units.index(before: index)] == 0x0D { offset -= 1 }
        return offset
    }

    /// The selection a start asks for, in `text`'s own UTF-16 units.
    public static func range(for start: VaultEditorStart, in text: String) -> NSRange {
        switch start {
        case .caret(let line):
            return NSRange(location: offset(ofLine: line, in: text), length: 0)
        case .select(let lines):
            let from = offset(ofLine: lines.lowerBound, in: text)
            let lastStart = max(from, offset(ofLine: lines.upperBound, in: text))
            let to = endOfLine(from: lastStart, in: text)
            return NSRange(location: from, length: max(0, to - from))
        }
    }

    /// The 1-based line a UTF-16 offset is on. An offset past the end is on the last line.
    public static func line(atOffset location: Int, in text: String) -> Int {
        var line = 1
        for (offset, unit) in text.utf16.enumerated() {
            if offset >= location { break }
            if unit == newline { line += 1 }
        }
        return line
    }
}

extension VaultNoteDocument {
    /// The file lines block `id` was parsed from: its first line through the line before the
    /// next block, less any trailing blank lines. Counted over `rawLines`, which is the file.
    public func sourceLines(ofBlock id: Int) -> ClosedRange<Int>? {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return nil }
        let first = blocks[index].line
        var last = index + 1 < blocks.count ? blocks[index + 1].line - 1 : rawLines.count
        last = min(last, rawLines.count)
        while last > first,
              rawLines[last - 1].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            last -= 1
        }
        return first...max(first, last)
    }

    /// The id of the block `line` falls INSIDE: the last block starting at or before it,
    /// and the first block when the line is above every block (the frontmatter).
    ///
    /// Not `blockID(forLine:in:)`, which answers "at or after" for a search hit that names a
    /// chunk's first line. A caret in the middle of a long paragraph is IN that paragraph,
    /// and "at or after" would put the reader on the block below it.
    public static func blockID(containingLine line: Int, in blocks: [VaultNoteBlock]) -> Int? {
        guard let first = blocks.first else { return nil }
        return blocks.last(where: { $0.line <= line })?.id ?? first.id
    }
}
