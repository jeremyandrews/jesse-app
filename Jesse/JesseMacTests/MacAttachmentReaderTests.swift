import XCTest
import AppKit
import UniformTypeIdentifiers
@testable import Jesse_Mac
import JesseNetworking

/// Paste, drop and Continuity Camera on the Mac composer, driven with SYNTHETIC pasteboards and
/// item providers: never the general pasteboard (a test must not clobber the clipboard) and
/// never a real iPhone. Continuity Camera's live behaviour (that AppKit offers Take Photo and
/// Scan Documents for this text view) needs a device and is on the PR's checklist; what is
/// pinned here is that the text view asks for images, and that what comes back is staged.
@MainActor
final class MacAttachmentReaderTests: XCTestCase {

    private var pasteboards: [NSPasteboard] = []

    override func tearDown() {
        for pb in pasteboards { pb.releaseGlobally() }
        pasteboards = []
        super.tearDown()
    }

    private func pasteboard(_ fill: (NSPasteboard) -> Void) -> NSPasteboard {
        let pb = NSPasteboard.withUniqueName()
        pb.clearContents()
        fill(pb)
        pasteboards.append(pb)
        return pb
    }

    private func textView(onMedia: @escaping ([MacMediaItem]) -> Void) -> ComposerNSTextView {
        let view = ComposerNSTextView()
        view.isEditable = true
        view.onMedia = onMedia
        return view
    }

    // MARK: - Paste

    func testAPastedPNGScreenshotStaysByteIdentical() throws {
        let png = try MacMediaFixtures.png()
        let pb = pasteboard { $0.setData(png, forType: .png) }

        let items = try XCTUnwrap(MacPasteboardMedia.read(pb))
        XCTAssertEqual(items.count, 1)
        XCTAssertEqual(items.first?.data, png, "a screenshot is staged verbatim, not re-encoded")
        XCTAssertTrue(items.first?.suggestedName?.hasPrefix("pasted-") ?? false)
        XCTAssertTrue(items.first?.suggestedName?.hasSuffix(".png") ?? false)
    }

    func testAPastedJPEGPhotoStaysJPEG() throws {
        let jpeg = try MacMediaFixtures.jpeg()
        let pb = pasteboard { $0.setData(jpeg, forType: NSPasteboard.PasteboardType(UTType.jpeg.identifier)) }

        let items = try XCTUnwrap(MacPasteboardMedia.read(pb))
        XCTAssertEqual(items.first?.data, jpeg)
    }

    func testATIFFOnlyImageBecomesPNG() throws {
        let tiff = try MacMediaFixtures.tiff()
        let pb = pasteboard { $0.setData(tiff, forType: .tiff) }

        let items = try XCTUnwrap(MacPasteboardMedia.read(pb))
        let data = try XCTUnwrap(items.first?.data)
        XCTAssertEqual(JesseAttachment.sniffMime(data), "image/png")
    }

    func testPlainTextIsNotMediaAndStillPastesAsText() {
        let pb = pasteboard { $0.setString("hello", forType: .string) }
        XCTAssertNil(MacPasteboardMedia.read(pb), "a text clipboard is not routed to attachments")

        var routed: [[MacMediaItem]] = []
        let view = textView { routed.append($0) }
        XCTAssertFalse(view.routeMedia(from: pb), "so the text view's own paste runs")
        XCTAssertTrue(routed.isEmpty)
    }

    func testAPastedImageIsRoutedToTheComposerNotTheText() throws {
        let png = try MacMediaFixtures.png()
        let pb = pasteboard { $0.setData(png, forType: .png) }
        var routed: [[MacMediaItem]] = []
        let view = textView { routed.append($0) }

        XCTAssertTrue(view.routeMedia(from: pb))
        XCTAssertEqual(routed.first?.first?.data, png)
        XCTAssertEqual(view.string, "", "nothing lands in the text")
    }

    // MARK: - Continuity Camera

