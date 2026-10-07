import XCTest
import CoreGraphics
import ImageIO
@testable import JesseNetworking

/// The shared attachment pipeline, run on macOS (and on the iOS simulator by the local CI on
/// an older host): the ONE staging function both composers call, the sniff, the caps, the
/// downscale and the preview. Everything here is CoreGraphics and ImageIO, so a UIKit
/// reference creeping back in would fail to compile on the Mac before it failed a test.
final class AttachmentPipelineTests: XCTestCase {

    // MARK: - Sniffing

    func testWhitelistedTypesSniffFromTheirMagicBytes() throws {
        XCTAssertEqual(JesseAttachment.sniffMime(try Fixtures.png()), "image/png")
        XCTAssertEqual(JesseAttachment.sniffMime(Data([0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10])), "image/jpeg")
        XCTAssertEqual(JesseAttachment.sniffMime(Data("GIF89a\u{01}\u{00}".utf8)), "image/gif")
        XCTAssertEqual(JesseAttachment.sniffMime(Data("RIFF\u{00}\u{00}\u{00}\u{00}WEBPVP8 ".utf8)), "image/webp")
        XCTAssertEqual(JesseAttachment.sniffMime(Fixtures.heicHeader), "image/heic")
        XCTAssertEqual(JesseAttachment.sniffMime(try Fixtures.pdf()), "application/pdf")
        XCTAssertNil(JesseAttachment.sniffMime(Data("plain text".utf8)))
    }

    // MARK: - Staging

    func testAnUnderCapFileIsStagedByteForByteUnderAGeneratedName() throws {
        let png = try Fixtures.png()
        let outcome = AttachmentStaging.stage(data: png, fallbackName: "Photo", suggestedName: nil,
                                              existing: [], frugal: .off)
        guard case .staged(let att) = outcome else { return XCTFail("staged: \(outcome)") }
        XCTAssertEqual(att.data, png, "under the cap nothing is decoded or re-encoded")
        XCTAssertEqual(att.mime, "image/png")
        XCTAssertEqual(att.filename, "Photo 1.png")
    }

    func testAnOversizedImageIsDownscaledUnderTheCapAndRenamedJpg() throws {
        let big = try Fixtures.noisePNG(side: 2200)
        XCTAssertGreaterThan(big.count, AttachmentLimits.maxBytesPerFile,
                             "precondition: the fixture is over the per-file cap")
        let outcome = AttachmentStaging.stage(data: big, fallbackName: "Photo",
                                              suggestedName: "noise.png", existing: [], frugal: .off)
        guard case .staged(let att) = outcome else { return XCTFail("staged: \(outcome)") }
        XCTAssertLessThanOrEqual(att.byteCount, AttachmentLimits.maxBytesPerFile)
        XCTAssertEqual(att.mime, "image/jpeg")
        XCTAssertEqual(att.filename, "noise.jpg")
    }

    func testAFifthFileIsRefused() throws {
        let pdf = try Fixtures.pdf()
        var files: [JesseAttachment] = []
        for _ in 0..<AttachmentLimits.maxCount {
            XCTAssertNil(AttachmentStaging.add(data: pdf, fallbackName: "Document", to: &files, frugal: .off))
        }
        XCTAssertEqual(AttachmentStaging.add(data: pdf, fallbackName: "Document", to: &files, frugal: .off),
                       "You can attach at most 4 files.")
        XCTAssertEqual(files.count, AttachmentLimits.maxCount, "the refused one is not appended")
    }

    func testAnElevenMegabytePDFIsRefused() throws {
        var pdf = try Fixtures.pdf()
        pdf.append(Data(count: 11 * 1_048_576 - pdf.count))
        let outcome = AttachmentStaging.stage(data: pdf, fallbackName: "Document",
                                              suggestedName: "big.pdf", existing: [], frugal: .off)
        XCTAssertEqual(outcome, .rejected("“big.pdf” is too large (max 10 MB per file)."),
                       "a PDF is never downscaled; the cap refuses it")
    }

    func testTwentyOneMegabytesInTotalIsRefused() throws {
        func pdf(_ megabytes: Double) throws -> Data {
            var data = try Fixtures.pdf()
            data.append(Data(count: Int(megabytes * 1_048_576) - data.count))
            return data
        }
        var files: [JesseAttachment] = []
        XCTAssertNil(AttachmentStaging.add(data: try pdf(9), fallbackName: "Document", to: &files, frugal: .off))
        XCTAssertNil(AttachmentStaging.add(data: try pdf(9), fallbackName: "Document", to: &files, frugal: .off))
        XCTAssertEqual(AttachmentStaging.add(data: try pdf(3), fallbackName: "Document", to: &files, frugal: .off),
                       "Attachments exceed the 20 MB total limit.")
        XCTAssertEqual(files.count, 2)
    }

    func testAnUnsupportedTypeIsRefusedWithTheExistingMessage() {
        let outcome = AttachmentStaging.stage(data: Data("PK\u{03}\u{04}zip".utf8), fallbackName: "Document",
                                              suggestedName: "a.zip", existing: [], frugal: .off)
        XCTAssertEqual(outcome, .rejected(AttachmentStaging.unsupportedMessage))
    }

    func testTheWireMappingIsBase64WithTheSniffedMime() throws {
        let png = try Fixtures.png()
        let att = JesseAttachment(filename: "a.png", mime: "image/png", data: png)
        XCTAssertEqual(att.wire, JesseRequest.Attachment(filename: "a.png", mime: "image/png",
                                                         dataBase64: png.base64EncodedString()))
    }

