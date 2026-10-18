// The only platform difference the shared reply code sees: which font and colour classes
// it builds with, and how a text style resolves to a size.
//
// `MarkdownDocument` and `MarkdownInline` compile for the iPhone and the Mac from the same
// source. They name `PlatformFont`, `PlatformColor` and the few helpers below, and never
// `UIFont` or `NSFont` directly, so this file is the whole of the `#if`.

#if os(iOS)
import UIKit

typealias PlatformFont = UIFont
typealias PlatformColor = UIColor
/// What a reply's fonts are resolved against: the Dynamic Type size, on the phone.
typealias MarkdownFontTraits = UITraitCollection

extension PlatformFont {
    /// The system font for `style`, at `traits`' Dynamic Type size when given.
    static func markdownFont(_ style: TextStyle, traits: MarkdownFontTraits?) -> PlatformFont {
        if let traits { return preferredFont(forTextStyle: style, compatibleWith: traits) }
        return preferredFont(forTextStyle: style)
    }

    /// This font with `bold` and `italic` added to the traits it already has, or, when
    /// `merging` is false, with exactly those traits in place of its own.
    func markdownAdding(bold: Bool, italic: Bool, merging: Bool = true) -> PlatformFont {
        var traits: UIFontDescriptor.SymbolicTraits = []
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        guard !traits.isEmpty else { return self }
        let merged = merging ? fontDescriptor.symbolicTraits.union(traits) : traits
        guard let descriptor = fontDescriptor.withSymbolicTraits(merged) else { return self }
        return PlatformFont(descriptor: descriptor, size: pointSize)
    }
}

extension PlatformColor {
    /// The primary text colour.
    static var markdownLabel: PlatformColor { .label }
}

#elseif os(macOS)
import AppKit

typealias PlatformFont = NSFont
typealias PlatformColor = NSColor

/// What a reply's fonts are resolved against on the Mac, which has no Dynamic Type: the
/// system text styles, scaled by `scale` (1 is the system's own sizes).
struct MarkdownFontTraits: Equatable {
    var scale: CGFloat = 1
}

extension PlatformFont {
    /// The system font for `style`, scaled by `traits` when given.
    static func markdownFont(_ style: TextStyle, traits: MarkdownFontTraits?) -> PlatformFont {
        let font = preferredFont(forTextStyle: style)
        guard let scale = traits?.scale, scale != 1 else { return font }
        return PlatformFont(descriptor: font.fontDescriptor, size: font.pointSize * scale) ?? font
    }

    /// This font with `bold` and `italic` added to the traits it already has, or, when
    /// `merging` is false, with exactly those traits in place of its own.
    func markdownAdding(bold: Bool, italic: Bool, merging: Bool = true) -> PlatformFont {
        var traits: NSFontDescriptor.SymbolicTraits = []
        if bold { traits.insert(.bold) }
        if italic { traits.insert(.italic) }
        guard !traits.isEmpty else { return self }
        let merged = merging ? fontDescriptor.symbolicTraits.union(traits) : traits
        return PlatformFont(descriptor: fontDescriptor.withSymbolicTraits(merged), size: pointSize)
            ?? self
    }
}

extension PlatformColor {
    /// The primary text colour.
    static var markdownLabel: PlatformColor { .labelColor }
}
#endif
