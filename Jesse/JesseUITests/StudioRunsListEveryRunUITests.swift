import Foundation
import XCTest

/// The Studio runs list shows EVERY run the bridge lists, and a run's report opens from it.
///
/// Skipped unless the run asks for it: pass `TEST_RUNNER_PD1_BRIDGE=host,port,token` and
/// `TEST_RUNNER_PD1_RUNS=<id>,<id>,…` (every run id `GET /jesse/runs` answers) on the
/// `xcodebuild test` command line, so the gate never runs it. Point it at a scratch bridge
/// serving a COPY of a run store, never at the live one: it presses nothing that posts.
///
/// - `PD1_REPORT_RUN`: a run with a report, opened at the end.
/// - `PD1_CLOUD_RUNS`: the cloud run ids among `PD1_RUNS` (bridge 0.216.0); each must show its
///   `Cloud` label.
/// - `PD1_SHOTS_DIR`: where the PNGs are written (they are attached to the result too).
///
/// `testCloudRowsShowInTheirOwnColourAndOfferNothing` needs no bridge: a loopback stub in the
/// test runner serves the runs as they stood on 2026-11-23 at 18:20 CET, Studio and cloud, and
/// it runs with the rest of the suite. `TEST_RUNNER_DJ7_SHOTS_DIR` writes its three PNGs.
final class StudioRunsListEveryRunUITests: XCTestCase {

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
    }

    private func shot(_ name: String, dir: String? = nil) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = dir ?? env["PD1_SHOTS_DIR"] {
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    private func row(_ app: XCUIApplication, _ runID: String) -> XCUIElement {
        app.descendants(matching: .any)
            // A row reads `<title>, <run id>, …` once the bridge serves titles, `<run id>, …` before.
            .matching(NSPredicate(format: "label BEGINSWITH %@ OR label CONTAINS %@",
                                  "\(runID),", ", \(runID),")).firstMatch
    }

    func testEveryRunIsListedAndAReportOpens() throws {
        guard env["PD1_BRIDGE"] != nil else {
            throw XCTSkip("taken only with PD1_BRIDGE set")
        }
        let ids = try XCTUnwrap(env["PD1_RUNS"]).split(separator: ",").map(String.init)
        let cloudIDs = Set((env["PD1_CLOUD_RUNS"] ?? "").split(separator: ",").map(String.init))
        let reportRun = try XCTUnwrap(env["PD1_REPORT_RUN"])

        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = env["PD1_BRIDGE"]
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands"]
        app.launch()

        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        let runsButton = app.navigationBars.buttons["Runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")
        runsButton.tap()
        XCTAssertTrue(row(app, ids[0]).waitForExistence(timeout: 30), "the first row")
        sleep(1)

        // Walk the list top to bottom: every id must turn up as a row.
        var seen = Set<String>()
        var page = 1
        shot("pd1-ios-runs-list-\(page)")
        for _ in 0..<12 {
            for id in ids where !seen.contains(id) && row(app, id).exists {
                // A cloud run's row says so; a Studio run's does not.
                let label = row(app, id).label
                XCTAssertEqual(label.contains(", Cloud"), cloudIDs.contains(id),
                               "\(id)'s row: \(label)")
                seen.insert(id)
            }
            if seen.count == ids.count { break }
            app.swipeUp(velocity: .slow)
            sleep(1)
            page += 1
            shot("pd1-ios-runs-list-\(page)")
        }
        let missing = ids.filter { !seen.contains($0) }
        XCTAssertTrue(missing.isEmpty, "rows missing from the Runs list: \(missing)")

        // One run's report, opened from its detail.
        var tries = 0
        while !(row(app, reportRun).exists && row(app, reportRun).isHittable) && tries < 12 {
            app.swipeDown(velocity: .slow)
            tries += 1
        }
        row(app, reportRun).tap()
        // The detail's own `Open report` button (the path itself is under Details).
        let report = app.buttons["Open report"].firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 30), "the Report link of \(reportRun)")
        sleep(1)
        shot("pd1-ios-run-detail")
        report.tap()
        sleep(4)
        shot("pd1-ios-report-open")
    }

    // MARK: - Cloud rows (bridge 0.216.0)

    /// The Runs page as it stood on 2026-11-23 at 18:20 CET: four Studio runs running, four
    /// queued, and the cloud's two running and one finished, in one list. Each cloud row says
    /// `Cloud`; a cloud run's detail is titled so and offers no Cancel, reply or Archive. With
    /// `DJ7_SHOTS_DIR` set the page and the detail are written, named by `DJ7_APPEARANCE`.
    func testCloudRowsShowInTheirOwnColourAndOfferNothing() throws {
        let stub = try RunsSnapshotBridge()
        defer { stub.stop() }
        let dir = env["DJ7_SHOTS_DIR"]
        // The simulator's own appearance (`xcrun simctl ui <device> appearance dark`) names
        // the shots: `XCUIDevice.appearance` does not reach a presented sheet.
        let look = env["DJ7_APPEARANCE"] ?? "light"

        let app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_BRIDGE"] = "127.0.0.1,\(stub.port),stub-token"
        app.launchEnvironment["JESSE_MUTE"] = "1"
        app.launchArguments += ["-\(TodayViewPreferencesKey.segment)", "strands",
                                "-studioRunsFilter", "all"]
        app.launch()

        let today = app.tabBars.buttons["Today"]
        XCTAssertTrue(today.waitForExistence(timeout: 30))
        today.tap()
        let runsButton = app.navigationBars.buttons["Runs"]
        XCTAssertTrue(runsButton.waitForExistence(timeout: 30), "the Runs toolbar button")
        runsButton.tap()

        let bg = row(app, "BG4-2")
        XCTAssertTrue(bg.waitForExistence(timeout: 30), "the cloud row BG4-2; served: \(stub.requestPaths)")
        XCTAssertTrue(bg.label.contains("Cloud"), "BG4-2's row: \(bg.label)")
        let studioRow = row(app, "B160")
        XCTAssertTrue(studioRow.exists)
        XCTAssertFalse(studioRow.label.contains("Cloud"), "B160's row: \(studioRow.label)")
        settled(bg, "BG4-2's row")
        shot("dj7-runs-\(look)", dir: dir)

        let dj6 = row(app, "DJ6")
        XCTAssertTrue(dj6.waitForExistence(timeout: 10))
        XCTAssertTrue(dj6.label.contains("Cloud"), "DJ6's row: \(dj6.label)")
        dj6.tap()
        XCTAssertTrue(app.navigationBars["Cloud run"].waitForExistence(timeout: 15),
                      "the cloud run's detail")
        XCTAssertTrue(app.staticTexts["Running cargo test -p jesse-bridge devjob"]
            .waitForExistence(timeout: 15), "its log, asked for with host=cloud")
        XCTAssertFalse(app.buttons["Cancel run"].exists)
        XCTAssertFalse(app.buttons["studio-run-archive"].exists)
        XCTAssertFalse(app.textFields["Reply"].exists)
        XCTAssertTrue(stub.requestPaths.contains { $0.contains("/jesse/runs/DJ6?host=cloud") },
                      "served: \(stub.requestPaths)")
        settled(app.navigationBars["Cloud run"], "the cloud run's detail")
        shot("dj7-cloud-run-detail-\(look)", dir: dir)
    }

    /// Waits until `element` can be tapped, which XCUITest reports only once the screen has
    /// stopped animating, so a screenshot does not catch a push or a list mid transition.
    private func settled(_ element: XCUIElement, _ what: String) {
        let hittable = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isHittable == true"),
                                                 object: element)
        XCTAssertEqual(XCTWaiter().wait(for: [hittable], timeout: 10), .completed, "\(what) settles")
    }
}