    // MARK: - Thumbnails

    func testAnImageThumbnailIsASmallFreshJPEG() throws {
        let input = try Fixtures.png(width: 1200, height: 800)
        let out = try XCTUnwrap(AttachmentThumbnail.make(data: input, mime: "image/png"))
        XCTAssertEqual(JesseAttachment.sniffMime(out), "image/jpeg")
        XCTAssertNotEqual(out, input, "the thumbnail never echoes the input bytes")
        let (w, h) = try Fixtures.pixelSize(out)
        XCTAssertLessThanOrEqual(CGFloat(max(w, h)), AttachmentThumbnail.maxDimension)
    }

    func testAPDFThumbnailIsTheFirstPageAsASmallJPEG() throws {
        let input = try Fixtures.pdf()
        let out = try XCTUnwrap(AttachmentThumbnail.make(data: input, mime: "application/pdf"))
        XCTAssertEqual(JesseAttachment.sniffMime(out), "image/jpeg")
        let (w, h) = try Fixtures.pixelSize(out)
        XCTAssertEqual(CGFloat(max(w, h)), AttachmentThumbnail.maxDimension, accuracy: 1)
    }

    func testPreviewsSkipWhatWillNotRenderAndKeepOrder() async throws {
        let files = [
            JesseAttachment(filename: "a.png", mime: "image/png", data: try Fixtures.png()),
            JesseAttachment(filename: "junk.png", mime: "image/png", data: Data("junk".utf8)),
            JesseAttachment(filename: "b.pdf", mime: "application/pdf", data: try Fixtures.pdf()),
        ]
        let previews = await AttachmentThumbnail.previews(for: files)
        XCTAssertEqual(previews.map(\.filename), ["a.png", "b.pdf"])
    }

    // MARK: - Paste rules

    func testAPastedTIFFBecomesAPNGAndAWhitelistedPasteIsVerbatim() throws {
        let tiff = try Fixtures.tiff()
        XCTAssertNil(JesseAttachment.sniffMime(tiff))
        let staged = try XCTUnwrap(PasteAttachment.stageableBytes(from: tiff))
        XCTAssertEqual(JesseAttachment.sniffMime(staged), "image/png")
        let png = try Fixtures.png()
        XCTAssertEqual(PasteAttachment.stageableBytes(from: png), png)
        XCTAssertNil(PasteAttachment.stageableBytes(from: Data("plain text".utf8)))
    }
}

/// Synthetic images and documents, made in memory with CoreGraphics so the tests carry no
/// binary fixtures and run on either platform.
enum Fixtures {
    static let heicHeader = Data([0, 0, 0, 0x18] + Array("ftypheic".utf8) + [0, 0, 0, 0])

    static func image(width: Int, height: Int, noise: Bool = false) throws -> CGImage {
        let ctx = try XCTUnwrap(CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        if noise, let buffer = ctx.data {
            // Incompressible content, so a PNG of it is genuinely large.
            var rng = SystemRandomNumberGenerator()
            let bytes = buffer.bindMemory(to: UInt64.self, capacity: ctx.bytesPerRow * height / 8)
            for i in 0..<(ctx.bytesPerRow * height / 8) { bytes[i] = rng.next() }
        } else {
            ctx.setFillColor(CGColor(red: 0.9, green: 0.2, blue: 0.1, alpha: 1))
            ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        }
        return try XCTUnwrap(ctx.makeImage())
    }

    static func png(width: Int = 8, height: Int = 8) throws -> Data {
        try XCTUnwrap(AttachmentImageEncoding.png(try image(width: width, height: height)))
    }

    static func noisePNG(side: Int) throws -> Data {
        try XCTUnwrap(AttachmentImageEncoding.png(try image(width: side, height: side, noise: true)))
    }

    static func tiff() throws -> Data {
        let out = NSMutableData()
        let dest = try XCTUnwrap(CGImageDestinationCreateWithData(out, "public.tiff" as CFString, 1, nil))
        CGImageDestinationAddImage(dest, try image(width: 4, height: 4), nil)
        XCTAssertTrue(CGImageDestinationFinalize(dest))
        return out as Data
    }

    /// A real one-page PDF (US Letter), so PDFKit can render it.
    static func pdf() throws -> Data {
        let out = NSMutableData()
        var box = CGRect(x: 0, y: 0, width: 612, height: 792)
        let consumer = try XCTUnwrap(CGDataConsumer(data: out))
        let ctx = try XCTUnwrap(CGContext(consumer: consumer, mediaBox: &box, nil))
        ctx.beginPDFPage(nil)
        ctx.setFillColor(CGColor(red: 0, green: 0, blue: 0.8, alpha: 1))
        ctx.fill(CGRect(x: 72, y: 72, width: 200, height: 200))
        ctx.endPDFPage()
        ctx.closePDF()
        return out as Data
    }

    static func pixelSize(_ data: Data) throws -> (Int, Int) {
        let source = try XCTUnwrap(CGImageSourceCreateWithData(data as CFData, nil))
        let props = try XCTUnwrap(CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any])
        return (try XCTUnwrap(props[kCGImagePropertyPixelWidth] as? Int),
                try XCTUnwrap(props[kCGImagePropertyPixelHeight] as? Int))
    }
}
