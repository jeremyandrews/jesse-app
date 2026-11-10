import XCTest
import AppKit
import SwiftUI
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// The Mac's Runs list holding every run a bridge lists, and one run's detail with its report,
/// rendered off screen as `MacStudioRunsScreenshotTests` does. Nothing is captured from the
/// screen and no permission is asked for.
///
/// Skipped unless the run asks for it: `TEST_RUNNER_PD1_BRIDGE=host,port,token` and
/// `TEST_RUNNER_PD1_REPORT_RUN=<run id>` on the `xcodebuild test` line, pointed at a scratch
/// bridge serving a COPY of a run store. Every call is a read. The PNGs land in
/// `build/screenshots/pd1-mac/`.
@MainActor
final class MacStudioRunsEveryRunScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var config: JesseConfig!

    override func setUp() async throws {
        try await super.setUp()
        guard let raw = ProcessInfo.processInfo.environment["PD1_BRIDGE"] else {
            throw XCTSkip("taken only with PD1_BRIDGE set")
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
            .appendingPathComponent("build/screenshots/pd1-mac", isDirectory: true)
    }

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
        print("PD1-SHOT \(url.path)")
    }

    func testPD1MacRunsListAndReport() async throws {
        let listed = try await JesseBridgeClient(config: config).listRuns()
        let runs = StudioRunsModel(makeClient: { [config] in JesseBridgeClient(config: config!) })
        await runs.load()
        XCTAssertEqual(runs.runs(strand: nil).count, listed.runs.count,
                       "every run the bridge lists is in the list")
        // Tall enough for every row at once.
        try await render(StudioRunsScreen(model: runs) {}, "1-runs-list.png",
                         height: CGFloat(140 + 76 * listed.runs.count))

        let id = try XCTUnwrap(ProcessInfo.processInfo.environment["PD1_REPORT_RUN"])
        let detail = runs.detailModel(for: id)
        await detail.refresh()
        XCTAssertNotNil(detail.run?.latestReportPath)
        try await render(NavigationStack { StudioRunDetailView(model: detail) },
                         "2-run-detail-report.png", height: 860)
    }
}
