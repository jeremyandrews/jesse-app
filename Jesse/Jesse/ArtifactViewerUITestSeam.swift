import Foundation
import SwiftData
import UIKit
import JesseCore
import JesseNetworking

/// The UI-test seam for the returned-file viewers, and nothing else.
///
/// Whether an opened file can be CLOSED is a question only the running app answers: it is
/// about which chrome UIKit draws around a presented controller and which gesture wins a
/// drag, and no view host reproduces either. Driving it needs a conversation whose reply
/// carries files, and the files' bytes already on the device, because a UI test has no
/// bridge to download them from. The app supplies both itself, behind one variable.
///
/// `JESSE_UITEST_ARTIFACTS=1` arms it. It seeds one conversation whose reply returned a
/// PDF, an HTML page, a PNG and an SVG, writes each one's bytes into the real
/// `ArtifactCache` under the id the row names (so the ordinary resolver finds them as a
/// cache hit and never reaches for a client), and posts the same push tap
/// `ThreadLandingUITestSeam` does, so the app opens the conversation the way a
/// notification would. ABSENT, which is every ordinary launch, `isArmed` is false and
/// `arm(context:)` returns at once. Compiled out of Release, and a launch environment is set
/// only by a debugger, an XCTest runner or `simctl launch`: the same terms as
/// `ThreadLandingUITestSeam`.
@MainActor
enum ArtifactViewerUITestSeam {

    /// Whether a UI test armed the seam. Read ONCE.
    static let isArmed: Bool = {
        #if DEBUG
        return ProcessInfo.processInfo.environment["JESSE_UITEST_ARTIFACTS"] != nil
        #else
        return false
        #endif
    }()

    /// The seeded conversation's name, in its navigation bar.
    static let threadTitle = "Artifact Probe"

    /// The four files, by the display name each chip or picture is labelled with. The UI
    /// tests duplicate these, because a UI test runs in its own process and cannot import
    /// the app.
    static let pdfName = "probe-report.pdf"
    static let htmlName = "probe-page.html"
    static let pngName = "probe-chart.png"
    static let svgName = "probe-diagram.svg"

    private static var armed = false

    /// Seed the conversation, cache its files, and post the tap that opens it.
    static func arm(context: ModelContext) {
        guard isArmed, !armed else { return }
        armed = true
        guard let cache = ArtifactCache.standard() else { return }

        let thread = JesseThread(title: threadTitle, mode: .ask)
        let conversationId = JesseThread.mintConversationId()
        thread.conversationId = conversationId
        context.insert(thread)
        let ask = Turn(role: .user, text: "Send me the report, the page, the chart and the diagram.",
                       createdAt: Date().addingTimeInterval(-60))
        ask.thread = thread
        context.insert(ask)
        let reply = Turn(role: .jesse, text: "Here are the four files.")
        reply.thread = thread
        context.insert(reply)

        let files: [(name: String, mime: String, data: Data)] = [
            (pdfName, "application/pdf", pdf()),
            (htmlName, "text/html", html()),
            (pngName, "image/png", png()),
            (svgName, "image/svg+xml", svg()),
        ]
        for (index, file) in files.enumerated() {
            // A fresh id per launch, so a cached file from an earlier run can never stand in
            // for this one. Lowercase hex, as `ArtifactCache.isValidID` requires.
            let id = UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: "")
            guard (try? cache.store(id: id, mime: file.mime, data: file.data)) != nil else { continue }
            let artifact = TurnArtifact(artifactID: id, filename: file.name, mime: file.mime,
                                        byteCount: file.data.count, sha256: "", sortIndex: index)
            artifact.turn = reply
            context.insert(artifact)
        }
        try? context.save()
        PushRouter.shared.pendingTap = PushTap(jobId: nil, conversationId: conversationId)
    }

    // MARK: - The fixtures, drawn at launch so nothing binary is checked in

    /// Several pages of text, so the PDF preview is a scroll view with somewhere to scroll,
    /// which is what used to swallow the swipe down.
    private static func pdf() -> Data {
        let page = CGRect(x: 0, y: 0, width: 612, height: 792)
        return UIGraphicsPDFRenderer(bounds: page).pdfData { context in
            for number in 1...3 {
                context.beginPage()
                let text = "Probe report, page \(number)\n\n"
                    + String(repeating: "A returned file must always have a way to close. ", count: 40)
                text.draw(in: page.insetBy(dx: 48, dy: 48),
                          withAttributes: [.font: UIFont.systemFont(ofSize: 16)])
            }
        }
    }

    private static func html() -> Data {
        let rows = (1...80).map { "<p>Row \($0) of a page long enough to scroll.</p>" }.joined()
        return Data("""
            <!doctype html><html><head><meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <title>Probe page</title></head><body><h1>Probe page</h1>\(rows)</body></html>
            """.utf8)
    }

    private static func png() -> Data {
        let size = CGSize(width: 600, height: 400)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        return UIGraphicsImageRenderer(size: size, format: format).pngData { context in
            UIColor.systemTeal.setFill()
            context.fill(CGRect(origin: .zero, size: size))
            UIColor.systemOrange.setFill()
            for bar in 0..<6 {
                let height = CGFloat(60 + bar * 45)
                context.fill(CGRect(x: 40 + CGFloat(bar) * 90, y: size.height - 30 - height,
                                    width: 60, height: height))
            }
        }
    }

    private static func svg() -> Data {
        Data("""
            <svg xmlns="http://www.w3.org/2000/svg" width="300" height="200" viewBox="0 0 300 200">
            <rect width="300" height="200" fill="#2a9d8f"/>
            <circle cx="150" cy="100" r="60" fill="#e9c46a"/>
            </svg>
            """.utf8)
    }
}
