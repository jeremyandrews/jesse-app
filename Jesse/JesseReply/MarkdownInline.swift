#if os(iOS)
import UIKit
#else
import AppKit
#endif
import JesseVault

// Inline markdown → `NSAttributedString` with *concrete* platform fonts and colors (`UIFont`
// on the phone, `NSFont` on the Mac; see `MarkdownPlatform`).
//
// `AttributedString(markdown:)` records bold / italic / code only as SEMANTIC
// `inlinePresentationIntent` runs; it does not set a bold font. SwiftUI's
// `Text` resolves those intents itself, but a text view (which we use for
// native, granular text selection) needs real font attributes. This resolves the
// intents to actual font traits + link attributes so a text view renders the
// same bold/italic/`code`/links a `Text` would.
//
// Pure and side-effect-free (the font/color resolution is the testable core).
enum MarkdownInline {
    /// One inline-markdown string as an `NSAttributedString` styled with `font`
    /// (the base body/heading font) and `color`. Bold/italic/`code`/links are
    /// resolved to concrete attributes; on a parse failure the raw string is
    /// returned with the base attributes so text is never lost.
    static func attributed(_ s: String, font: PlatformFont, color: PlatformColor) -> NSAttributedString {
        // `[[WIKI LINKS]]` FIRST, and as a rewrite rather than a case in the loop below.
        // Foundation's parser knows `[text](url)` and knows nothing about double
        // brackets, so a reply naming `[[todo-list/Strands/Strands-System]]` used to
        // arrive as literal brackets around a path: the one run in a reply that names a
        // real file on this device, and the one you could not act on. Rewriting it into
        // an ordinary markdown link before the parse means the `.link` attribution,
        // the font resolution and the macOS renderer all get it for free — see
        // `VaultWikiMarkdown`, which is the one scanner both platforms share.
        let s = VaultWikiMarkdown.linked(s)
        guard let parsed = try? AttributedString(
            markdown: s,
            options: .init(
                interpretedSyntax: .inlineOnlyPreservingWhitespace,
                failurePolicy: .returnPartiallyParsedIfPossible)) else {
            return NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: color])
        }

        let result = NSMutableAttributedString()
        for run in parsed.runs {
            let text = String(parsed[run.range].characters)
            let intent = run.inlinePresentationIntent ?? []
            var attrs: [NSAttributedString.Key: Any] = [
                .font: resolvedFont(base: font, intent: intent),
                .foregroundColor: color,
            ]
            if let link = run.link {
                // The text view colors links via its `tintColor`; the attribute
                // just makes the run a tappable link.
                attrs[.link] = link
            }
            result.append(NSAttributedString(string: text, attributes: attrs))
        }
        return result
    }

    /// A list item (`•` / `1.`) as an `NSAttributedString` with a hanging indent,
    /// so wrapped lines align under the text rather than under the marker —
    /// matching the SwiftUI bullet/number column layout.
    static func listItem(marker: String, text: String, font: PlatformFont,
                         color: PlatformColor, indent: CGFloat = 20) -> NSAttributedString {
        let style = NSMutableParagraphStyle()
        style.headIndent = indent
        style.firstLineHeadIndent = 0
        style.tabStops = [NSTextTab(textAlignment: .left, location: indent)]
        style.defaultTabInterval = indent

        let result = NSMutableAttributedString(
            string: "\(marker)\t", attributes: [.font: font, .foregroundColor: color])
        result.append(attributed(text, font: font, color: color))
        result.addAttribute(.paragraphStyle, value: style,
                            range: NSRange(location: 0, length: result.length))
        return result
    }

    /// The base `font` with the bold/italic/monospaced traits implied by `intent`.
    /// `code` switches to a monospaced face at the same point size (carrying any
    /// bold/italic that is also set); otherwise bold/italic are layered onto the
    /// base descriptor's existing traits.
    static func resolvedFont(base: PlatformFont, intent: InlinePresentationIntent) -> PlatformFont {
        let bold = intent.contains(.stronglyEmphasized)
        let italic = intent.contains(.emphasized)
        if intent.contains(.code) {
            // A code span replaces the base face's traits with its own, rather than merging
            // into them: it is a different face, carrying only the emphasis also set.
            return PlatformFont.monospacedSystemFont(ofSize: base.pointSize, weight: .regular)
                .markdownAdding(bold: bold, italic: italic, merging: false)
        }
        return base.markdownAdding(bold: bold, italic: italic)
    }
}
