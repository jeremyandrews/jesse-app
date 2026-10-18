import SwiftUI
import JesseVault

// The reply WHILE IT STREAMS, on the Mac: the shared parser's blocks drawn as lightweight
// SwiftUI views. Not the document view a finished reply gets, because the partial grows many
// times a second and needs no selection; when the turn finishes, `MacTurnBubble` renders the
// persisted reply through `MacDocumentText`. The parser is the same one, so a fence looks the
// same streaming as after.
//
// The parse is coalesced by the shared `MarkdownStreamRenderer` (at most one per interval,
// never one per token), with one catch-up re-render when a publish landed inside its cooldown:
// the same arrangement as the phone's `StreamingPartialText`. The Mac's interval is longer than
// the phone's, see `interval`.

struct MacStreamingMarkdown: View {
    let text: String

    /// Bumped by the catch-up task to force one more body evaluation, past the renderer's
    /// cooldown, so a suppressed tail is never left on screen.
    @State private var catchUps = 0
    @State private var renderer = MarkdownStreamRenderer(
        interval: MacStreamingMarkdown.interval,
        parse: { text in
            MacStreamingMarkdown.parses += 1
            return parseMarkdownBlocks(text)
        })

    /// At most four rebuilds a second. Measured with a 1 MB reply streamed in 2 KB chunks: at
    /// the phone's ten a second, a parse and render of the grown partial took longer than the
    /// interval (162 rebuilds in 18.5 s, about 114 ms each), so the main thread never caught
    /// up. The Mac publishes every chunk, with no coalescing upstream of this view.
    nonisolated static let interval: TimeInterval = 0.25

    /// Instrumentation: how many times the streaming partial has been parsed.
    static var parses = 0

    var body: some View {
        let blocks = renderer.blocks(for: text, now: Date())
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(for: block)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .task(id: CatchUpKey(text: text, catchUps: catchUps)) { await catchUpIfSuppressed() }
    }

    private struct CatchUpKey: Equatable {
        let text: String
        let catchUps: Int
    }

    private func catchUpIfSuppressed() async {
        guard !renderer.hasRendered(text) else { return }
        try? await Task.sleep(for: .seconds(Self.interval))
        guard !Task.isCancelled else { return }
        catchUps &+= 1
    }

    @ViewBuilder
    private func view(for block: MarkdownBlock) -> some View {
        switch block {
        case let .heading(level, text):
            Text(Self.inline(text)).font(Self.headingFont(level)).fontWeight(.semibold)
        case let .paragraph(text):
            Text(Self.inline(text)).fixedSize(horizontal: false, vertical: true)
        case let .bullet(text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("•").foregroundStyle(.secondary)
                Text(Self.inline(text)).fixedSize(horizontal: false, vertical: true)
            }
        case let .numbered(number, text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(number).").foregroundStyle(.secondary).monospacedDigit()
                Text(Self.inline(text)).fixedSize(horizontal: false, vertical: true)
            }
        case let .code(code):
            Text(code)
                .font(.system(.body, design: .monospaced))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 6))
        case let .table(headers, rows, _):
            Grid(alignment: .topLeading, horizontalSpacing: 12, verticalSpacing: 4) {
                GridRow {
                    ForEach(Array(headers.enumerated()), id: \.offset) { _, header in
                        Text(Self.inline(header)).fontWeight(.semibold)
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(0..<headers.count, id: \.self) { col in
                            Text(Self.inline(col < row.count ? row[col] : ""))
                        }
                    }
                }
            }
            .padding(8)
            .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 6))
        }
    }

    static func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: return .title3
        case 2: return .headline
        default: return .subheadline
        }
    }

    /// Inline emphasis via the system Markdown parser, after the same wiki link rewrite the
    /// finished reply gets (`MarkdownInline`). Falls back to plain text if the fragment does not
    /// parse.
    static func inline(_ s: String) -> AttributedString {
        let s = VaultWikiMarkdown.linked(s)
        return (try? AttributedString(
            markdown: s,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace,
                           failurePolicy: .returnPartiallyParsedIfPossible)))
            ?? AttributedString(s)
    }
}
