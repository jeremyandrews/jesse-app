import XCTest
import AppKit
import SwiftData
import ImageIO
@testable import Jesse_Mac
import JesseCore
import JesseNetworking

/// The Mac send carries the composer's staged files. Until this existed, `MacCoordinator.post`
/// passed `attachments: []` to every turn, so nothing the Mac could stage would ever have
/// reached the bridge; these pin the request the fake client receives and the previews the
/// user turn keeps.
@MainActor
final class MacAttachmentSendTests: XCTestCase {

    private func coordinator(_ fake: MacFakeBridgeClient,
                             config: MacConfigStore = MacTestFixtures.configured()) -> MacCoordinator {
        MacCoordinator(configStore: config, makeClient: { _ in fake },
                       sessionDeletionStore: MacTestFixtures.deletionStore())
    }

    /// Stage through the SAME shared function the composer calls.
    private func staged(_ sources: [(Data, String)]) -> [JesseAttachment] {
        var files: [JesseAttachment] = []
        for (data, name) in sources {
            XCTAssertNil(AttachmentStaging.add(data: data, fallbackName: "Document",
                                               suggestedName: name, to: &files, frugal: .off))
        }
        return files
    }

    private func waitForSettle(_ coord: MacCoordinator, _ thread: JesseThread) async {
        var spins = 0
        while coord.isRunning(thread.id) && spins < 100_000 { await Task.yield(); spins += 1 }
    }

    func testAPNGAndAPDFStagedOnTheMacReachTheBridgeWithSniffedMimesAndBase64() async throws {
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask); context.insert(thread); try context.save()
        let sid = "sess-\(UUID().uuidString)"; defer { MacCursorStore.clear(sid) }
        let fake = MacFakeBridgeClient(
            sendResult: .reply(JesseReply(text: "ok", sessionId: sid), jobId: nil, conversationId: nil))
        let coord = coordinator(fake)
        let png = try MacMediaFixtures.png()
        let pdf = try MacMediaFixtures.pdf()
        let files = staged([(png, "screen.png"), (pdf, "report.pdf")])

        XCTAssertTrue(coord.stageAndSend(text: "what are these", mode: .ask, thread: thread,
                                         context: context, files: files))
        await waitForSettle(coord, thread)

