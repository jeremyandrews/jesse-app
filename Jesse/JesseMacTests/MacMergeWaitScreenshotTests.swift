import XCTest
import AppKit
import SwiftUI
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// DJ12: a run awaiting its owner's merge, as the Mac draws it, rendered off screen the way
/// `MacWorkScreenshotTests` does (an `NSHostingView` in a window placed off every display,
/// drawn with `cacheDisplay(in:to:)`). Nothing is captured from the screen and nothing posts but
/// the last look the board itself posts.
///
/// Skipped unless the run asks for it: `TEST_RUNNER_DJ12_BRIDGE=host,port,token` on the
/// `xcodebuild test` line, pointed at a scratch bridge (or a stand-in serving the wire
/// fixtures) that lists an `awaiting_merge` run with its `merge_wait`.
///
/// - `DJ12_RUN`: the awaiting-merge run.
/// - `DJ12_PROJECT_SLUG`: its project, whose Your turn lists it.
/// - `DJ12_SHOTS_DIR`: where the PNGs land.
@MainActor
final class MacMergeWaitScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var config: JesseConfig!

    override func setUp() async throws {
        try await super.setUp()
        guard let raw = ProcessInfo.processInfo.environment["DJ12_BRIDGE"] else {
            throw XCTSkip("screenshots are taken only with DJ12_BRIDGE set")
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

    private func env(_ key: String) throws -> String {
        try XCTUnwrap(ProcessInfo.processInfo.environment[key], key)
    }

    private func render<V: View>(_ view: V, _ name: String, width: CGFloat = 900,
                                 height: CGFloat) async throws {
        let host = NSHostingView(rootView: view
            .frame(width: width, height: height, alignment: .topLeading)
            .background(Color(nsColor: .windowBackgroundColor)))
        let window = makeTestWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: width,
                                                        height: height),
                                    styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        windows.append(window)
        // Let `.task` loads land and the list settle.
        for _ in 0..<60 {
            try await Task.sleep(for: .milliseconds(50))
            host.layoutSubtreeIfNeeded()
        }
        let dir = URL(fileURLWithPath: try env("DJ12_SHOTS_DIR"), isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        host.display()
        let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = dir.appendingPathComponent(name)
        try png.write(to: url)
        print("DJ12-SHOT \(url.path)")
    }

    func testDJ12MacMergeWaitShots() async throws {
        let make: @MainActor () -> JesseBridgeClient = { [config] in
            JesseBridgeClient(config: config!)
        }
        let runID = try env("DJ12_RUN")
        let slug = try env("DJ12_PROJECT_SLUG")
        let strands = StrandsModel(makeClient: make)
        await strands.refresh()
        let runs = StudioRunsModel(makeClient: make)
        await runs.loadLastLook()
        await runs.load()
        let waiting = try XCTUnwrap(runs.runs.first { $0.runID == runID })
        XCTAssertEqual(waiting.status, .awaitingMerge)
        XCTAssertNotNil(StudioRunSemantics.rowMergeLink(waiting), "the row's link")
        XCTAssertTrue(runs.runs(filter: .needsMe, strand: nil).contains { $0.runID == runID })

        // The Runs list under Needs me: the awaiting-merge row with its pull request link.
        let key = StudioRunFilter.storageKey
        let before = UserDefaults.standard.string(forKey: key)
        defer { UserDefaults.standard.set(before, forKey: key) }
        UserDefaults.standard.set(StudioRunFilter.needsMe.rawValue, forKey: key)
        try await render(StudioRunsScreen(model: runs) {}, "dj12-mac-1-runs-needs-me.png",
                         height: 520)

        // Every run, the awaiting-merge one among the live ones.
        UserDefaults.standard.set(StudioRunFilter.all.rawValue, forKey: key)
        try await render(StudioRunsScreen(model: runs) {}, "dj12-mac-2-runs-all.png",
                         height: 900)

        // The run's detail: Awaiting your merge, the link, the reply composer.
        let detail = runs.detailModel(for: runID)
        await detail.refresh()
        XCTAssertEqual(detail.status, .awaitingMerge)
        XCTAssertNotNil(detail.run?.mergeWait)
        try await render(NavigationStack { StudioRunDetailView(model: detail) },
                         "dj12-mac-3-run-detail.png", height: 900)

        // The project's view: Your turn lists the run with its link.
        try await render(NavigationStack {
            WorkProjectView(slug: slug, strands: strands, runs: runs, opener: StrandOpener())
        }, "dj12-mac-4-project-your-turn.png", height: 1_000)

        // The whole page: the detail with Details open, top to bottom.
        try await render(NavigationStack {
            StudioRunDetailView(model: detail, showsDetails: true)
        }, "dj12-mac-5-run-detail-full-page.png", height: 1_500)
    }
}
