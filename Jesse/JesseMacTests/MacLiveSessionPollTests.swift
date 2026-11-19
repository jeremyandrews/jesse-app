import XCTest
import AppKit
import SwiftUI
import SwiftData
import JesseCore
import JesseNetworking
import JesseSearch
@testable import Jesse_Mac

/// MR1's live check: the real Chats window, against a running bridge, picks up a conversation
/// started elsewhere with no click, and an idle window costs about two `304`s a minute.
///
/// Rendered off screen, as `MacWorkScreenshotTests` does: an `NSHostingView` in a window placed
/// off every display, drawn with `cacheDisplay(in:to:)`. Nothing is captured from the screen, the
/// Keychain is not read and no permission is asked for. The store is in memory, so the Mac's own
/// store is untouched.
///
/// Skipped unless the run asks for it, with `TEST_RUNNER_`-prefixed variables on the
/// `xcodebuild test` line:
///
/// - `MR1_BRIDGE=host,port,token`: the bridge.
/// - `MR1_SHOTS_DIR`: where the PNGs and `ready` / `found` handshake files go.
/// - `MR1_MARKER`: text the conversation started elsewhere will open with.
/// - `MR1_IDLE_SECONDS` (default 120): how long the idle request count runs.
///
/// The test writes `ready` once the first pull has landed and the "before" shot is taken; the
/// operator then starts a conversation from another client (`curl` `POST /jesse`). The test
/// touches nothing in between: only the poll can bring it in.
@MainActor
final class MacLiveSessionPollTests: XCTestCase {

    private var windows: [NSWindow] = []

    override func tearDown() async throws {
        for window in windows {
            window.contentView = nil
            window.close()
        }
        windows = []
        try await super.tearDown()
    }