        let sent = try XCTUnwrap(fake.sentAttachments.last)
        XCTAssertEqual(sent.map(\.filename), ["screen.png", "report.pdf"])
        XCTAssertEqual(sent.map(\.mime), ["image/png", "application/pdf"],
                       "the declared MIME is the sniffed one")
        XCTAssertEqual(sent.map(\.dataBase64), [png.base64EncodedString(), pdf.base64EncodedString()],
                       "the bytes are the staged bytes, base64-encoded")
    }

    func testTheUserTurnKeepsPreviewsAndNeverTheFullBytes() async throws {
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask); context.insert(thread); try context.save()
        let sid = "sess-\(UUID().uuidString)"; defer { MacCursorStore.clear(sid) }
        let fake = MacFakeBridgeClient(
            sendResult: .reply(JesseReply(text: "ok", sessionId: sid), jobId: nil, conversationId: nil))
        let coord = coordinator(fake)
        let png = try MacMediaFixtures.png(width: 900, height: 600)
        let pdf = try MacMediaFixtures.pdf()

        XCTAssertTrue(coord.stageAndSend(text: "look", mode: .ask, thread: thread, context: context,
                                         files: staged([(png, "a.png"), (pdf, "b.pdf")])))
        await coord.lastPreviewTask?.value
        await waitForSettle(coord, thread)

        let user = try XCTUnwrap(thread.orderedTurns.first { $0.isUser })
        let previews = user.orderedAttachments
        XCTAssertEqual(previews.map(\.filename), ["a.png", "b.pdf"])
        XCTAssertEqual(previews.map(\.mime), ["image/png", "application/pdf"])
        for preview in previews {
            XCTAssertEqual(JesseAttachment.sniffMime(preview.thumbnail), "image/jpeg")
            XCTAssertNotEqual(preview.thumbnail, png)
            XCTAssertNotEqual(preview.thumbnail, pdf)
        }
    }

    /// Empty text with a staged file is a real turn, as an attached context already was.
    func testEmptyTextWithAStagedFileIsSent() async throws {
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask); context.insert(thread); try context.save()
        let sid = "sess-\(UUID().uuidString)"; defer { MacCursorStore.clear(sid) }
        let fake = MacFakeBridgeClient(
            sendResult: .reply(JesseReply(text: "ok", sessionId: sid), jobId: nil, conversationId: nil))
        let coord = coordinator(fake)

        XCTAssertTrue(coord.stageAndSend(text: "", mode: .ask, thread: thread, context: context,
                                         files: staged([(try MacMediaFixtures.png(), "a.png")])))
        await waitForSettle(coord, thread)

        XCTAssertEqual(fake.sentTexts, [""])
        XCTAssertEqual(fake.sentAttachments.last?.count, 1)
    }

    func testEmptyTextWithNoFileIsStillNothingToSend() async throws {
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask); context.insert(thread); try context.save()
        let fake = MacFakeBridgeClient()
        let coord = coordinator(fake)

        XCTAssertFalse(coord.stageAndSend(text: "  ", mode: .ask, thread: thread, context: context))
        XCTAssertTrue(fake.sentTexts.isEmpty)
        XCTAssertTrue(thread.orderedTurns.isEmpty)
    }

    /// The composer's gate, asked the way `MacThreadDetailView.canSend` asks it: a staged file
    /// counts, so the button is live with nothing typed.
    func testTheSendGateCountsAStagedFile() {
        let files = staged([(try! MacMediaFixtures.png(), "a.png")])
        XCTAssertNil(MacSendGate.refusal(typed: "", hasAttachment: !files.isEmpty,
                                         isConfigured: true, isRunningInThisConversation: false))
        XCTAssertEqual(MacSendGate.refusal(typed: "", hasAttachment: false, isConfigured: true,
                                           isRunningInThisConversation: false), .nothingToSend)
    }

    /// A refused send stages nothing, so the composer keeps its files (it clears only on true).
    func testARefusedSendWithFilesSendsNothing() async throws {
        let context = try MacTestFixtures.context()
        let thread = JesseThread(mode: .ask); context.insert(thread); try context.save()
        let fake = MacFakeBridgeClient()
        let coord = coordinator(fake, config: MacTestFixtures.unconfigured())

        XCTAssertFalse(coord.stageAndSend(text: "", mode: .ask, thread: thread, context: context,
                                          files: staged([(try MacMediaFixtures.png(), "a.png")])))
        XCTAssertTrue(fake.sentTexts.isEmpty)
        XCTAssertTrue(thread.orderedTurns.isEmpty)
    }
}

/// Synthetic media made in memory, so the Mac tests carry no binary fixtures.
@MainActor
enum MacMediaFixtures {
    static func cgImage(width: Int, height: Int) throws -> CGImage {
        let ctx = try XCTUnwrap(CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        ctx.setFillColor(CGColor(red: 0.1, green: 0.6, blue: 0.3, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        return try XCTUnwrap(ctx.makeImage())
    }

    static func png(width: Int = 8, height: Int = 8) throws -> Data {
        try XCTUnwrap(AttachmentImageEncoding.png(try cgImage(width: width, height: height)))
    }

    static func jpeg() throws -> Data {
        try XCTUnwrap(AttachmentImageEncoding.jpeg(try cgImage(width: 8, height: 8), quality: 0.9))
    }

    static func tiff() throws -> Data {
        let out = NSMutableData()
        let dest = try XCTUnwrap(CGImageDestinationCreateWithData(out, "public.tiff" as CFString, 1, nil))
        CGImageDestinationAddImage(dest, try cgImage(width: 4, height: 4), nil)
        XCTAssertTrue(CGImageDestinationFinalize(dest))
        return out as Data
    }

    static func pdf() throws -> Data {
        let out = NSMutableData()
        var box = CGRect(x: 0, y: 0, width: 612, height: 792)
        let ctx = try XCTUnwrap(CGContext(consumer: try XCTUnwrap(CGDataConsumer(data: out)),
                                          mediaBox: &box, nil))
        ctx.beginPDFPage(nil)
        ctx.fill(CGRect(x: 72, y: 72, width: 100, height: 100))
        ctx.endPDFPage()
        ctx.closePDF()
        return out as Data
    }
}
