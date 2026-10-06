import UIKit
import JesseNetworking

// The iOS half of the paste fallback: a pasteboard item the provider vends only as a
// `UIImage` (no concrete data representation) is re-encoded to PNG here. Everything else
// about a paste (the type order, the verbatim rule, the name) is `ComposerPaste` and
// `PasteAttachment` in JesseNetworking, shared with the Mac, whose own reader adds the
// `NSImage` equivalent beside its text view.
extension PasteAttachment {
    /// Re-encode a decoded image (e.g. a bitmap pasted with no lossless original) to PNG
    /// bytes, so its magic bytes sniff as a whitelisted `image/png`.
    static func pngData(from image: UIImage) -> Data? { image.pngData() }
}
