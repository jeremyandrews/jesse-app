import Foundation
import ImageIO

/// Turning a pasted/clipboard payload into stageable attachment bytes, on either platform.
///
/// The pasteboard reading itself is I/O and lives in each platform's view; these are the
/// pure, testable core:
///
/// * A copied PNG / JPEG / GIF / WebP / HEIC / PDF that already carries whitelisted
///   magic bytes is kept **verbatim** (lossless — the original bytes are what we stage).
/// * A bitmap with no lossless original (e.g. a copied screenshot the pasteboard only
///   offers as TIFF/BMP) is re-encoded to PNG, because `JesseAttachment.sniffMime` keys
///   off magic bytes — whatever we hand it must actually be a whitelisted type.
/// * Anything that is neither a whitelisted type nor a decodable bitmap → `nil`, which
///   the caller surfaces through its error line.
///
/// The staged bytes then flow through `AttachmentStaging.stage`, the same path the pickers
/// use, so they inherit `sniffMime`, `AttachmentLimits`, the chip UI, and send. The one
/// per-platform piece is `pngData(from:)` for a decoded platform image, which each app adds
/// beside its own pasteboard reader.
public enum PasteAttachment {
    /// A generated, sortable filename for a pasted item, e.g.
    /// `pasted-20260704-141530.png`. POSIX locale + fixed format so it's stable
    /// and testable (no hidden clock read).
    public static func filename(ext: String, date: Date = Date()) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return "pasted-\(formatter.string(from: date)).\(ext)"
    }

    /// Stageable bytes for a raw pasted payload, or `nil` if it can't be represented as a
    /// whitelisted type. Whitelisted input is returned verbatim; a decodable bitmap with a
    /// non-whitelisted encoding is re-encoded to PNG through ImageIO.
    public static func stageableBytes(from data: Data) -> Data? {
        if JesseAttachment.sniffMime(data) != nil { return data }
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0,
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { return nil }
        return AttachmentImageEncoding.png(image)
    }

    /// Stageable bytes and a generated `pasted-<timestamp>.<ext>` name for a raw payload, or
    /// nil when it cannot be staged. What both paste readers hand to `AttachmentStaging`.
    public static func named(_ data: Data, date: Date = Date()) -> (data: Data, filename: String)? {
        guard let staged = stageableBytes(from: data) else { return nil }
        let ext = JesseAttachment.sniffMime(staged).map(JesseAttachment.fileExtension(forMime:)) ?? "png"
        return (staged, filename(ext: ext, date: date))
    }
}
