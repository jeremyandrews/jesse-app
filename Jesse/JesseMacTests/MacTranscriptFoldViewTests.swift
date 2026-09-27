import XCTest
import SwiftUI
import JesseCore
@testable import Jesse_Mac

/// The two transcript folds as the Mac draws them: a collapsed fold is one compact row, and
/// opening it shows the full text. Mirrors the phone's `TranscriptFoldViewTests`.
@MainActor
final class MacTranscriptFoldViewTests: XCTestCase {

    private func height<V: View>(_ view: V) -> CGFloat {
        let host = NSHostingController(rootView: view.frame(width: 560))
        return host.sizeThatFits(in: CGSize(width: 560, height: 10_000)).height
    }

    private let longPrompt = String(repeating: "Process the archive box and report what moved. ", count: 12)

    func testAnUntypedPromptRendersCollapsedAndExpandsToTheFullText() {
        let turn = Turn(role: .user, text: longPrompt)
        let collapsed = height(MacPromptFold(turn: turn, hint: "Scheduled: archive box",
                                             isExpanded: .constant(false)))
        let expanded = height(MacPromptFold(turn: turn, hint: "Scheduled: archive box",
                                            isExpanded: .constant(true)))
        XCTAssertLessThan(collapsed, 50)
        XCTAssertGreaterThan(expanded, collapsed * 2)
    }

    func testTheThinkingRowRendersCollapsedAndExpands() {
        let narration = String(repeating: "Checking the footer format first. ", count: 4)
        let collapsed = height(MacThinkingFold(text: narration, isLive: false,
                                               isExpanded: .constant(false)))
        let expanded = height(MacThinkingFold(text: narration, isLive: false,
                                              isExpanded: .constant(true)))
        XCTAssertLessThan(collapsed, 30)
        XCTAssertGreaterThan(expanded, collapsed)
    }
}
