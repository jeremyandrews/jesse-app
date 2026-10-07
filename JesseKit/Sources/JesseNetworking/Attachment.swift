import Foundation

// THE OUTGOING ATTACHMENT PIPELINE, ONCE, FOR BOTH APPS.
//
// A file the user stages with a turn, the client-side caps that mirror the bridge's, the ONE
// staging function every source on both platforms calls, and the mapping onto the wire. It
// lived in the iOS target folder until the Mac needed it, which is why the Mac target never
// compiled it and its send hard-coded no attachments: the `Jesse Mac` target compiles only
// its own folder plus this package.
//
// It is in JesseNetworking rather than JesseCore for three reasons, none of which adds a
// dependency edge: both app targets already link this module, the wire type it maps onto
// (`JesseRequest.Attachment`) and the frugal policy the downscaler reads (`FrugalPolicy`) are
// both here, and this module's default isolation is nonisolated, which is what the pipeline
// is: pure functions over bytes, run on and off the main actor alike.

/// A file the user picked to send with a turn. `data` is the raw bytes; the client
/// base64-encodes it for the wire. Held in the composer as a removable chip and cleared
/// after an accepted send.
public struct JesseAttachment: Identifiable, Equatable, Sendable {
    public let id: UUID
    public var filename: String
    public var mime: String
    public var data: Data

    public init(id: UUID = UUID(), filename: String, mime: String, data: Data) {
        self.id = id
        self.filename = filename
        self.mime = mime
        self.data = data
    }

    public var byteCount: Int { data.count }
    public var isImage: Bool { mime.hasPrefix("image/") }

    /// Detect a whitelisted MIME from the file's magic bytes — the same sniff the bridge
    /// runs — so the declared type always matches the actual bytes (a PhotosPicker item may
    /// be HEIC even when it looks like a JPEG). Returns nil for anything not on the whitelist.
    ///
    /// `nonisolated` stays pinned explicitly, as it was in the app target: it is called from
    /// the downscaler and from detached preview work, and must never be inferred onto an actor.
    public nonisolated static func sniffMime(_ data: Data) -> String? {
        let b = [UInt8](data.prefix(16))
        func match(_ ascii: String, at off: Int = 0) -> Bool {
            let sig = Array(ascii.utf8)
            guard b.count >= off + sig.count else { return false }
            return Array(b[off..<off + sig.count]) == sig
        }
        if b.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
        if b.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        if match("GIF87a") || match("GIF89a") { return "image/gif" }
        if match("%PDF-") { return "application/pdf" }
        if match("RIFF") && match("WEBP", at: 8) { return "image/webp" }
        if match("ftyp", at: 4) {
            let brand = b.count >= 12 ? Array(b[8..<12]) : []
            let brands = ["heic", "heix", "hevc", "hevx", "heim", "heis", "mif1", "msf1"]
            if brands.contains(where: { Array($0.utf8) == brand }) { return "image/heic" }
        }
        return nil
    }

    /// The on-disk extension matching a whitelisted MIME (for display names).
    public static func fileExtension(forMime mime: String) -> String {
        switch mime {
        case "image/png": return "png"
        case "image/jpeg": return "jpg"
        case "image/gif": return "gif"
        case "image/webp": return "webp"
        case "image/heic": return "heic"
        case "application/pdf": return "pdf"
        default: return "bin"
        }
    }

    /// This attachment as the bridge receives it: base64-in-JSON, re-validated by the bridge
    /// for type and size. The ONE mapping both apps send through, so the declared MIME is
    /// always the sniffed one `AttachmentStaging` recorded.
    public var wire: JesseRequest.Attachment {
        JesseRequest.Attachment(filename: filename, mime: mime,
                                dataBase64: data.base64EncodedString())
    }
}

/// Client-side attachment limits. Mirror the bridge's server-side caps
/// (`DEFAULT_MAX_ATTACHMENT*` in `bridge/src/config.rs`) so a file that would be rejected is
/// caught before it is uploaded; the server still enforces them as the authority.
public enum AttachmentLimits {
    public static let maxCount = 4
    public static let maxBytesPerFile = 10 * 1024 * 1024
    public static let maxBytesTotal = 20 * 1024 * 1024

    /// MIME types the bridge will accept (magic-byte-verified server-side).
    public static let allowedMimes: Set<String> = [
        "image/png", "image/jpeg", "image/gif", "image/webp", "image/heic",
        "application/pdf",
    ]