/// A loopback bridge in the test runner serving the Runs page as it stood on 2026-11-23 at
/// 18:20 CET (`GET /jesse/runs`), DJ6's detail and log with `host=cloud`, and an empty answer
/// to everything else. BSD sockets, as `StubBridge`: a Network.framework listener waits on the
/// local network permission no one can grant in a UI test.
final class RunsSnapshotBridge: @unchecked Sendable {

    let port: UInt16
    private let lock = NSLock()
    private var paths: [String] = []
    private var listenFD: Int32 = -1
    private var stopped = false

    var requestPaths: [String] {
        lock.lock(); defer { lock.unlock() }
        return paths
    }

    private static func studio(_ id: String, _ status: String, strand: String, repo: String,
                               branch: String, created: String, started: String? = nil,
                               waiting: String? = nil) -> String {
        """
        {"run_id":"\(id)","host":"studio","strand":"\(strand)","item_id":"\(id)",
         "repo":"\(repo)","branch":"\(branch)","origin":"app","status":"\(status)",
         "created_at":"\(created)","queued_at":"\(created)",
         "started_at":\(started.map { "\"\($0)\"" } ?? "null"),
         "waiting_for":\(waiting.map { "\"\($0)\"" } ?? "null"),
         "rounds":[],"report_paths":[],"title":null,"last_line":null,"report_line":null}
        """
    }

