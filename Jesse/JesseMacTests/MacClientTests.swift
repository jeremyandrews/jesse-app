import XCTest
@testable import Jesse_Mac

// Mac-app-specific unit coverage: the reply parser as the Mac uses it, the pairing-link
// parser, and the notification snippet formatter. The SSE framing and host-sanitizing tests that used
// to live here moved into the JesseNetworking package (SSEParserTests, JesseConfigTests)
// when those duplicated implementations were unified into the shared client.

/// The reply parser as the Mac uses it: the shared `parseMarkdownBlocks`, whose fences follow
/// the CommonMark rule in `MarkdownFence`. The Mac used to have a parser of its own that
/// toggled a code block on any line starting with three backticks.
@MainActor
final class MacReplyParserTests: XCTestCase {
    func testAFenceWithALanguageTagIsOneCodeBlockWithoutItsFences() {
        let blocks = parseMarkdownBlocks("Before\n\n```swift\nlet x = 1\n```\n\nAfter")
        XCTAssertEqual(blocks, [.paragraph("Before"), .code("let x = 1"), .paragraph("After")])
    }

    func testATildeFenceKeepsABacktickLineAsCode() {
        let blocks = parseMarkdownBlocks("~~~\ncargo test\n```\n~~~\nAfter")
        XCTAssertEqual(blocks, [.code("cargo test\n```"), .paragraph("After")])
    }

    func testAFourBacktickFenceQuotesAThreeBacktickFence() {
        let blocks = parseMarkdownBlocks("````markdown\n```swift\nlet q = true\n```\n````\nAfter")
        XCTAssertEqual(blocks, [.code("```swift\nlet q = true\n```"), .paragraph("After")])
    }

    func testAnInfoStringLineInsideABlockIsContentNotAClose() {
        let blocks = parseMarkdownBlocks("```\none\n```swift\ntwo\n```")
        XCTAssertEqual(blocks, [.code("one\n```swift\ntwo")])
    }

    func testAnUnclosedFenceRunsToTheEndAndLosesNothing() {
        let blocks = parseMarkdownBlocks("Intro\n\n```\ntail -f log\n\n# not a heading")
        XCTAssertEqual(blocks, [.paragraph("Intro"), .code("tail -f log\n\n# not a heading")])
    }

    func testTheFixtureParsesEveryFenceShapeItCarries() throws {
        let blocks = parseMarkdownBlocks(try MacReplyFixture.text())
        let code = blocks.compactMap { block -> String? in
            if case let .code(text) = block { return text }
            return nil
        }
        XCTAssertTrue(code.contains("let blocks = parseMarkdownBlocks(reply)\nprint(blocks.count)"))
        XCTAssertTrue(code.contains("cargo test --workspace\n```"))
        XCTAssertTrue(code.contains("```swift\nlet quoted = true\n```"))
        // Unclosed, it runs to the end of the reply, the file's final newline included.
        XCTAssertEqual(code.last, "tail -f ~/Library/Logs/jesse-bridge.log\n")
        XCTAssertTrue(blocks.contains { if case .table = $0 { return true } else { return false } })
    }
}

final class MacPairLinkTests: XCTestCase {
    func testValidLink() {
        let parsed = MacPairLink.parse("jesse://pair?url=box.ts.net:8765&token=secret")
        XCTAssertEqual(parsed?.host, "box.ts.net")
        XCTAssertEqual(parsed?.port, 8765)
        XCTAssertEqual(parsed?.token, "secret")
    }

    func testRejectsWrongSchemeOrMissingToken() {
        XCTAssertNil(MacPairLink.parse("https://pair?url=box&token=t"))
        XCTAssertNil(MacPairLink.parse("jesse://pair?url=box"))
        XCTAssertNil(MacPairLink.parse("not a url"))
    }
}

final class MacNotifierSnippetTests: XCTestCase {
    func testCollapsesAndTruncates() {
        XCTAssertEqual(MacNotifier.snippet("line one\nline two"), "line one line two")
        let long = String(repeating: "a", count: 200)
        XCTAssertEqual(MacNotifier.snippet(long, limit: 10).count, 11)  // 10 chars + ellipsis
    }
}
