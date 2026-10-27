import Foundation
import os
import XCTest

/// THE MEASUREMENT behind the navigation cost work: how long the four everyday moves take
/// on a store of 1,000 conversations, against a control of 5.
///
///  (a) `newChat`: tap New conversation on the Chats list until the composer has the
///      keyboard.
///  (b) `backOut`: back out of the 200 turn conversation to the list.
///  (c) `tabCycle`: Chats, Today, Vault, Health, and back to Chats.
///  (d) `openLong`: open the 200 turn conversation.
///
/// Each runs five measured iterations under `XCTClockMetric` (and the navigation transition
/// signpost where there is a push or a pop). The app is launched with
/// `JESSE_UITEST_SCALE` (see `ScaleSeedUITestSeam`), so it opens a store of its own, seeded
/// once per install, and with `JESSE_UITEST_BRIDGE` pointed at a stub in this process that
/// serves the matching conversation list (so a pull runs the whole sync pass and changes
/// nothing, which is what a pull after a reply costs), the day file, and a 200 for the rest.
///
/// Every iteration is also bracketed by a `mark pf1-<scale>-<scenario>-<n>-begin/end` line in
/// the app's `render` category, so `scripts/nav-cost.sh` can read the run back out of the
/// simulator's log: the `nav` signpost intervals and `RenderProbe` body lines that fell
/// inside each window, per scenario.
///
/// NOTHING HERE ASSERTS A TIME, for the reason `UnreadBadgeRenderUITests` gives: a number
/// pinned in a test fails on a busy simulator. The numbers go in the pull request.
class NavigationCostUITestCase: XCTestCase {

    /// The scale a subclass measures. Overridden; never run at this level.
    class var scale: Int { 0 }

    static let probeTitle = "Scale probe, 200 turns"
    private let composerID = "composer.input"
    private let log = Logger(subsystem: "com.tag1.jesse", category: "render")

    private var stub: ScaleStub!
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        try XCTSkipIf(Self.scale == 0, "the base class measures nothing")
        // OPT IN. A measurement is minutes of simulator time and asserts nothing, so the
        // ordinary test run (and the local gate) skips it. `scripts/nav-cost.sh` sets it
        // through xcodebuild's `TEST_RUNNER_` prefix.
        try XCTSkipUnless(ProcessInfo.processInfo.environment["PF1_MEASURE"] == "1",
                          "set TEST_RUNNER_PF1_MEASURE=1 to measure")
        stub = try ScaleStub(count: Self.scale)
        app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_SCALE"] = String(Self.scale)
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = "127.0.0.1,\(stub.port),stub-token"
        app.launchEnvironment["JESSE_RENDER_PROBE"] = "1"
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launch()
        // A first launch seeds the store, which takes a while at 1,000 conversations.
        XCTAssertTrue(app.staticTexts[Self.probeTitle].waitForExistence(timeout: 900),
                      "the seeded store is up: \(stub.requestSummary)")
        settle()
    }

    override func tearDownWithError() throws {
        app?.terminate()
        stub?.stop()
        stub = nil
        try super.tearDownWithError()
    }

    // MARK: - The four scenarios

    func measureNewChat() {
        measureScenario("newChat", transition: true) {
            self.app.navigationBars.buttons["New conversation"].tap()
            let field = self.app.textViews[self.composerID]
            XCTAssertTrue(field.waitForExistence(timeout: 30), "the new conversation's composer")
            field.tap()
            XCTAssertTrue(self.waitFor(field, "hasKeyboardFocus == true"), "the composer has the keyboard")
        } reset: {
            self.backToList()
        }
    }

    func measureBackOut() {
        measureScenario("backOut", transition: true, prepare: {
            self.openProbe()
        }) {
            self.backToList()
        }
    }

    func measureTabCycle() {
        measureScenario("tabCycle", transition: false) {
            for tab in ["Today", "Vault", "Health", "Chats"] {
                let button = self.app.tabBars.buttons[tab]
                button.tap()
                XCTAssertTrue(self.waitFor(button, "isSelected == true"), "\(tab) selected")
            }
            XCTAssertTrue(self.app.staticTexts[Self.probeTitle].waitForExistence(timeout: 30),
                          "back on the Chats list")
        }
    }

    func measureOpenLong() {
        measureScenario("openLong", transition: true) {
            self.openProbe()
        } reset: {
            self.backToList()
        }
    }

    /// The screens the measurement drives, at its scale, kept in the result bundle: the
    /// Chats list, a new conversation with the composer focused, the 200 turn conversation,
    /// Today and Vault. Opt in like the measurement.
    func captureScreens() {
        func shot(_ name: String) {
            settle()
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = name
            attachment.lifetime = .keepAlways
            add(attachment)
        }
        shot("1-chats-list")
        app.navigationBars.buttons["New conversation"].tap()
        let field = app.textViews[composerID]
        XCTAssertTrue(field.waitForExistence(timeout: 30))
        field.tap()
        XCTAssertTrue(waitFor(field, "hasKeyboardFocus == true"), "the composer has the keyboard")
        shot("2-new-chat-keyboard")
        backToList()
        openProbe()
        shot("3-long-chat")
        backToList()
        app.tabBars.buttons["Today"].tap()
        shot("4-today")
        app.tabBars.buttons["Vault"].tap()
        shot("5-vault")
        app.tabBars.buttons["Chats"].tap()
    }

    // MARK: - Driving

    /// Five measured iterations of `body`. `prepare` runs before each one and `reset` after,
    /// both outside the measured window.
    private func measureScenario(_ name: String, transition: Bool,
                                 prepare: (() -> Void)? = nil,
                                 _ body: @escaping () -> Void,
                                 reset: (() -> Void)? = nil) {
        var metrics: [XCTMetric] = [XCTClockMetric()]
        if transition { metrics.append(XCTOSSignpostMetric.navigationTransitionMetric) }
        let options = XCTMeasureOptions()
        options.iterationCount = 5
        options.invocationOptions = [.manuallyStart, .manuallyStop]
        var iteration = 0
        measure(metrics: metrics, options: options) {
            prepare?()
            settle()
            iteration += 1
            let label = "pf1-\(Self.scale)-\(name)-\(iteration)"
            mark("\(label)-begin")
            let started = Date()
            startMeasuring()
            body()
            stopMeasuring()
            let ms = Date().timeIntervalSince(started) * 1000
            mark("\(label)-end")
            print(String(format: "PF1 scale=%d scenario=%@ iteration=%d ms=%.1f",
                         Self.scale, name, iteration, ms))
            reset?()
            settle()
        }
    }

    private func openProbe() {
        let row = app.staticTexts[Self.probeTitle]
        XCTAssertTrue(row.waitForExistence(timeout: 30), "the 200 turn conversation's row")
        row.tap()
        XCTAssertTrue(app.textViews[composerID].waitForExistence(timeout: 30),
                      "the 200 turn conversation opened")
    }

    private func backToList() {
        let back = app.navigationBars.buttons["Jesse"]
        XCTAssertTrue(back.waitForExistence(timeout: 30), "the back button")
        back.tap()
        XCTAssertTrue(app.navigationBars.buttons["New conversation"].waitForExistence(timeout: 30),
                      "back on the list")
    }

    private func waitFor(_ element: XCUIElement, _ predicate: String) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: predicate),
                                                    object: element)
        return XCTWaiter().wait(for: [expectation], timeout: 30) == .completed
    }

    private func mark(_ label: String) {
        log.notice("mark \(label, privacy: .public)")
    }

    private func settle() {
        RunLoop.current.run(until: Date().addingTimeInterval(1.5))
    }
}