    private static func cloud(_ id: String, _ status: String, repo: String, slug: String,
                              title: String?, created: String, started: String?,
                              finished: String?, outcome: String?, report: String?) -> String {
        """
        {"run_id":"\(id)","host":"cloud","job_name":"devjob-\(id.lowercased())",
         "strand":null,"item_id":null,"repo":"\(repo)","branch":"jesse/\(slug)","slug":"\(slug)",
         "origin":"turn","agent":"claude","model":null,"status":"\(status)",
         "created_at":"\(created)","queued_at":"\(created)",
         "started_at":\(started.map { "\"\($0)\"" } ?? "null"),
         "finished_at":\(finished.map { "\"\($0)\"" } ?? "null"),
         "outcome":\(outcome.map { "\"\($0)\"" } ?? "null"),"pull_request":null,
         "report_paths":\(report.map { "[\"\($0)\"]" } ?? "[]"),"error":null,
         "title":\(title.map { "\"\($0)\"" } ?? "null")}
        """
    }

    private static let dj6 = cloud(
        "DJ6", "running", repo: "jeremyandrews/jesse-app",
        slug: "dj6-longer-cloud-dev-jobs-devjob-cloud-max-t", title: nil,
        created: "2026-11-23T17:09:02.000Z", started: "2026-11-23T17:09:05.000Z", finished: nil,
        outcome: nil, report: nil)

    private static let listJSON: String = {
        let runs = [
            studio("B160", "running", strand: "trovato", repo: "jeremyandrews/trovato",
                   branch: "jesse/b160", created: "2026-11-23T16:51:40.000Z",
                   started: "2026-11-23T16:51:44.000Z"),
            studio("DJ3", "running", strand: "jesse-app", repo: "jeremyandrews/jesse-app",
                   branch: "jesse/dj3", created: "2026-11-23T16:58:12.000Z",
                   started: "2026-11-23T16:58:15.000Z"),
            studio("C2", "running", strand: "trovato-site", repo: "jeremyandrews/trovato-site",
                   branch: "jesse/c2", created: "2026-11-23T16:44:05.000Z",
                   started: "2026-11-23T16:44:09.000Z"),
            studio("GO348", "running", strand: "goose-blog", repo: "local/goose-blog-0-19",
                   branch: "jesse/go348", created: "2026-11-23T16:20:31.000Z",
                   started: "2026-11-23T16:20:35.000Z"),
            studio("A9b", "queued", strand: "jesse-app", repo: "jeremyandrews/jesse-app",
                   branch: "jesse/a9b", created: "2026-11-23T17:11:02.000Z", waiting: "slot"),
            studio("B199", "queued", strand: "trovato", repo: "jeremyandrews/trovato",
                   branch: "jesse/b199", created: "2026-11-23T17:13:47.000Z", waiting: "slot"),
            studio("U40", "queued", strand: "jesse-app", repo: "jeremyandrews/jesse-app",
                   branch: "jesse/u40", created: "2026-11-23T17:15:20.000Z", waiting: "slot"),
            studio("BG12", "queued", strand: "trovato", repo: "jeremyandrews/trovato",
                   branch: "jesse/bg12", created: "2026-11-23T17:18:09.000Z", waiting: "slot"),
            cloud("BG4-2", "running", repo: "jeremyandrews/trovato",
                  slug: "bg4-2-one-install-hosts-many-blogs-by-host-nam", title: nil,
                  created: "2026-11-23T17:08:22.000Z", started: "2026-11-23T17:08:27.000Z",
                  finished: nil, outcome: nil, report: nil),
            dj6,
            cloud("s-dj2-cloud-live-proof", "reported", repo: "jeremyandrews/k3s",
                  slug: "dj2-cloud-live-proof", title: nil,
                  created: "2026-11-23T16:41:55.000Z", started: "2026-11-23T16:42:01.000Z",
                  finished: "2026-11-23T17:08:10.000Z", outcome: "pull request opened",
                  report: "Inbox/2026-11-23-run-s-dj2-cloud-live-proof.md"),
        ]
        return """
        {"runs":[\(runs.joined(separator: ","))],
         "counts":{"running":4,"queued":4,"needs_you":0,"paused":0,"needs_me":0,"moving":8,
                   "finished":0,"archived":0,
                   "cloud":{"running":2,"queued":0,"needs_me":0,"moving":2,"finished":1}},
         "disk":{},"orphans":[],"cloud_available":true}
        """
    }()

