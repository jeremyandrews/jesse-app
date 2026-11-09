import XCTest
import AppKit
import SwiftUI
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// The Studio run screens as the Mac draws them, against a running bridge, rendered off
/// screen: an `NSHostingView` in a window placed off every display, drawn with
/// `cacheDisplay(in:to:)`, as `MacTranscriptScreenshotTests` does. Nothing is captured from the
/// screen and no permission is asked for.
///
/// Skipped unless the run asks for it: `TEST_RUNNER_HO1B_BRIDGE=host,port,token` on the
/// `xcodebuild test` line, pointed at a scratch bridge serving a COPY of a run store. Every
/// call these screens make here is a read. The PNGs land in `build/screenshots/ho1b-mac/`.
@MainActor
final class MacStudioRunsScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var config: JesseConfig!

    override func setUp() async throws {
        try await super.setUp()
        guard let raw = ProcessInfo.processInfo.environment["HO1B_BRIDGE"] else {
            throw XCTSkip("screenshots are taken only with HO1B_BRIDGE set")
        }
        let parts = raw.split(separator: ",").map(String.init)
        config = JesseConfig(host: parts[0], port: Int(parts[1]) ?? 0, token: parts[2])
    }

    override func tearDown() async throws {
        for window in windows {
            window.contentView = nil
            window.close()
        }
        windows = []
        try await super.tearDown()
    }

    private var directory: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("build/screenshots/ho1b-mac", isDirectory: true)
    }

    private func env(_ key: String) throws -> String {
        try XCTUnwrap(ProcessInfo.processInfo.environment[key], key)
    }

    private func client() -> JesseBridgeClient { JesseBridgeClient(config: config) }

    private func render<V: View>(_ view: V, _ name: String, width: CGFloat = 900,
                                 height: CGFloat) async throws {
        let host = NSHostingView(rootView: view
            .frame(width: width, height: height, alignment: .topLeading)
            .background(Color(nsColor: .windowBackgroundColor)))
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: width,
                                                  height: height),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        // Let `.task` loads land and the list settle.
        for _ in 0..<40 {
            try await Task.sleep(for: .milliseconds(50))
            host.layoutSubtreeIfNeeded()
        }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        host.display()
        let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = directory.appendingPathComponent(name)
        try png.write(to: url)
        print("HO1B-SHOT \(url.path)")
    }

    func testHO1BMacShots() async throws {
        let strands = StrandsModel(makeClient: { [config] in JesseBridgeClient(config: config!) })
        await strands.refresh()
        let runs = StudioRunsModel(makeClient: { [config] in JesseBridgeClient(config: config!) })
        await runs.load()
        XCTAssertFalse(runs.runs.isEmpty)
        let opener = StrandOpener()

        try await render(StrandsListView(model: strands, opener: opener), "1-board-badge.png",
                         height: 1_000)

        let slug = try env("HO1B_STRAND_SLUG")
        let strand = try XCTUnwrap(strands.snapshot?.strand(slug: slug))
        try await render(StrandDraftsBlock(strand: strand, runs: runs).padding(20),
                         "2-drafts-block.png", height: 520)

        try await render(StudioRunsScreen(model: runs) {}, "3-runs-list.png", height: 1_120)

        let reply = runs.detailModel(for: try env("HO1B_RUN_REPLY"))
        await reply.refresh()
        try await render(NavigationStack { StudioRunDetailView(model: reply) },
                         "4-run-detail-reply.png", height: 860)

        let cancel = runs.detailModel(for: try env("HO1B_RUN_CANCEL"))
        await cancel.refresh()
        try await render(NavigationStack { StudioRunDetailView(model: cancel) },
                         "5-run-detail-cancel.png", height: 900)
    }
}