final class NavigationCost1000UITests: NavigationCostUITestCase {
    override class var scale: Int { 1_000 }
    func testANewChat() { measureNewChat() }
    func testBBackOut() { measureBackOut() }
    func testCTabCycle() { measureTabCycle() }
    func testDOpenLong() { measureOpenLong() }
    func testEScreens() { captureScreens() }
}

final class NavigationCost5UITests: NavigationCostUITestCase {
    override class var scale: Int { 5 }
    func testANewChat() { measureNewChat() }
    func testBBackOut() { measureBackOut() }
    func testCTabCycle() { measureTabCycle() }
    func testDOpenLong() { measureOpenLong() }
}

/// A loopback bridge serving the seeded store's own conversation list, the day file, and a
/// 200 for everything else, in the test runner. BSD sockets, for the reason
/// `ConversationsStub` gives (a Network.framework listener meets the local network gate).
///
/// The list mirrors `ScaleSeedUITestSeam`'s rules field for field (ids, sessions, titles,
/// flags and their clocks, reply and read stamps), so the sync pass it triggers adopts,
/// updates and deletes nothing: it is the cost of a pull alone. `last_modified` is 0, which
/// never moves a local stamp. It never answers 304, so EVERY pull runs the full pass: the
/// shape of a pull after a reply has landed somewhere.
final class ScaleStub: @unchecked Sendable {

    /// The store `UnreadBadgeRenderUITests`' bound is asserted on, and the bound: list body
    /// evaluations allowed per Chats, Today, Chats round trip. Measured at 1,000: 8 per round
    /// trip before the fix (24 for three), 2 after. Three leaves room for a busy simulator
    /// and still fails the regression by a wide margin.
    static let renderBoundScale = 1_000
    static let renderBoundPerRoundTrip = 3

    let port: UInt16

    private let lock = NSLock()
    private var listenFD: Int32 = -1
    private var stopped = false
    private var paths: [String] = []
    private let conversations: String
    private let today: String