    /// Validate adding `candidate` to the `existing` set. Returns a user-facing error
    /// message if it should be rejected, else nil.
    public static func rejectionReason(adding candidate: JesseAttachment,
                                       to existing: [JesseAttachment]) -> String? {
        if existing.count >= maxCount {
            return "You can attach at most \(maxCount) files."
        }
        if !allowedMimes.contains(candidate.mime) {
            return "“\(candidate.filename)” isn’t a supported type (images or PDF only)."
        }
        if candidate.byteCount > maxBytesPerFile {
            return "“\(candidate.filename)” is too large (max \(maxBytesPerFile / 1_048_576) MB per file)."
        }
        let total = existing.reduce(0) { $0 + $1.byteCount } + candidate.byteCount
        if total > maxBytesTotal {
            return "Attachments exceed the \(maxBytesTotal / 1_048_576) MB total limit."
        }
        return nil
    }
}

/// The ONE staging step every attachment source on both platforms goes through: the photo
/// picker, the file importer, the camera, paste, drop and Continuity Camera.
///
/// It is one function because the alternative already failed once: PR #51 fixed paste and the
/// picker having drifted into two different byte paths, so a pasted photo was re-encoded where
/// a picked one was not. Each platform owns only how the bytes are READ; what happens to them
/// from here (downscale, sniff, name, cap) is this, and nothing else.
public enum AttachmentStaging {

    /// What staging decided.
    public enum Outcome: Equatable, Sendable {
        /// Ready to append to the composer's files.
        case staged(JesseAttachment)
        /// Refused, with the sentence the composer's error line shows.
        case rejected(String)
    }

    /// The message for bytes that are not a whitelisted type at all.
    public static let unsupportedMessage = "That file type isn’t supported (images or PDF only)."

    /// Downscale if over the cap, sniff, name, and run the caps.
    ///
    /// - Parameters:
    ///   - fallbackName: the stem of a generated name ("Photo", "Document", "Pasted"), used
    ///     only when there is no `suggestedName`; the name is "<stem> <n>.<ext>".
    ///   - suggestedName: the source's own name (a file's, a camera's, a paste's), if any.
    ///   - existing: what the composer already holds, for the count and total caps.
    ///   - frugal: the metered-link policy; `.off` on the Mac, which has no frugal mode.
    public static func stage(data: Data, fallbackName: String, suggestedName: String?,
                             existing: [JesseAttachment],
                             frugal: FrugalPolicy) -> Outcome {
        // Oversized IMAGE → a JPEG that fits the per-file cap, so a large photo attaches
        // instead of erroring. Under-cap images and every non-image fall through untouched
        // (`fitToCap` returns nil), preserving the byte-verbatim staging PR #51 restored. The
        // output is always JPEG, so the display name gets a `.jpg` extension.
        var data = data
        var suggestedName = suggestedName
        if let fitted = AttachmentDownscaler.fitToCap(data, cap: AttachmentLimits.maxBytesPerFile,
                                                      frugal: frugal) {
            data = fitted
            suggestedName = suggestedName.map(AttachmentDownscaler.jpegFilename(from:))
        }
        guard let mime = JesseAttachment.sniffMime(data) else {
            return .rejected(unsupportedMessage)
        }
        let ext = JesseAttachment.fileExtension(forMime: mime)
        let name = suggestedName ?? "\(fallbackName) \(existing.count + 1).\(ext)"
        let candidate = JesseAttachment(filename: name, mime: mime, data: data)
        if let reason = AttachmentLimits.rejectionReason(adding: candidate, to: existing) {
            return .rejected(reason)
        }
        return .staged(candidate)
    }

    /// `stage`, applied to a composer's own list: appends on success and returns nil, or
    /// returns the rejection and leaves `files` alone.
    @discardableResult
    public static func add(data: Data, fallbackName: String, suggestedName: String? = nil,
                           to files: inout [JesseAttachment],
                           frugal: FrugalPolicy) -> String? {
        switch stage(data: data, fallbackName: fallbackName, suggestedName: suggestedName,
                     existing: files, frugal: frugal) {
        case .staged(let attachment):
            files.append(attachment)
            return nil
        case .rejected(let reason):
            return reason
        }
    }
}
