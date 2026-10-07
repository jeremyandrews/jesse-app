import AppKit
import JesseNetworking
import UniformTypeIdentifiers

// The Mac's half of attaching a file: READING bytes out of AppKit. Everything that happens to
// them afterwards (downscale, sniff, name, caps) is `AttachmentStaging` in JesseNetworking,
// the same function the phone's composer calls, and the type order and the verbatim rule are
// the shared `ComposerPaste` and `PasteAttachment`.
//
// Three AppKit sources arrive as an `NSPasteboard` (Paste, a drop onto the text view, and
// Continuity Camera's Take Photo / Scan Documents, which AppKit hands to
// `readSelection(from:)`), and one as `NSItemProvider`s (a SwiftUI drop on the rest of the
// composer). Both readers produce the same `MacMediaItem`s, one per pasteboard item or
// provider, so a drop of three files stages three chips.

/// One item read for staging: its bytes (already verbatim or PNG, per `PasteAttachment`) and
/// the name it should carry, or `nil` data when it could not be read as an image or a PDF.
struct MacMediaItem: Equatable {
    var data: Data?
    var suggestedName: String?
}

extension PasteAttachment {
    /// The Mac half of the paste fallback: an image the pasteboard offers only as an
    /// `NSImage` (no concrete data type) re-encoded to PNG, so its magic bytes sniff as a
    /// whitelisted `image/png`. The phone has the `UIImage` twin beside its own reader.
    static func pngData(from image: NSImage) -> Data? {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        return AttachmentImageEncoding.png(cg)
    }
}

enum MacPasteboardMedia {

    /// The concrete data types tried on each pasteboard item, in the shared order.
    static var pasteboardTypes: [NSPasteboard.PasteboardType] {
        ComposerPaste.mediaTypes.map { NSPasteboard.PasteboardType($0.identifier) }
    }

    /// Whether a pasteboard type identifier is something this pipeline can take: an image
    /// encoding or a PDF. What Continuity Camera's return types are checked against.
    static func isMediaType(_ identifier: String) -> Bool {
        guard let type = UTType(identifier) else { return false }
        return type.conforms(to: .image) || type.conforms(to: .pdf)
    }

    /// Whether `pasteboard` carries an image or a PDF, either as data or as a file, and so
    /// should be staged as attachments rather than pasted as text. The shared
    /// `ComposerPaste.isMediaPaste` makes the call; this only asks AppKit the two questions.
    static func hasMedia(_ pasteboard: NSPasteboard) -> Bool {
        let items = pasteboard.pasteboardItems ?? []
        var hasImages = false
        var hasPDF = false
        for item in items {
            if let url = fileURL(in: item), let type = UTType(filenameExtension: url.pathExtension) {
                if type.conforms(to: .pdf) { hasPDF = true }
                if type.conforms(to: .image) { hasImages = true }
                continue
            }
            for type in item.types {
                guard let uti = UTType(type.rawValue) else { continue }
                if uti.conforms(to: .pdf) { hasPDF = true }
                if uti.conforms(to: .image) { hasImages = true }
            }
        }
        return ComposerPaste.isMediaPaste(hasImages: hasImages, hasPDF: hasPDF)
    }

    /// Every item on `pasteboard` read for staging, or nil when it carries no media and the
    /// text view should paste (or accept the drop) as text.
    ///
    /// Per item: a FILE (a Finder copy or drag) is read from disk under its own name, never
    /// as the icon bitmap Finder also puts on the pasteboard; otherwise the concrete encodings
    /// are tried in `ComposerPaste.mediaTypes` order and kept VERBATIM, so a screenshot stays
    /// PNG and a photo stays JPEG or HEIC. A TIFF-only image becomes PNG (`stageableBytes`).
    /// Only when no item yielded anything does the `NSImage` representation get re-encoded,
    /// the Mac twin of the phone's `UIImage` fallback.
    static func read(_ pasteboard: NSPasteboard, now: Date = Date()) -> [MacMediaItem]? {
        guard hasMedia(pasteboard) else { return nil }
        let items = pasteboard.pasteboardItems ?? []
        var read: [MacMediaItem] = []
        for item in items {
            if let url = fileURL(in: item) {
                read.append(fileItem(at: url))
                continue
            }
            guard let type = item.availableType(from: pasteboardTypes),
                  let raw = item.data(forType: type) else { continue }
            if let named = PasteAttachment.named(raw, date: now) {
                read.append(MacMediaItem(data: named.data, suggestedName: named.filename))
            } else {
                read.append(MacMediaItem(data: nil, suggestedName: nil))
            }
        }
        if read.isEmpty {
            if let image = NSImage(pasteboard: pasteboard),
               let png = PasteAttachment.pngData(from: image) {
                read.append(MacMediaItem(data: png,
                                         suggestedName: PasteAttachment.filename(ext: "png", date: now)))
            } else {
                read.append(MacMediaItem(data: nil, suggestedName: nil))
            }
        }
        return read
    }

