import XCTest
import AppKit
import SwiftUI
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// The runs list's filter bar and an archive, as the Mac draws them, against a running bridge,
/// rendered off screen as `MacWorkScreenshotTests` does: nothing is captured from the screen
/// and no permission is asked for.
///
/// Skipped unless the run asks for it: `TEST_RUNNER_HO4_BRIDGE=host,port,token` on the
/// `xcodebuild test` line, pointed at a scratch bridge serving a COPY of a run store. It
/// archives `HO4_SWIPE_RUN` and takes it back with Undo. The PNGs land in `HO4_SHOTS_DIR`.
@MainActor
final class MacRunsFiltersScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var config: JesseConfig!

    override func setUp() async throws {
        try await super.setUp()
        guard let raw = ProcessInfo.processInfo.environment["HO4_BRIDGE"] else {
            throw XCTSkip("screenshots are taken only with HO4_BRIDGE set")
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
        UserDefaults.standard.removeObject(forKey: StudioRunFilter.storageKey)
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
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: width,
                                                  height: height),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        windows.append(window)
        for _ in 0..<60 {
            try await Task.sleep(for: .milliseconds(50))
            host.layoutSubtreeIfNeeded()
        }
        let dir = URL(fileURLWithPath: try env("HO4_SHOTS_DIR"), isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        host.display()
        let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = dir.appendingPathComponent(name)
        try png.write(to: url)
        print("HO4-SHOT \(url.path)")
    }

    func testHO4MacShots() async throws {
        let make: @MainActor () -> JesseBridgeClient = { [config] in
            JesseBridgeClient(config: config!)
        }
        let runs = StudioRunsModel(makeClient: make)
        await runs.load()
        await runs.loadArchived()
        XCTAssertFalse(runs.runs.isEmpty)
        XCTAssertNotNil(runs.counts.needsMe, "a bridge that counts the filters")

        for f in [StudioRunFilter.all, .needsMe, .archived] {
            UserDefaults.standard.set(f.rawValue, forKey: StudioRunFilter.storageKey)
            try await render(StudioRunsScreen(model: runs) {}, "mac-filter-\(f.rawValue).png",
                             height: 1_000)
        }

        // An archive, as the row's context menu sends it: the row leaves, Undo shows.
        UserDefaults.standard.set(StudioRunFilter.finished.rawValue,
                                  forKey: StudioRunFilter.storageKey)
        let id = try env("HO4_SWIPE_RUN")
        let run = try XCTUnwrap(runs.runs.first { $0.runID == id })
        await runs.archive(run)
        XCTAssertNil(runs.alert)
        XCTAssertEqual(runs.undo?.runIDs, [id])
        try await render(StudioRunsScreen(model: runs) {}, "mac-archive-undo.png", height: 1_000)
        await runs.undoArchive()
        XCTAssertTrue(runs.runs.contains { $0.runID == id }, "back after Undo")
    }
}
