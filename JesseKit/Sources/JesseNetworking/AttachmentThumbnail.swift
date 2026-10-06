import Foundation
import CoreGraphics
import ImageIO
import PDFKit
import UniformTypeIdentifiers

// Storage-optimized preview generation for attachments. Turns a picked image or
// PDF (potentially many MB) into a small downscaled JPEG (a few KB) that history
// can persist per turn without unbounded growth. The full-resolution bytes are
// used only to produce the thumbnail and are never returned or retained here.
//
// Pure and stateless so it's unit-testable (assert output format/size and that
// the original bytes aren't echoed back) and safe to run off the main actor.
//
// Platform-neutral: the image path is ImageIO end to end, and the PDF path renders the
// first page with PDFKit into a CoreGraphics bitmap rather than through
// `PDFPage.thumbnail(of:for:)`, whose return type is `UIImage` on iOS and `NSImage` on the
// Mac. Both encode through `AttachmentImageEncoding`.
public nonisolated enum AttachmentThumbnail {
    /// Longest-side pixel cap for a stored preview. Small on purpose — the preview
    /// only needs to be recognizable in a history row, not sharp.
    public static let maxDimension: CGFloat = 320
    /// JPEG quality for the re-encoded preview. Modest, since it's a thumbnail —
    /// keeps a typical preview in the low-KB range.
    public static let jpegQuality: CGFloat = 0.6

    /// A downscaled JPEG preview of `data` (an image or a PDF), or nil if the bytes
    /// can't be rendered. The result is a freshly re-encoded JPEG; the original bytes
    /// are never returned or held beyond this call.
    public static func make(data: Data, mime: String) -> Data? {
        if mime == "application/pdf" {
            return pdfThumbnail(data)
        }
        return imageThumbnail(data)
    }

    /// One preview per attachment that renders, in order, as `(filename, mime, thumbnail)`.
    ///
    /// Runs on a detached utility task, so the decode and encode never touch the caller's
    /// actor: both coordinators call this right after the user turn is saved and attach the
    /// results as `TurnAttachment`s when it returns. An attachment that will not render is
    /// simply absent, since a preview is never critical to the turn.
    public static func previews(for attachments: [JesseAttachment]) async
        -> [(filename: String, mime: String, thumbnail: Data)] {
        await Task.detached(priority: .utility) {
            attachments.compactMap { att in
                make(data: att.data, mime: att.mime).map { (att.filename, att.mime, $0) }
            }
        }.value
    }

    /// Downsample an image to `maxDimension` on its longest side using ImageIO —
    /// which decodes only the reduced thumbnail, never the full-resolution image —
    /// then JPEG-encode. Handles PNG/JPEG/GIF/WebP/HEIC (the sniffed whitelist) and
    /// respects EXIF orientation.
    private static func imageThumbnail(_ data: Data) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: Int(maxDimension),
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            return nil
        }
        return AttachmentImageEncoding.jpeg(cg, quality: jpegQuality)
    }

    /// Render the first page of a PDF into an image no larger than `maxDimension`
    /// on its longest side, on white, then JPEG-encode. The first page is enough to
    /// recognize the document in history.
    private static func pdfThumbnail(_ data: Data) -> Data? {
        guard let doc = PDFDocument(data: data), let page = doc.page(at: 0) else { return nil }
        let bounds = page.bounds(for: .mediaBox)
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        let scale = maxDimension / max(bounds.width, bounds.height)
        let width = max(1, Int((bounds.width * scale).rounded()))
        let height = max(1, Int((bounds.height * scale).rounded()))
        guard let ctx = CGContext(data: nil, width: width, height: height,
                                  bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        // A PDF page is usually transparent where nothing is drawn; a JPEG has no alpha, and
        // a black page would be unrecognisable in a history row.
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        ctx.scaleBy(x: CGFloat(width) / bounds.width, y: CGFloat(height) / bounds.height)
        ctx.translateBy(x: -bounds.minX, y: -bounds.minY)
        page.draw(with: .mediaBox, to: ctx)
        guard let cg = ctx.makeImage() else { return nil }
        return AttachmentImageEncoding.jpeg(cg, quality: jpegQuality)
    }
}

/// The two final encodes the pipeline needs, through ImageIO's `CGImageDestination` so they
/// run on both platforms. They replace `UIImage.jpegData` and `UIImage.pngData`, which were
/// the only reason the downscaler, the thumbnail and the paste fallback were iOS-only.
public nonisolated enum AttachmentImageEncoding {
    /// `image` as JPEG bytes at `quality` (0...1), or nil if ImageIO refuses it.
    public static func jpeg(_ image: CGImage, quality: CGFloat) -> Data? {
        encode(image, type: .jpeg,
               properties: [kCGImageDestinationLossyCompressionQuality: quality])
    }

    /// `image` as PNG bytes, or nil if ImageIO refuses it.
    public static func png(_ image: CGImage) -> Data? {
        encode(image, type: .png, properties: [:])
    }

    private static func encode(_ image: CGImage, type: UTType,
                               properties: [CFString: Any]) -> Data? {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil)
        else { return nil }
        CGImageDestinationAddImage(dest, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }
}