    /// The file URL an item carries, if it is a file at all.
    private static func fileURL(in item: NSPasteboardItem) -> URL? {
        guard let string = item.string(forType: .fileURL),
              let url = URL(string: string), url.isFileURL else { return nil }
        return url
    }

    /// A file's bytes under its own name, when it is an image or a PDF; unreadable otherwise.
    /// What a picked, pasted or dropped file all come down to.
    static func fileItem(at url: URL) -> MacMediaItem {
        guard let type = UTType(filenameExtension: url.pathExtension),
              type.conforms(to: .image) || type.conforms(to: .pdf) else {
            return MacMediaItem(data: nil, suggestedName: url.lastPathComponent)
        }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let data = try? Data(contentsOf: url) else {
            return MacMediaItem(data: nil, suggestedName: url.lastPathComponent)
        }
        // A file is staged as itself: its bytes go to `AttachmentStaging` untouched, and a
        // TIFF file (not on the bridge's whitelist) is refused there with the usual message
        // rather than quietly converted.
        return MacMediaItem(data: data, suggestedName: url.lastPathComponent)
    }
}

/// The `NSItemProvider` reader, for a SwiftUI drop on the composer outside the text view.
/// The same per-item rules as `MacPasteboardMedia.read`, over the async provider API.
enum MacItemProviderMedia {

    /// The types a composer drop accepts.
    static let dropTypes: [UTType] = [.fileURL, .image, .pdf]

    static func read(_ providers: [NSItemProvider], now: Date = Date()) async -> [MacMediaItem] {
        var read: [MacMediaItem] = []
        for provider in providers {
            read.append(await item(from: provider, now: now))
        }
        return read
    }

    static func item(from provider: NSItemProvider, now: Date = Date()) async -> MacMediaItem {
        if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
           let raw = await loadData(provider, type: .fileURL),
           let url = URL(dataRepresentation: raw, relativeTo: nil), url.isFileURL {
            return MacPasteboardMedia.fileItem(at: url)
        }
        for type in ComposerPaste.mediaTypes {
            guard provider.hasItemConformingToTypeIdentifier(type.identifier) else { continue }
            if let raw = await loadData(provider, type: type),
               let staged = PasteAttachment.stageableBytes(from: raw) {
                return MacMediaItem(data: staged, suggestedName: name(for: staged, provider: provider, now: now))
            }
        }
        if provider.canLoadObject(ofClass: NSImage.self),
           let image = await loadImage(provider),
           let png = PasteAttachment.pngData(from: image) {
            return MacMediaItem(data: png, suggestedName: name(for: png, provider: provider, now: now))
        }
        return MacMediaItem(data: nil, suggestedName: provider.suggestedName)
    }

    /// The provider's own name with the extension the bytes actually are, else a generated
    /// `pasted-<timestamp>` one.
    private static func name(for data: Data, provider: NSItemProvider, now: Date) -> String {
        let ext = JesseAttachment.sniffMime(data).map(JesseAttachment.fileExtension(forMime:)) ?? "png"
        if let base = provider.suggestedName, !base.isEmpty {
            let stem = (base as NSString).deletingPathExtension
            return "\(stem.isEmpty ? base : stem).\(ext)"
        }
        return PasteAttachment.filename(ext: ext, date: now)
    }

    private static func loadData(_ provider: NSItemProvider, type: UTType) async -> Data? {
        await withCheckedContinuation { continuation in
            _ = provider.loadDataRepresentation(forTypeIdentifier: type.identifier) { data, _ in
                continuation.resume(returning: data)
            }
        }
    }

    private static func loadImage(_ provider: NSItemProvider) async -> NSImage? {
        await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: NSImage.self) { object, _ in
                continuation.resume(returning: object as? NSImage)
            }
        }
    }
}