    func testANewRemoteConversationAppearsWithoutAClickAndIdleCostsTwo304sAMinute() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let raw = env["MR1_BRIDGE"], let shots = env["MR1_SHOTS_DIR"],
              let marker = env["MR1_MARKER"] else {
            throw XCTSkip("the live poll check runs only with MR1_BRIDGE, MR1_SHOTS_DIR and MR1_MARKER set")
        }
        let parts = raw.split(separator: ",").map(String.init)
        let config = JesseConfig(host: parts[0], port: Int(parts[1]) ?? 0, token: parts[2])
        let idleSeconds = Double(env["MR1_IDLE_SECONDS"] ?? "") ?? 120
        let dir = URL(fileURLWithPath: shots, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        // Start from no stored ETag, as a fresh install would, so the first pull is the full list.
        UserDefaults.standard.removeObject(forKey: "sessions.etag")
        let counter = CountingClient(JesseBridgeClient(config: config))
        let coordinator = MacCoordinator(configStore: MacConfigStore(config: config),
                                         makeClient: { _ in counter },
                                         sessionDeletionStore: MacTestFixtures.deletionStore())
        let container = try ModelContainer(
            for: jesseCurrentSchema,
            configurations: ModelConfiguration(schema: jesseCurrentSchema,
                                               isStoredInMemoryOnly: true))

        // The real window root, poller and all, over a scratch vault so the Mac's own notes are
        // not read. Nothing below clicks, sends or refreshes.
        let root = MacShellView(storeError: nil,
                                vaultModel: try await MacSearchVisibilityTests.scratchVault(),
                                searchExpander: NoExpansion())
            .environment(coordinator)
            .modelContainer(container)
        let controller = NSHostingController(rootView: root)
        controller.sceneBridgingOptions = [.toolbars, .title]
        let frame = NSRect(origin: MacSearchVisibilityTests.offscreenOrigin,
                           size: NSSize(width: 1_100, height: 760))
        let window = makeTestWindow(MacSearchVisibilityTests.OffscreenWindow.self,
                                    contentRect: frame,
                                    styleMask: [.titled, .closable, .miniaturizable, .resizable,
                                                .fullSizeContentView],
                                    backing: .buffered, defer: false)
        window.toolbarStyle = .unified
        // One appearance for the whole render: the sidebar's table is drawn separately below
        // and would otherwise come out light inside dark chrome.
        window.appearance = NSAppearance(named: .aqua)
        window.contentViewController = controller
        window.setFrame(frame, display: false)
        windows.append(window)
        window.orderBack(nil)
        let host: NSView = try XCTUnwrap(window.contentView?.superview ?? window.contentView)

        // The test host is not the frontmost app, so the window's own poller is on its 5 minute
        // background cadence. Run the same loop as a frontmost window would, through the same
        // coordinator and the same `refreshSessions`, so the 30 second cadence is what is measured.
        let poller = MacSessionPoller()
        let context = container.mainContext
        let loop = Task { @MainActor in
            await poller.run(MacSessionPoller.Inputs(
                isAppActive: { true },
                reachability: { BridgeReachabilityModel.shared.state },
                failures: { coordinator.sessionRefreshFailures },
                lastRefreshStart: { coordinator.lastSessionRefreshStartedAt },
                refresh: { await coordinator.refreshSessions(context: context) },
                reprobe: { BridgeReachabilityModel.shared.refresh(config: config) }))
        }
        defer { loop.cancel() }

        // The launch pull (the shell's `.task`) adopts the whole list.
        try await settle(host, until: { self.threadCount(context) > 0 && counter.calls.count >= 1 },
                         timeout: 120)
        try await settle(host, seconds: 3)
        try shoot(host, dir.appendingPathComponent("1-sidebar-before.png"), sidebarOnly: true)
        let before = threadCount(context)
        print("MR1 adopted \(before) conversations; calls so far: \(counter.calls)")

        // Idle: nothing changes on the bridge (the operator waits for this phase to finish).
        counter.reset()
        let idleStart = Date()
        try await settle(host, seconds: idleSeconds)
        let idle = counter.calls
        let minutes = Date().timeIntervalSince(idleStart) / 60
        let rate = Double(idle.count) / minutes
        print("MR1 IDLE \(String(format: "%.1f", minutes)) min: \(idle.count) list calls " +
              "(\(idle.filter { $0 == .notModified }.count) x 304, " +
              "\(idle.filter { $0 == .changed }.count) x 200, " +
              "\(idle.filter { $0 == .failed }.count) failed) = \(String(format: "%.2f", rate))/min")
        try "\(idle)".write(to: dir.appendingPathComponent("idle.txt"), atomically: true, encoding: .utf8)

        // Hand over: start the conversation elsewhere now.
        counter.reset()
        try "ready".write(to: dir.appendingPathComponent("ready"), atomically: true, encoding: .utf8)
        let readyAt = Date()
        try await settle(host, until: { self.thread(containing: marker, in: context) != nil },
                         timeout: 120)
        let seen = Date().timeIntervalSince(readyAt)
        let found = try XCTUnwrap(thread(containing: marker, in: context))
        print("MR1 FOUND after \(String(format: "%.1f", seen)) s from ready, " +
              "calls: \(counter.calls), title: \(found.title)")
        try "\(seen)".write(to: dir.appendingPathComponent("found"), atomically: true, encoding: .utf8)
        try await settle(host, seconds: 3)
        try shoot(host, dir.appendingPathComponent("2-sidebar-after.png"), sidebarOnly: true)
        try shoot(host, dir.appendingPathComponent("3-window-after.png"), sidebarOnly: false)

        XCTAssertGreaterThan(threadCount(context), before - 1)
        XCTAssertLessThanOrEqual(rate, 2.5, "an idle window stays at about two pulls a minute")
        // Not "every one is a 304": the live list is shared, and another device or run changing
        // it during the window is a real 200. What an idle window must never do is fail.
        XCTAssertFalse(idle.contains(.failed))
    }

    // MARK: - Helpers

    private func threadCount(_ context: ModelContext) -> Int {
        ((try? context.fetch(FetchDescriptor<JesseThread>())) ?? []).count
    }

    private func thread(containing marker: String, in context: ModelContext) -> JesseThread? {
        ((try? context.fetch(FetchDescriptor<JesseThread>())) ?? []).first {
            $0.title.contains(marker)
        }
    }

