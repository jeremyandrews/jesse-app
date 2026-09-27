import XCTest
import SwiftData
import JesseCore
@testable import JesseNetworking

/// The wire half of folding narration and untyped prompts: what a newer bridge sends, what an
/// older one sends, and what the app makes of each. Nothing may disappear either way.
final class NarrationWireTests: XCTestCase {

    // MARK: - The live split

    /// A tool call closes what streamed before it as narration; the answer is what streamed
    /// since the last one. The same cut the bridge makes for the finished reply.
    func testAToolCallMovesWhatStreamedIntoTheNarration() {
        var live = LiveReply()
        for e: JesseStreamEvent in [.delta("Let me "), .delta("look."),
                                    .activity(ToolActivity(name: "Read", refused: false)),
                                    .delta("Checking the footer."),
                                    .activity(ToolActivity(name: "Grep", refused: false)),
                                    .delta("Found "), .delta("it.")] {
            live.apply(e)
        }
        XCTAssertEqual(live.thinking, "Let me look.\n\nChecking the footer.")
        XCTAssertEqual(live.answer, "Found it.")
    }

    func testNoToolCallMeansNoNarration() {
        var live = LiveReply()
        live.apply(.delta("Just the answer."))
        XCTAssertEqual(live.thinking, "")
        XCTAssertEqual(live.answer, "Just the answer.")
    }

    /// A late subscriber's reset: split when the bridge names the cut, whole otherwise (an
    /// older bridge, or nothing narrated yet).
    func testResetsReplaceBothHalves() {
        var live = LiveReply()
        live.apply(.delta("stale"))
        live.apply(.resetSplit(narration: "Let me look.", answer: "Fou"))
        XCTAssertEqual(live.thinking, "Let me look.")
        XCTAssertEqual(live.answer, "Fou")
        live.apply(.reset("Whole text"))
        XCTAssertEqual(live.thinking, "")
        XCTAssertEqual(live.answer, "Whole text")
    }

    func testACommentaryBlockIsNarrationAndNeverTheAnswer() {
        var live = LiveReply()
        live.apply(.narration("I'll look that up."))
        live.apply(.delta("42"))
        XCTAssertEqual(live.thinking, "I'll look that up.")
        XCTAssertEqual(live.answer, "42")
    }

    // MARK: - Frames

    func testTheNewFramesDecode() {
        let lines = [
            "event: reset", #"data: {"text":"Let me look.Fou","narration":"Let me look.","answer":"Fou"}"#, "",
            "event: reset", #"data: {"text":"Fou"}"#, "",
            "event: narration", #"data: {"text":"I'll look."}"#, "",
            "event: done", #"data: {"response":"Found it.","narration":"Let me look."}"#, "",
        ]
        let frames = SSEParser.framesFromLines(lines)
        XCTAssertEqual(frames.count, 4)
        XCTAssertEqual(frames[0], .resetSplit(narration: "Let me look.", answer: "Fou"))
        XCTAssertEqual(frames[1], .reset("Fou"))
        XCTAssertEqual(frames[2], .narration("I'll look."))
        guard case .done(let reply) = frames[3] else { return XCTFail("\(frames[3])") }
        XCTAssertEqual(reply.text, "Found it.")
        XCTAssertEqual(reply.storedNarration, "Let me look.")
    }

    /// An older bridge's `done` has no narration: the reply is the whole answer, as always.
    func testAnOlderBridgesDoneFrameIsTheWholeReply() {
        let frames = SSEParser.framesFromLines([
            "event: done", #"data: {"response":"Found it.","session_id":"s"}"#, "",
        ])
        guard case .done(let reply) = frames.first else { return XCTFail("\(frames)") }
        XCTAssertEqual(reply.text, "Found it.")
        XCTAssertNil(reply.narration)
        XCTAssertNil(reply.storedNarration)
    }

    func testThePollResultCarriesTheNarration() throws {
        let body = #"{"status":"done","response":"Found it.","narration":"Let me look."}"#
        let decoded = try JSONDecoder().decode(JesseResultResponse.self, from: Data(body.utf8))
        XCTAssertEqual(decoded.narration, "Let me look.")
        let old = #"{"status":"done","response":"Found it."}"#
        XCTAssertNil(try JSONDecoder().decode(JesseResultResponse.self, from: Data(old.utf8)).narration)
    }

    func testAHydratedTurnSaysWhetherItIsNarration() throws {
        let body = #"[{"role":"assistant","text":"Starting now.","turn_key":"s:1","narration":true},{"role":"assistant","text":"Done.","turn_key":"s:2"}]"#
        let turns = try JSONDecoder().decode([HydratedTurn].self, from: Data(body.utf8))
        XCTAssertEqual(turns.map(\.narration), [true, false])
    }

    @MainActor
    func testAHydratedNarrationTurnIsStoredAsNarration() {
        let narr = TranscriptMerge.newTurn(from: HydratedTurn(role: "assistant", text: "Starting now.",
                                                              timestamp: nil, turnKey: "s:1",
                                                              narration: true))
        XCTAssertTrue(narr.isNarration)
        XCTAssertEqual(narr.sourceKey, "s:1")
        // A user line is never narration, whatever the wire says.
        let user = TranscriptMerge.newTurn(from: HydratedTurn(role: "user", text: "hi", timestamp: nil,
                                                              turnKey: "s:0", narration: true))
        XCTAssertFalse(user.isNarration)
    }

    // MARK: - Sent for

    func testTheConversationListCarriesSentFor() throws {
        let body = #"{"conversation_id":"c","sent_for":"Scheduled: archive box"}"#
        let row = try JSONDecoder().decode(ConversationSummary.self, from: Data(body.utf8))
        XCTAssertEqual(row.sentFor, "Scheduled: archive box")
        let typed = try JSONDecoder().decode(ConversationSummary.self,
                                             from: Data(#"{"conversation_id":"c"}"#.utf8))
        XCTAssertNil(typed.sentFor)
    }

    @MainActor
    func testSyncAdoptsSentForAndNeverClearsIt() {
        let thread = JesseThread(mode: .tell)
        XCTAssertTrue(thread.adoptSentFor(from: ConversationSummary(conversationId: "c",
                                                                    sentFor: "Morning routine")))
        XCTAssertEqual(thread.sentFor, "Morning routine")
        XCTAssertFalse(thread.adoptSentFor(from: ConversationSummary(conversationId: "c")),
                       "an older bridge omits the key; that is not an un-send")
        XCTAssertEqual(thread.sentFor, "Morning routine")
    }

    func testTheRequestSendsSentForOnlyWhenSet() throws {
        func body(_ sentFor: String?) throws -> String {
            let req = JesseBridgeClient.makeRequest(mode: .tell, text: "t", sessionId: nil,
                                                    conversationId: "c", voice: false,
                                                    instructions: nil, floorOverride: nil,
                                                    attachments: [], sentFor: sentFor)
            return String(decoding: try JesseBridgeClient.encodeBody(req), as: UTF8.self)
        }
        XCTAssertTrue(try body("Morning routine").contains(#""sent_for":"Morning routine""#))
        XCTAssertFalse(try body(nil).contains("sent_for"), "a typed turn omits the key")
        XCTAssertFalse(try body("  ").contains("sent_for"), "blank is typed")
    }
}
