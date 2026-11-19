import XCTest
@testable import JesseNetworking

// The two timed-reminder routes, driven through the real `JesseBridgeClient` over a
// `URLProtocol` stub: what goes on the wire (method, path, bearer, body) and what each
// answer becomes. The split that matters is the contract's — `404` is FINAL, while a
// transport failure or a `5xx` THROWS so the caller queues the press and retries it.

/// Answers one scripted reply and remembers the request, body included (a `URLProtocol`
/// sees the body only as a stream).
final class ReminderStubURLProtocol: URLProtocol {
    struct Reply { var status: Int; var body: String }
    nonisolated(unsafe) static var reply: Reply?
    nonisolated(unsafe) static var seen: [(request: URLRequest, body: Data?)] = []

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        Self.seen.append((request, Self.readBody(request)))
        guard let url = request.url, let reply = Self.reply else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost)); return
        }
        let http = HTTPURLResponse(url: url, statusCode: reply.status, httpVersion: "HTTP/1.1",
                                   headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: http, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(reply.body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    private static func readBody(_ request: URLRequest) -> Data? {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return nil }
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 1024)
        while stream.hasBytesAvailable {
            let n = stream.read(&buffer, maxLength: buffer.count)
            guard n > 0 else { break }
            data.append(buffer, count: n)
        }
        return data
    }
}

final class ReminderActionsWireTests: XCTestCase {

    private let reminderJSON = #"{"reminder":{"id":"r7k2m9","text":"start the pancakes"}}"#

    override func setUp() {
        super.setUp()
        ReminderStubURLProtocol.reply = nil
        ReminderStubURLProtocol.seen = []
    }

    private func client() -> JesseBridgeClient {
        let c = URLSessionConfiguration.ephemeral
        c.protocolClasses = [ReminderStubURLProtocol.self]
        return JesseBridgeClient(config: JesseConfig(host: "laptop", port: 8765, token: "tok"),
                                 session: URLSession(configuration: c))
    }

    func testDonePostsToTheDoneRouteWithTheBearerAndNoBody() async throws {
        ReminderStubURLProtocol.reply = .init(status: 200, body: reminderJSON)

        let result = try await client().reminderDone(id: "r7k2m9")

        XCTAssertEqual(result, .applied)
        let sent = try XCTUnwrap(ReminderStubURLProtocol.seen.first)
        XCTAssertEqual(sent.request.httpMethod, "POST")
        XCTAssertEqual(sent.request.url?.path, "/jesse/reminders/r7k2m9/done")
        XCTAssertEqual(sent.request.value(forHTTPHeaderField: "Authorization"), "Bearer tok")
        XCTAssertTrue(sent.body?.isEmpty ?? true, "Done sends no body")
        XCTAssertEqual(sent.request.timeoutInterval, ReminderActionResult.requestTimeout)
    }

    func testSnoozePostsItsMinutes() async throws {
        ReminderStubURLProtocol.reply = .init(status: 200, body: reminderJSON)

        let result = try await client().reminderSnooze(id: "r7k2m9", minutes: 10)

        XCTAssertEqual(result, .applied)
        let sent = try XCTUnwrap(ReminderStubURLProtocol.seen.first)
        XCTAssertEqual(sent.request.httpMethod, "POST")
        XCTAssertEqual(sent.request.url?.path, "/jesse/reminders/r7k2m9/snooze")
        XCTAssertEqual(sent.request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = try XCTUnwrap(sent.body)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(obj["minutes"] as? Int, 10)
        XCTAssertEqual(obj.count, 1)
    }

    /// An id is a path segment, never a path: anything outside the alphanumerics is escaped
    /// (by the same `pathEscaped` the Studio run routes use), so a hostile id cannot climb
    /// out of `/jesse/reminders/` — the bridge sees one segment it has no reminder for.
    func testTheIdIsPathEscaped() async throws {
        ReminderStubURLProtocol.reply = .init(status: 200, body: reminderJSON)
        _ = try await client().reminderDone(id: "a/../b")
        let url = try XCTUnwrap(ReminderStubURLProtocol.seen.first?.request.url)
        XCTAssertEqual(url.pathComponents.count, 5, url.absoluteString)
        XCTAssertEqual(Array(url.pathComponents.prefix(3)), ["/", "jesse", "reminders"])
        XCTAssertEqual(url.pathComponents.last, "done")
        XCTAssertFalse(url.pathComponents.contains(".."))
    }

    func testA404IsAnUnknownReminderNotAThrow() async throws {
        ReminderStubURLProtocol.reply = .init(status: 404, body: #"{"error":"no such reminder"}"#)
        let done = try await client().reminderDone(id: "gone")
        XCTAssertEqual(done, .unknownReminder)
        let snooze = try await client().reminderSnooze(id: "gone", minutes: 10)
        XCTAssertEqual(snooze, .unknownReminder)
    }

    func testA5xxThrowsSoTheCallerQueues() async {
        ReminderStubURLProtocol.reply = .init(status: 503, body: "busy")
        do {
            _ = try await client().reminderDone(id: "r1")
            XCTFail("a 5xx must throw")
        } catch JesseError.badResponse(let status, let body) {
            XCTAssertEqual(status, 503)
            XCTAssertEqual(body, "busy")
        } catch {
            XCTFail("unexpected \(error)")
        }
    }

    func testAnUnreachableBridgeThrows() async {
        ReminderStubURLProtocol.reply = nil
        do {
            _ = try await client().reminderSnooze(id: "r1", minutes: 10)
            XCTFail("a transport failure must throw")
        } catch {
            XCTAssertTrue(error is JesseError)
        }
    }

    /// The status table, without a server.
    func testClassification() throws {
        XCTAssertEqual(try ReminderActionResult.classify(status: 200, body: ""), .applied)
        XCTAssertEqual(try ReminderActionResult.classify(status: 204, body: ""), .applied)
        XCTAssertEqual(try ReminderActionResult.classify(status: 404, body: ""), .unknownReminder)
        XCTAssertEqual(try ReminderActionResult.classify(status: 400, body: "bad"),
                       .refused(status: 400, message: "bad"))
        // The pairing, a timeout and a rate limit are worth asking again.
        for status in [401, 403, 408, 429, 500, 502, 503] {
            XCTAssertThrowsError(try ReminderActionResult.classify(status: status, body: ""),
                                 "\(status) must throw")
        }
    }
}
