import SwiftUI
import UIKit
import JesseCore

// The iOS rendering of a reply. The parser (`parseMarkdownBlocks`, `MarkdownBlock`,
// `MarkdownStreamRenderer`) lives in `JesseReply/ReplyMarkdownBlocks.swift`, which the Mac
// compiles too.

/// Renders parsed markdown blocks.
///
/// Two rendering modes, chosen by `selectable`:
/// - **Selectable** (persisted turns): the whole reply is composed into ONE
///   `MarkdownDocument` and rendered by ONE text view, so a selection is a plain
///   character range over one text storage — it spans paragraphs, headings, list
///   items, code and table cells, and `Select All` covers the reply. This is the
///   fix for the selection islands the old per-block `SelectableText` layout
///   created; `MarkdownDocument` carries the full explanation.
/// - **Non-selectable** (the live streaming partial): the lightweight SwiftUI
///   `Text` path, unchanged — the growing partial re-renders ~10×/s and needs no
///   selection, so it avoids rebuilding an attributed document mid-stream. When
///   the turn finishes, `TurnRow` renders the persisted `Turn` through the
///   selectable path above.
struct MarkdownText: View {
    let blocks: [MarkdownBlock]
    let selectable: Bool

    /// The document's fonts are resolved at this size and rebuilt when it
    /// changes — a baked `NSAttributedString` cannot rescale itself, and the
    /// table's tab stops are measured from the text at a specific size.
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    init(_ raw: String, selectable: Bool = true) {
        self.blocks = parseMarkdownBlocks(raw)
        self.selectable = selectable
    }

    /// Render pre-parsed blocks directly — used by the throttled streaming path
    /// (`MarkdownStreamRenderer`) so the parse isn't re-run inside the view body.
    /// Defaults to non-selectable (the streaming partial).
    init(blocks: [MarkdownBlock], selectable: Bool = false) {
        self.blocks = blocks
        self.selectable = selectable
    }

    var body: some View {
        if selectable {
            SelectableDocumentText(blocks: blocks, typeSize: dynamicTypeSize)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                    view(for: block)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .textSelection(.enabled)
        }
    }

    /// The streaming path's per-block SwiftUI views. The selectable path does not
    /// come through here at all — it has no per-block views to build.
    @ViewBuilder
    private func view(for block: MarkdownBlock) -> some View {
        switch block {
        case let .heading(level, text):
            inline(text).font(headingFont(level))

        case let .bullet(text):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("•")
                inline(text)
            }

        case let .numbered(number, text):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("\(number).")
                inline(text)
            }

        case let .code(code):
            codeBlock {
                Text(code)
                    .font(.system(.body, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }

        case let .table(headers, rows, alignments):
            tableView(headers: headers, rows: rows, alignments: alignments)

        case let .paragraph(text):
            inline(text)
        }
    }

    /// The shared code-block chrome (monospaced content on a shaded card).
    @ViewBuilder
    private func codeBlock<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.secondary.opacity(0.12))
            )
    }

    // The equivalent UIKit fonts for the selectable path live in
    // `MarkdownDocument`'s builder, which needs them as concrete `UIFont`s to
    // bake into the attributed string.

    /// Render a GFM pipe table as a SwiftUI `Grid` inside a horizontal
    /// `ScrollView` so a wide table scrolls rather than truncating. Ragged rows
    /// (fewer cells than headers) pad with empties; extra cells are dropped.
    @ViewBuilder
    private func tableView(headers: [String], rows: [[String]], alignments: [TableAlignment]) -> some View {
        let columns = headers.count
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .topLeading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(0..<columns, id: \.self) { col in
                        cell(headers[col], alignment: alignment(alignments, col))
                            .fontWeight(.semibold)
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(0..<columns, id: \.self) { col in
                            cell(col < row.count ? row[col] : "",
                                 alignment: alignment(alignments, col))
                        }
                    }
                }
            }
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.secondary.opacity(0.12))
            )
        }
    }

    /// One table cell: inline-styled text, padded and aligned per its column.
    private func cell(_ text: String, alignment: Alignment) -> some View {
        inline(text)
            .padding(.vertical, 2)
            .padding(.horizontal, 6)
            .frame(maxWidth: .infinity, alignment: alignment)
    }

    private func alignment(_ alignments: [TableAlignment], _ col: Int) -> Alignment {
        switch col < alignments.count ? alignments[col] : .leading {
        case .leading:  return .leading
        case .center:   return .center
        case .trailing: return .trailing
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1:  return .title3.weight(.semibold)
        case 2:  return .headline.weight(.semibold)
        default: return .subheadline.weight(.semibold)
        }
    }

    /// Inline styling within a block. Parses bold/italic/`code`/links while
    /// preserving whitespace; falls back to the plain string on any failure.
    private func inline(_ s: String) -> Text {
        let attributed = (try? AttributedString(
            markdown: s,
            options: .init(
                interpretedSyntax: .inlineOnlyPreservingWhitespace,
                failurePolicy: .returnPartiallyParsedIfPossible
            )
        )) ?? AttributedString(s)
        return Text(attributed)
    }
}