    func testTheTextViewAsksForImagesAndPDFsBack() {
        let view = textView { _ in }
        XCTAssertTrue(view.validRequestor(forSendType: nil, returnType: .png) as AnyObject === view)
        XCTAssertTrue(view.validRequestor(forSendType: nil, returnType: .tiff) as AnyObject === view)
        XCTAssertTrue(view.validRequestor(forSendType: nil, returnType: .pdf) as AnyObject === view)
    }

    func testAContinuityCameraPhotoIsStagedNotInserted() throws {
        let jpeg = try MacMediaFixtures.jpeg()
        let pb = pasteboard { $0.setData(jpeg, forType: NSPasteboard.PasteboardType(UTType.jpeg.identifier)) }
        var routed: [[MacMediaItem]] = []
        let view = textView { routed.append($0) }

        XCTAssertTrue(view.readSelection(from: pb))
        XCTAssertEqual(routed.first?.first?.data, jpeg)
        XCTAssertEqual(view.string, "")
    }

    func testAContinuityCameraScanIsStagedAsAPDF() throws {
        let pdf = try MacMediaFixtures.pdf()
        let pb = pasteboard { $0.setData(pdf, forType: .pdf) }
        var routed: [[MacMediaItem]] = []
        let view = textView { routed.append($0) }

        XCTAssertTrue(view.readSelection(from: pb))
        let data = try XCTUnwrap(routed.first?.first?.data)
        XCTAssertEqual(JesseAttachment.sniffMime(data), "application/pdf")
    }

    // MARK: - Drop

    func testADroppedPDFAndTwoImagesStageThreeChips() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacAttachmentReaderTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let pdfURL = dir.appendingPathComponent("report.pdf")
        try MacMediaFixtures.pdf().write(to: pdfURL)

        // A Finder file: a file URL. Two images dragged out of an app: their data.
        let file = NSItemProvider()
        file.registerDataRepresentation(forTypeIdentifier: UTType.fileURL.identifier,
                                        visibility: .all) { completion in
            completion(pdfURL.dataRepresentation, nil)
            return nil
        }
        let png = try MacMediaFixtures.png()
        let jpeg = try MacMediaFixtures.jpeg()
        let first = NSItemProvider(item: png as NSData, typeIdentifier: UTType.png.identifier)
        first.suggestedName = "screen"
        let second = NSItemProvider(item: jpeg as NSData, typeIdentifier: UTType.jpeg.identifier)

        let items = await MacItemProviderMedia.read([file, first, second])
        var chips: [JesseAttachment] = []
        for item in items {
            let data = try XCTUnwrap(item.data)
            XCTAssertNil(AttachmentStaging.add(data: data, fallbackName: "Pasted",
                                               suggestedName: item.suggestedName,
                                               to: &chips, frugal: .off))
        }

        XCTAssertEqual(chips.count, 3)
        XCTAssertEqual(chips.map(\.mime), ["application/pdf", "image/png", "image/jpeg"])
        XCTAssertEqual(chips.first?.filename, "report.pdf")
        XCTAssertEqual(chips[1].filename, "screen.png")
        XCTAssertEqual(chips[1].data, png, "a dropped image is staged verbatim")
    }

    func testAFinderCopiedFileIsReadAsTheFileNotItsIcon() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacAttachmentReaderTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("scan.pdf")
        let pdf = try MacMediaFixtures.pdf()
        try pdf.write(to: url)
        let tiff = try MacMediaFixtures.tiff()
        // What Finder's Copy puts on the pasteboard: the file URL and an icon bitmap.
        let pb = pasteboard {
            let item = NSPasteboardItem()
            item.setString(url.absoluteString, forType: .fileURL)
            item.setData(tiff, forType: .tiff)
            $0.writeObjects([item])
        }

        let items = try XCTUnwrap(MacPasteboardMedia.read(pb))
        XCTAssertEqual(items, [MacMediaItem(data: pdf, suggestedName: "scan.pdf")])
    }
}
