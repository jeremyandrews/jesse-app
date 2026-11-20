import XCTest
import AppKit
import SwiftUI
import JesseNetworking
import JesseTodayDisplay
@testable import Jesse_Mac

/// The Work segment as the Mac draws it, against a running bridge, rendered off screen: an
/// `NSHostingView` in a window placed off every display, drawn with `cacheDisplay(in:to:)`, as
/// `MacStudioRunsScreenshotTests` does. Nothing is captured from the screen and no permission is
/// asked for.
///
/// Skipped unless the run asks for it: `TEST_RUNNER_HO2_BRIDGE=host,port,token` on the
/// `xcodebuild test` line, pointed at a scratch bridge serving a COPY of a run store and of the
/// vault's Strands notes. Nothing here posts but the last look the board itself posts. The PNGs
/// land in `HO2_SHOTS_DIR`.
///
/// - `HO2_PROJECT_SLUG`: a project whose view has Your turn, Moving and Next.
/// - `HO2_RUN`: a `needs_you` run.
@MainActor
final class MacWorkScreenshotTests: XCTestCase {

    private var windows: [NSWindow] = []
    private var config: JesseConfig!

    override func setUp() async throws {
        try await super.setUp()
        let environment = ProcessInfo.processInfo.environment
        guard let raw = environment["HO2_BRIDGE"] ?? environment["XR2_BRIDGE"] else {
            throw XCTSkip("screenshots are taken only with HO2_BRIDGE (or XR2_BRIDGE) set")
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
                                 height: CGFloat, shotsKey: String = "HO2_SHOTS_DIR") async throws {
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
        let dir = URL(fileURLWithPath: try env(shotsKey), isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        host.display()
        let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = dir.appendingPathComponent(name)
        try png.write(to: url)
        print("HO2-SHOT \(url.path)")
    }

    /// HO5: the whole Work board as the Mac draws it, every row, Dormant expanded, at a window
    /// height that holds them all.
    func testHO5MacBoard() async throws {
        guard let prefix = ProcessInfo.processInfo.environment["HO5_PREFIX"] else {
            throw XCTSkip("HO5 board shots need HO5_PREFIX")
        }
        let make: @MainActor () -> JesseBridgeClient = { [config] in
            JesseBridgeClient(config: config!)
        }
        let strands = StrandsModel(makeClient: make)
        await strands.refresh()
        let runs = StudioRunsModel(makeClient: make)
        await runs.loadLastLook()
        await runs.load()
        let opener = StrandOpener()
        try await render(NavigationStack {
            StrandsListView(model: strands, opener: opener, runs: runs, dormantStartsOpen: true)
                .navigationTitle("Work")
        }, "\(prefix)-mac-board.png", width: 1_000, height: 2_000)
        if let slug = ProcessInfo.processInfo.environment["HO5_PROJECT_SLUG"] {
            try await render(NavigationStack {
                WorkProjectView(slug: slug, strands: strands, runs: runs, opener: opener)
            }, "\(prefix)-mac-project-\(slug).png", height: 1_000)
        }
    }

    func testHO2MacShots() async throws {
        let make: @MainActor () -> JesseBridgeClient = { [config] in
            JesseBridgeClient(config: config!)
        }
        let strands = StrandsModel(makeClient: make)
        await strands.refresh()
        let runs = StudioRunsModel(makeClient: make)
        await runs.loadLastLook()
        await runs.load()
        XCTAssertFalse(runs.runs.isEmpty)
        XCTAssertNotNil(runs.lastLook)
        let opener = StrandOpener()

        // The whole Work segment, as the Today tab shows it.
        UserDefaults.standard.set(TodaySegment.strands.rawValue,
                                  forKey: TodayViewPreferences.segmentKey)
        let day = TodayDashboardModel(makeClient: make)
        try await render(NavigationStack {
            TodayListView(model: day, strands: strands, strandOpener: opener, runs: runs)
        }, "mac-6-work-segment.png", height: 1_100)

        // The board with Since your last look expanded.
        try await render(NavigationStack {
            StrandsListView(model: strands, opener: opener, runs: runs)
                .navigationTitle("Work")
        }
        .environment(\.workSinceStartsExpanded, true),
                         "mac-1-work-board-since-expanded.png", height: 1_100)

        // A project's view.
        let slug = try env("HO2_PROJECT_SLUG")
        try await render(NavigationStack {
            WorkProjectView(slug: slug, strands: strands, runs: runs, opener: opener)
        }, "mac-2-project-view.png", height: 1_000)

        // A question's detail, then the same with Details open.
        let runID = try env("HO2_RUN")
        let detail = runs.detailModel(for: runID)
        await detail.refresh()
        XCTAssertEqual(detail.status, .needsYou)
        try await render(NavigationStack { StudioRunDetailView(model: detail) },
                         "mac-3-run-detail-question.png", height: 900)
        try await render(NavigationStack {
            StudioRunDetailView(model: detail, showsDetails: true)
        }, "mac-4-run-detail-details.png", height: 1_300)

        // Every run, as the Runs button opens it.
        try await render(StudioRunsScreen(model: runs) {}, "mac-5-runs-list.png", height: 1_120)
    }

    /// XR2: a run in review, as the Mac draws it. Gated on `XR2_BRIDGE=host,port,token` (a
    /// bridge, or a read only copy of one, serving a `reviewing` run and a reported run with an
    /// answered review). `XR2_RUN_REVIEWING`, `XR2_RUN_REVIEWED`; the PNGs land in
    /// `XR2_SHOTS_DIR`.
    func testXR2MacReviewShots() async throws {
        guard let raw = ProcessInfo.processInfo.environment["XR2_BRIDGE"] else {
            throw XCTSkip("taken only with XR2_BRIDGE set")
        }
        let parts = raw.split(separator: ",").map(String.init)
        let xr2 = JesseConfig(host: parts[0], port: Int(parts[1]) ?? 0, token: parts[2])
        let make: @MainActor () -> JesseBridgeClient = { JesseBridgeClient(config: xr2) }
        let runs = StudioRunsModel(makeClient: make)
        await runs.load()
        let reviewingID = try env("XR2_RUN_REVIEWING")
        let reviewedID = try env("XR2_RUN_REVIEWED")
        let reviewing = try XCTUnwrap(runs.runs.first { $0.runID == reviewingID })
        XCTAssertEqual(reviewing.status, .reviewing)
        XCTAssertTrue(runs.runs(filter: .moving, strand: nil).contains { $0.runID == reviewingID })
        let shots = "XR2_SHOTS_DIR"

        // The Runs list under Moving, the reviewing row naming its reviewer.
        let key = StudioRunFilter.storageKey
        let before = UserDefaults.standard.string(forKey: key)
        defer { UserDefaults.standard.set(before, forKey: key) }
        UserDefaults.standard.set(StudioRunFilter.moving.rawValue, forKey: key)
        try await render(StudioRunsScreen(model: runs) {},
                         "xr2-mac-1-runs-moving.png", height: 900, shotsKey: shots)

        // The run in review: Reviewing, the line where the composer would be, its Review.
        let detail = runs.detailModel(for: reviewingID)
        await detail.refresh()
        XCTAssertEqual(detail.status, .reviewing)
        XCTAssertFalse(detail.showsComposer)
        try await render(NavigationStack { StudioRunDetailView(model: detail) },
                         "xr2-mac-2-run-detail-reviewing.png", height: 900, shotsKey: shots)

        // A reviewed run: its answered review, which opens the note.
        let reviewed = runs.detailModel(for: reviewedID)
        await reviewed.refresh()
        XCTAssertFalse(reviewed.run?.reviews.isEmpty ?? true)
        try await render(NavigationStack { StudioRunDetailView(model: reviewed) },
                         "xr2-mac-3-run-detail-reviewed.png", height: 900, shotsKey: shots)

        // The whole page: the run in review with Details open, top to bottom.
        try await render(NavigationStack {
            StudioRunDetailView(model: detail, showsDetails: true)
        }, "xr2-mac-4-run-detail-reviewing-full-page.png", height: 1_400, shotsKey: shots)
    }
}