    private static let dj6Detail: String = {
        String(dj6.dropLast()) + #","waiting_for":null,"last_line":"Running cargo test -p jesse-bridge devjob","elapsed_secs":4200}"#
    }()

    private static let dj6Log = """
    {"run_id":"DJ6","lines":["Cloning jeremyandrews/jesse-app into /work","Rebased onto origin/main (e3db8ad)",\
    "Reading bridge/src/devjob/cloud.rs","Editing [devjob.cloud] max_timeout_minutes",\
    "Running cargo fmt --check","Running cargo test -p jesse-bridge devjob"]}
    """

    init() throws {
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
        guard listen(fd, 16) == 0 else { close(fd); throw Self.err("listen() failed: \(errno)") }
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

    func stop() {
        lock.lock()
        stopped = true
        let fd = listenFD
        listenFD = -1
        lock.unlock()
        if fd >= 0 { close(fd) }
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
        var buf = [UInt8](repeating: 0, count: 16 * 1024)
        let n = recv(conn, &buf, buf.count, 0)
        let request = n > 0 ? String(decoding: buf[0..<n], as: UTF8.self) : ""
        let line = request.split(separator: "\r\n").first.map(String.init) ?? "?"
        lock.lock(); paths.append(line); lock.unlock()
        let path = line.split(separator: " ").dropFirst().first.map(String.init) ?? ""
        let body: String
        if path.hasPrefix("/jesse/runs/DJ6/log") {
            body = Self.dj6Log
        } else if path.hasPrefix("/jesse/runs/DJ6") {
            body = Self.dj6Detail
        } else if path == "/jesse/runs" || path.hasPrefix("/jesse/runs?") {
            body = Self.listJSON
        } else if path.hasPrefix("/jesse/work/last-look") {
            body = #"{"last_look":null}"#
        } else {
            body = "{}"
        }
        let bytes = Array(body.utf8)
        let head = Array("""
        HTTP/1.1 200 OK\r
        Content-Type: application/json\r
        Content-Length: \(bytes.count)\r
        Connection: close\r
        \r

        """.utf8)
        let out = head + bytes
        var sent = 0
        while sent < out.count {
            let k = out.withUnsafeBytes { send(conn, $0.baseAddress! + sent, out.count - sent, 0) }
            if k <= 0 { break }
            sent += k
        }
    }

    private static func err(_ message: String) -> NSError {
        NSError(domain: "RunsSnapshotBridge", code: 1,
                userInfo: [NSLocalizedDescriptionKey: message])
    }
}
