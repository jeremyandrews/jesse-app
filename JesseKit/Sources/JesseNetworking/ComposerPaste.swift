import Foundation
import UniformTypeIdentifiers

// The pure core of "paste a copied photo or PDF into the composer", shared by both apps.
//
// Each composer's text view offers the native Paste, and when the clipboard holds an image
// or a PDF it stages it as an attachment (the same chip + send flow the paperclip menu
// uses) instead of dropping raw bytes into the text. The pasteboard I/O lives in each
// platform's view (`UIPasteboard` item providers on iOS, `NSPasteboard` on the Mac); these
// are the side-effect-free decisions, so they're unit-tested without touching a global
// pasteboard.
public enum ComposerPaste {
    /// Type identifiers tried, in order, when reading a pasted item's ORIGINAL bytes. The
    /// loop is keyed on whether the item actually carries the type, so a JPEG/HEIC photo
    /// (which does not conform to `public.png`) loads its own compact bytes verbatim and is
    /// never re-encoded to a much larger PNG — the regression that made pasted photos trip
    /// the per-file size cap. A bare bitmap with no concrete encoding falls back to a
    /// re-encoded image in the platform reader.
    public static let mediaTypes: [UTType] = [.pdf, .png, .jpeg, .heic, .heif, .gif, .webP, .tiff, .bmp]

    /// Whether the composer should treat a paste as *media* (stage an attachment) rather
    /// than let the text view paste text. True when the clipboard carries an image or a
    /// PDF; a text-only clipboard pastes as text as usual.
    public static func isMediaPaste(hasImages: Bool, hasPDF: Bool) -> Bool {
        hasImages || hasPDF
    }
}
