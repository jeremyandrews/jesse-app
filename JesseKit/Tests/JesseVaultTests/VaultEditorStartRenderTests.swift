import XCTest
import SwiftUI
@testable import JesseVault
#if os(iOS)
import UIKit
#endif

// **The editor where it opens, drawn.** The positions are asserted as numbers in
// `VaultEditorStartTests`; this draws the screen those numbers are about, through the
// SHIPPING `VaultNoteEditorView` hosted in a window the size of the smallest iPhone, and
// writes a PNG per start. In process: no app install, no UI automation, nothing on a
// device.
//
// The editor only. The reader is a SwiftUI `ScrollView` over a `LazyVStack`, and in a test
// process no scroll of it (programmatic, or a search hit's `scrollTo`) rebuilds its rows,
// so a picture of it scrolled would be a picture of nothing. iOS only: the Mac half of the
// same screen needs a macOS 26 test host.
//
// `EDITOR_START_PNG_DIR` (as `TEST_RUNNER_EDITOR_START_PNG_DIR` through xcodebuild)
// chooses where the files land, which is how the ones on the pull request were collected.
@MainActor
final class VaultEditorStartRenderTests: XCTestCase {

    #if os(iOS)

    private var container: URL!
    private let path = "Projects/Arch-Notes.md"

    /// A note long enough to scroll, with frontmatter, headings, a list, a table and code.
    private var note: String {
        var lines = ["---", "title: Arch notes", "tags: [house, survey]", "---", "# Arch notes", ""]
        for section in 1...12 {
            lines.append("## Section \(section)")
            lines.append("")
            lines.append("Paragraph \(section) talks about the arch over the cellar door, "
                         + "the crack above the keystone, and what the surveyor said about it.")
            lines.append("")
            lines.append("- Check the mortar at course \(section)")
            lines.append("- Photograph the keystone from the stair")
            lines.append("")
            if section == 6 {
                lines += ["| Course | Mortar | Note |", "| --- | --- | --- |",
                          "| 5 | lime | sound |", "| 6 | cement | cracked |", ""]
            }
        }
        return lines.joined(separator: "\n") + "\n"
    }

    override func setUp() async throws {
        try await super.setUp()
        container = VaultFixture.makeDirectory()
    }

    override func tearDown() async throws {
        VaultFixture.cleanUp(container)
        try await super.tearDown()
    }

    private var directory: URL {
        get throws {
            let url = ProcessInfo.processInfo.environment["EDITOR_START_PNG_DIR"]
                .map { URL(fileURLWithPath: $0, isDirectory: true) }
                ?? FileManager.default.temporaryDirectory
                    .appendingPathComponent("editor-start-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
            return url
        }
    }

    private let frame = CGRect(x: 0, y: 0, width: 375, height: 667)

    /// Host `view` in an iPhone SE sized window.
    private func host<V: View>(_ view: V) -> UIWindow {
        let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene
        let window = scene.map { UIWindow(windowScene: $0) } ?? UIWindow(frame: frame)
        window.frame = frame
        window.rootViewController = UIHostingController(rootView: view)
        window.makeKeyAndVisible()
        return window
    }

    private func settle(_ seconds: TimeInterval) {
        RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    }

    /// Write what `window` shows as a PNG.
    private func write(_ window: UIWindow, name: String, in directory: URL) throws {
        window.layoutIfNeeded()
        // `layer.render`, not `drawHierarchy`: the test process has no screen for the
        // latter to snapshot, and it hands back white.
        let image = UIGraphicsImageRenderer(bounds: frame).image { context in
            window.layer.render(in: context.cgContext)
        }
        let png = try XCTUnwrap(image.pngData(), "\(name) would not encode")
        XCTAssertGreaterThan(distinctShades(image), 8, "\(name) rendered flat")
        let url = directory.appendingPathComponent(name)
        try png.write(to: url)
        print("EDITOR START PNG: \(url.path)")
    }

    /// Host `view`, let it settle, and write it as a PNG.
    private func shoot<V: View>(_ view: V, name: String, settle seconds: TimeInterval,
                                in directory: URL) throws {
        let window = host(view)
        defer { window.isHidden = true }
        settle(seconds)
        try write(window, name: name, in: directory)
    }

    /// How many different grey levels a coarse sample of the image holds. A blank render is
    /// one or two; a screen of text is dozens.
    private func distinctShades(_ image: UIImage) -> Int {
        guard let cg = image.cgImage else { return 0 }
        let width = 64, height = 64
        var pixels = [UInt8](repeating: 0, count: width * height)
        guard let context = CGContext(data: &pixels, width: width, height: height,
                                      bitsPerComponent: 8, bytesPerRow: width,
                                      space: CGColorSpaceCreateDeviceGray(),
                                      bitmapInfo: CGImageAlphaInfo.none.rawValue)
        else { return 0 }
        context.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
        return Set(pixels).count
    }

    func testTheEditorRendersWhereItOpens() throws {
        let out = try directory

        let document = VaultNoteDocument.parse(path: path, text: note)
        let heading = try XCTUnwrap(document.blocks.first { $0.text == "Section 6" })

        // 1. The editor Edit opens when the reader's top block is Section 6's heading: that
        //    line at the top, the caret on it.
        let stash = VaultEditStash(directory: container.appendingPathComponent("stash"))
        let editor = VaultNoteEditorModel(path: path, start: .caret(line: heading.line),
                                          writer: FakeNoteWriter(text: note), stash: stash)
        try shoot(NavigationStack { VaultNoteEditorView(path: path, model: editor) },
                  name: "1-editor-from-edit.png", settle: 2, in: out)

        // 2. Edit here on the table: its source lines selected.
        let table = try XCTUnwrap(document.blocks.first {
            if case .table = $0.kind { return true }
            return false
        })
        let lines = try XCTUnwrap(document.sourceLines(ofBlock: table.id))
        let selecting = VaultNoteEditorModel(path: path, start: .select(lines: lines),
                                             writer: FakeNoteWriter(text: note), stash: stash)
        try shoot(NavigationStack { VaultNoteEditorView(path: path, model: selecting) },
                  name: "2-editor-from-edit-here.png", settle: 2, in: out)
    }

    #endif
}