    init(count: Int) throws {
        conversations = Self.list(count: count)
        today = Self.todayFixture()
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw Self.err("socket() failed: \(errno)") }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { close(fd); throw Self.err("bind() failed: \(errno)") }
        guard listen(fd, 32) == 0 else { close(fd); throw Self.err("listen() failed: \(errno)") }
        var actual = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &actual) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) }
        }
        guard named == 0 else { close(fd); throw Self.err("getsockname() failed: \(errno)") }
        port = UInt16(bigEndian: actual.sin_port)
        listenFD = fd
        Thread.detachNewThread { [weak self] in self?.acceptLoop(fd) }
    }

    var requestSummary: String {
        lock.lock(); defer { lock.unlock() }
        let lists = paths.filter { $0.contains("/jesse/conversations") }.count
        return "\(paths.count) requests, \(lists) of them for the list"
    }

    func stop() {
        lock.lock()
        stopped = true
        let fd = listenFD
        listenFD = -1
        lock.unlock()
        if fd >= 0 { close(fd) }
    }

    // MARK: - Payloads (the seam's rules, duplicated: this process cannot import the app)

    private static let topics = [
        "kiln controller wiring", "Q3 hiring plan", "marathon taper", "fiber rebrand copy",
        "vault index design", "Italian residency forms", "pizza oven hydration",
        "bridge deploy runbook", "strand queue cleanup", "vendor contract review",
        "garden irrigation", "search expansion terms", "watch relay retries",
        "board meeting prep", "BBQ brisket timing", "client SOW draft",
    ]

    private static func list(count: Int) -> String {
        var rows: [String] = []
        let clock = 1_700_000_000_000
        for i in 0..<count {
            let title = i == 0 ? NavigationCostUITestCase.probeTitle
                               : "Seeded conversation \(i): \(topics[i % topics.count])"
            let favorite = i % 25 == 5
            let archived = i % 10 == 3
            let unread = i % 15 == 1
            let replied = clock + i
            let read = unread ? 0 : replied
            rows.append("""
            {"conversation_id":"\(String(format: "%08x-0000-4000-8000-777777777777", i))",\
            "session_id":"pf1-sess-\(i)","session_ids":["pf1-sess-\(i)"],"last_modified":0,\
            "first_message":"\(title)","title":"\(title)",\
            "favorite":\(favorite),"favorite_updated_ms":\(favorite ? clock : 0),\
            "archived":\(archived),"archived_updated_ms":\(archived ? clock : 0),\
            "last_reply_ms":\(replied),"read_through_ms":\(read),\
            "read_updated_ms":\(unread ? 0 : clock),"registered_ms":0}
            """)
        }
        return "{\"conversations\":[\(rows.joined(separator: ","))],\"deleted\":[]}"
    }

    /// JesseKit's own day file fixture, read from the source tree (a simulator process can
    /// read the host's files), with its date moved to today so the screen shows it as the
    /// current day.
    private static func todayFixture() -> String {
        let file = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("JesseKit/Tests/JesseNetworkingTests/Fixtures/today-full.json")
        let raw = (try? String(contentsOf: file, encoding: .utf8)) ?? "{}"
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        return raw.replacingOccurrences(of: "\"2026-03-03\"", with: "\"\(formatter.string(from: Date()))\"")
    }

    // MARK: - The socket

    private static func err(_ message: String) -> NSError {
        NSError(domain: "ScaleStub", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
    }

    private func acceptLoop(_ fd: Int32) {
        while true {
            let conn = accept(fd, nil, nil)
            lock.lock(); let done = stopped; lock.unlock()
            if done { if conn >= 0 { close(conn) }; return }
            guard conn >= 0 else { return }
            serve(conn)
            close(conn)
        }
    }

    private func serve(_ conn: Int32) {
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        let n = recv(conn, &buf, buf.count, 0)
        let request = n > 0 ? String(decoding: buf[0..<n], as: UTF8.self) : ""
        let line = request.split(separator: "\r\n").first.map(String.init) ?? "?"
        lock.lock(); paths.append(line); lock.unlock()

        let responseBody: String
        if line.contains("/jesse/conversations") {
            responseBody = conversations
        } else if line.contains("/jesse/today") {
            responseBody = today
        } else if line.contains("/jesse/models") {
            responseBody = #"{"active":"opus","models":[]}"#
        } else {
            responseBody = #"{"ok":true,"version":"0.187.0"}"#
        }
        let bytes = Array(responseBody.utf8)
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
            + "Content-Length: \(bytes.count)\r\nConnection: close\r\n\r\n"
        var out = Array(head.utf8)
        out.append(contentsOf: bytes)
        var sent = 0
        while sent < out.count {
            let wrote = out.withUnsafeBytes {
                send(conn, $0.baseAddress!.advanced(by: sent), out.count - sent, 0)
            }
            if wrote <= 0 { return }
            sent += wrote
        }
    }
}
