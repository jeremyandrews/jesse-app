import Foundation
import UniformTypeIdentifiers

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
    /// Word, RTF, PowerPoint, Excel or the text family: sent as bytes like any file, but the
    /// bridge inlines its TEXT into the prompt, so it has no thumbnail.
    public var isDocument: Bool { AttachmentDocumentTypes.mimes.contains(mime) }

    /// Detect a whitelisted MIME from the file's own bytes — the same rules the bridge
    /// runs — so the declared type always matches the actual bytes (a PhotosPicker item may
    /// be HEIC even when it looks like a JPEG). Returns nil for anything not on the whitelist.
    ///
    /// Images, PDF, RTF and the Office Open XML formats (by the ZIP central directory) are
    /// recognised from their bytes alone. Two kinds cannot be:
    ///  * the text family (plain text, Markdown, CSV, TSV, JSON) has no magic bytes, so
    ///    `contentType` — the picked file's type, or the type its name implies — chooses
    ///    WHICH one, and the bytes must then really be text: UTF-8 (or UTF-16 with a BOM),
    ///    no NUL, and for JSON, parseable;
    ///  * a legacy Word `.doc` is an OLE compound file, as are `.xls` and `.ppt`, so it is
    ///    accepted only when `contentType` says Word.
    ///
    /// `nonisolated` stays pinned explicitly, as it was in the app target: it is called from
    /// the downscaler and from detached preview work, and must never be inferred onto an actor.
    public nonisolated static func sniffMime(_ data: Data, contentType: UTType? = nil) -> String? {
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
        if b.starts(with: [0x50, 0x4B, 0x03, 0x04]) {
            return AttachmentDocumentTypes.officeOpenXMLMime(data)
        }
        if b.starts(with: [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
            let word = contentType.map { $0.conforms(to: AttachmentDocumentTypes.doc) } ?? false
            return word ? AttachmentDocumentTypes.docMime : nil
        }
        if match("{\\rtf") { return AttachmentDocumentTypes.rtfMime }
        guard let contentType, let mime = AttachmentDocumentTypes.textMime(for: contentType),
              AttachmentDocumentTypes.isText(data, mime: mime)
        else { return nil }
        return mime
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
        default: return AttachmentDocumentTypes.extensions[mime] ?? "bin"
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

    /// MIME types the bridge will accept (verified from the bytes server-side). Held equal
    /// to the bridge's whitelist by `bridge/tests/fixtures/attachment-mimes.json`, which both
    /// test suites read.
    public static let allowedMimes: Set<String> = Set([
        "image/png", "image/jpeg", "image/gif", "image/webp", "image/heic",
        "application/pdf",
    ]).union(AttachmentDocumentTypes.mimes)

    /// What the composer can take, in the words of every rejection.
    public static let supportedKinds = "images, PDF, Word, PowerPoint, text, CSV or Excel"

    /// Validate adding `candidate` to the `existing` set. Returns a user-facing error
    /// message if it should be rejected, else nil.
    public static func rejectionReason(adding candidate: JesseAttachment,
                                       to existing: [JesseAttachment]) -> String? {
        if existing.count >= maxCount {
            return "You can attach at most \(maxCount) files."
        }
        if !allowedMimes.contains(candidate.mime) {
            return "“\(candidate.filename)” isn’t a supported type (\(supportedKinds) only)."
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
    public static let unsupportedMessage =
        "That file type isn’t supported (\(AttachmentLimits.supportedKinds) only)."

    /// Downscale if over the cap, sniff, name, and run the caps.
    ///
    /// - Parameters:
    ///   - fallbackName: the stem of a generated name ("Photo", "Document", "Pasted"), used
    ///     only when there is no `suggestedName`; the name is "<stem> <n>.<ext>".
    ///   - suggestedName: the source's own name (a file's, a camera's, a paste's), if any.
    ///   - existing: what the composer already holds, for the count and total caps.
    ///   - frugal: the metered-link policy; `.off` on the Mac, which has no frugal mode.
    ///   - contentType: the source's declared type, if it has one (a picked or dropped file).
    ///     It only ever narrows: it chooses between the text formats, which have no magic
    ///     bytes, and lets an OLE file through as Word. When nil, the type `suggestedName`'s
    ///     extension implies is used. The bytes still decide everything else.
    public static func stage(data: Data, fallbackName: String, suggestedName: String?,
                             contentType: UTType? = nil,
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
        let declared = contentType ?? suggestedName.flatMap {
            let ext = ($0 as NSString).pathExtension
            return ext.isEmpty ? nil : UTType(filenameExtension: ext)
        }
        guard let mime = JesseAttachment.sniffMime(data, contentType: declared) else {
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
                           contentType: UTType? = nil,
                           to files: inout [JesseAttachment],
                           frugal: FrugalPolicy) -> String? {
        switch stage(data: data, fallbackName: fallbackName, suggestedName: suggestedName,
                     contentType: contentType, existing: files, frugal: frugal) {
        case .staged(let attachment):
            files.append(attachment)
            return nil
        case .rejected(let reason):
            return reason
        }
    }
}

/// THE DOCUMENT TYPES, ONCE, FOR BOTH APPS: Word, RTF, PowerPoint, Excel and the text family.
///
/// The bridge turns each of these into text and inlines it in the prompt, so every model on
/// every harness reads the same thing. The client's part is to send only bytes the bridge will
/// accept, under the MIME the bridge will detect, which is why the recognition here follows
/// the bridge's `sniff_document` rule for rule. Legacy `.xls` and `.ppt` are not here: the
/// bridge refuses them, and so do the pickers.
public enum AttachmentDocumentTypes {
    public static let docxMime = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    public static let xlsxMime = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    public static let pptxMime = "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    public static let docMime = "application/msword"
    public static let rtfMime = "application/rtf"

    /// MIME → the extension the bridge writes and a display name uses.
    public static let extensions: [String: String] = [
        docxMime: "docx", docMime: "doc", rtfMime: "rtf", xlsxMime: "xlsx", pptxMime: "pptx",
        "text/plain": "txt", "text/markdown": "md", "text/csv": "csv",
        "text/tab-separated-values": "tsv", "application/json": "json",
    ]

    public static let mimes: Set<String> = Set(extensions.keys)

    // Declared by Apple only where the system ships an importer, so constructed by identifier
    // with a fallback that conforms to nothing a picker would show by mistake.
    static let doc = UTType("com.microsoft.word.doc") ?? .data
    static let docx = UTType("org.openxmlformats.wordprocessingml.document") ?? .data
    static let xlsx = UTType("org.openxmlformats.spreadsheetml.sheet") ?? .data
    static let pptx = UTType("org.openxmlformats.presentationml.presentation") ?? .data
    static let markdown = UTType("net.daringfireball.markdown") ?? .plainText

    /// What the "Document…" pickers on both platforms accept, and what a dropped or pasted
    /// file URL must conform to: PDF plus every document type above.
    public static let pickerContentTypes: [UTType] = [
        .pdf, docx, doc, .rtf, pptx, .plainText, markdown, .commaSeparatedText,
        .tabSeparatedText, .json, xlsx,
    ]

    /// Whether a FILE of this type is one the composer can attach as a document (a pasted or
    /// dropped file URL). Images and PDF have their own checks at each call site.
    public static func isDocumentFile(_ type: UTType) -> Bool {
        pickerContentTypes.contains { $0 != .pdf && type.conforms(to: $0) }
    }

    /// Which text-family MIME a declared type names, most specific first: a CSV is also text,
    /// and must be sent as CSV.
    static func textMime(for type: UTType) -> String? {
        if type.conforms(to: .json) { return "application/json" }
        if type.conforms(to: .commaSeparatedText) { return "text/csv" }
        if type.conforms(to: .tabSeparatedText) { return "text/tab-separated-values" }
        if type.conforms(to: markdown) { return "text/markdown" }
        if type.conforms(to: .plainText) { return "text/plain" }
        return nil
    }

    /// Whether `data` is text the bridge will accept as `mime`: UTF-8 (a BOM allowed) or
    /// UTF-16 with a BOM, no NUL, and for JSON, parseable. The same test the bridge runs, so a
    /// binary renamed `.txt` is refused here rather than uploaded to be refused there.
    static func isText(_ data: Data, mime: String) -> Bool {
        let bytes = [UInt8](data)
        let text: String?
        if bytes.starts(with: [0xFF, 0xFE]) || bytes.starts(with: [0xFE, 0xFF]) {
            text = bytes.count % 2 == 0 ? String(data: data, encoding: .utf16) : nil
        } else {
            let body = bytes.starts(with: [0xEF, 0xBB, 0xBF]) ? Array(bytes.dropFirst(3)) : bytes
            text = String(data: Data(body), encoding: .utf8)
        }
        guard let text, !text.contains("\u{0}") else { return false }
        if mime == "application/json" {
            // Foundation's parsers accept a trailing comma (`{"a": 1,}`) where the bridge's
            // serde_json does not, so such a file passes here and is refused there with a 400
            // that says it does not parse. Everything else invalid is caught here.
            return (try? JSONSerialization.jsonObject(with: Data(text.utf8),
                                                      options: .fragmentsAllowed)) != nil
        }
        return true
    }

    /// The Office Open XML MIME a ZIP's CENTRAL DIRECTORY names, or nil. Read from the
    /// directory the way the bridge reads it, not by searching the bytes, so a ZIP that merely
    /// contains `word/document.xml` somewhere in a file is not mistaken for a Word document.
    static func officeOpenXMLMime(_ data: Data) -> String? {
        let names = zipEntryNames(data)
        if names.contains("word/document.xml") { return docxMime }
        if names.contains("xl/workbook.xml") { return xlsxMime }
        if names.contains("ppt/presentation.xml") { return pptxMime }
        return nil
    }

    /// Every entry name in a ZIP's central directory, or empty if there is none to read.
    static func zipEntryNames(_ data: Data) -> Set<String> {
        let b = [UInt8](data)
        func u16(_ i: Int) -> Int { i + 2 <= b.count ? Int(b[i]) | Int(b[i + 1]) << 8 : -1 }
        func u32(_ i: Int) -> Int {
            i + 4 <= b.count ? Int(b[i]) | Int(b[i + 1]) << 8 | Int(b[i + 2]) << 16 | Int(b[i + 3]) << 24 : -1
        }
        // The end-of-central-directory record is in the last 22 bytes plus at most a 64 KiB
        // comment; search backwards for its signature.
        let eocdSig = 0x0605_4B50, entrySig = 0x0201_4B50
        guard b.count >= 22 else { return [] }
        var eocd = -1
        for i in stride(from: b.count - 22, through: max(0, b.count - 22 - 65_535), by: -1)
        where u32(i) == eocdSig {
            eocd = i
            break
        }
        guard eocd >= 0 else { return [] }
        let count = u16(eocd + 10)
        var at = u32(eocd + 16)
        guard count > 0, at >= 0, at < b.count else { return [] }
        var names = Set<String>()
        for _ in 0..<count {
            guard u32(at) == entrySig else { break }
            let nameLen = u16(at + 28), extraLen = u16(at + 30), commentLen = u16(at + 32)
            guard nameLen >= 0, extraLen >= 0, commentLen >= 0, at + 46 + nameLen <= b.count else { break }
            if let name = String(bytes: b[(at + 46)..<(at + 46 + nameLen)], encoding: .utf8) {
                names.insert(name)
            }
            at += 46 + nameLen + extraLen + commentLen
        }
        return names
    }
}