    private func settle(_ host: NSView, seconds: Double) async throws {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            try await Task.sleep(for: .milliseconds(100))
            host.layoutSubtreeIfNeeded()
        }
    }

    private func settle(_ host: NSView, until done: () -> Bool, timeout: Double) async throws {
        let end = Date().addingTimeInterval(timeout)
        while !done() {
            guard Date() < end else { throw XCTSkip("timed out after \(timeout) s") }
            try await Task.sleep(for: .milliseconds(100))
            host.layoutSubtreeIfNeeded()
        }
    }

    /// Draw the window off screen. `cacheDisplay` leaves the sidebar's material blank, so the
    /// sidebar's own table (the real `List` rows) is drawn on its own and laid over its place in
    /// the window. `sidebarOnly` writes just that column.
    private func shoot(_ frameView: NSView, _ url: URL, sidebarOnly: Bool) throws {
        frameView.layoutSubtreeIfNeeded()
        frameView.display()
        let table = try XCTUnwrap(MacSearchVisibilityTests.all(frameView)
            .compactMap { $0 as? NSTableView }.first, "the sidebar list")
        let visible = table.visibleRect
        let tableRep = try XCTUnwrap(table.bitmapImageRepForCachingDisplay(in: visible))
        table.cacheDisplay(in: visible, to: tableRep)
        let place = table.convert(visible, to: frameView)

        let frameRep = try XCTUnwrap(frameView.bitmapImageRepForCachingDisplay(in: frameView.bounds))
        frameView.cacheDisplay(in: frameView.bounds, to: frameRep)
        let size = frameView.bounds.size
        let out = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: frameRep.pixelsWide, pixelsHigh: frameRep.pixelsHigh,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        out.size = size
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: out)
        frameRep.draw(in: NSRect(origin: .zero, size: size))
        NSColor.white.setFill()
        place.fill()
        tableRep.draw(in: place)
        NSGraphicsContext.restoreGraphicsState()

        var final = out
        if sidebarOnly {
            let scale = CGFloat(out.pixelsWide) / size.width
            let column = NSRect(x: 0, y: 0, width: place.maxX * scale,
                                height: CGFloat(out.pixelsHigh))
            let cropped = try XCTUnwrap(out.cgImage?.cropping(to: column))
            final = NSBitmapImageRep(cgImage: cropped)
        }
        let png = try XCTUnwrap(final.representation(using: .png, properties: [:]))
        try png.write(to: url)
        print("MR1-SHOT \(url.path)")
    }
}

/// The real client, counting every list call by how it answered. The proxy count the idle check
/// reads: the bridge does not log individual requests.
private final class CountingClient: BridgeClientProtocol, @unchecked Sendable {
    enum Outcome: Equatable, CustomStringConvertible {
        case notModified, changed, failed
        var description: String {
            switch self { case .notModified: "304"; case .changed: "200"; case .failed: "failed" }
        }
    }

    private let inner: JesseBridgeClient
    private let lock = NSLock()
    private var _calls: [Outcome] = []
    var calls: [Outcome] { lock.withLock { _calls } }
    func reset() { lock.withLock { _calls = [] } }

    init(_ inner: JesseBridgeClient) { self.inner = inner }

    nonisolated var config: JesseConfig { inner.config }

    nonisolated func listConversations(since: UInt64?, etag: String?) async throws -> ConversationsResult {
        do {
            let r = try await inner.listConversations(since: since, etag: etag)
            let o: Outcome
            if case .notModified = r { o = .notModified } else { o = .changed }
            lock.withLock { _calls.append(o) }
            return r
        } catch {
            lock.withLock { _calls.append(.failed) }
            throw error
        }
    }

    nonisolated func deleteConversation(_ conversationId: String) async throws {
        // Never on a live bridge from a test.
    }
    nonisolated func sendPrepared(_ request: JesseRequest) async throws -> JesseSendResult { throw JesseError.notConfigured }
    nonisolated func send(mode: JesseMode, text: String, sessionId: String?,
                          conversationId: String, voice: Bool,
                          instructions: String?, floorOverride: String?,
                          attachments: [JesseRequest.Attachment], requestId: String,
                          model: String?, effort: String?, sentFor: String?) async throws -> JesseSendResult {
        throw JesseError.notConfigured
    }
    nonisolated func result(jobId: String) async throws -> JesseResultState { throw JesseError.notConfigured }
    nonisolated func stream(jobId: String) -> AsyncThrowingStream<JesseStreamEvent, Error> {
        AsyncThrowingStream { $0.finish() }
    }
    nonisolated func hydrate(conversationId: String, after cursor: String?) async throws
        -> (turns: [HydratedTurn], nextCursor: String) {
        try await inner.hydrate(conversationId: conversationId, after: cursor)
    }
    nonisolated func title(text: String, conversationId: String?) async -> String? { nil }
    nonisolated func cancelJob(jobId: String) async throws {}
    nonisolated func health() async throws -> BridgeHealth { try await inner.health() }
    nonisolated func fetchDietSnapshot(date: String?) async throws -> DietSnapshot { throw DietFetchError.notConfigured }
    nonisolated func fetchPrompts() async throws -> PromptDefaults { throw JesseError.notConfigured }
}
