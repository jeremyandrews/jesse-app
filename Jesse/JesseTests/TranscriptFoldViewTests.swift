import XCTest
import SwiftUI
import SwiftData
import JesseCore
@testable import Jesse

/// The two transcript folds as the phone draws them. Sizes come from a hosting controller
/// (the expanded prompt is a UIKit text view, which `ImageRenderer` cannot draw): a collapsed
/// fold is one compact row, and opening it shows the full text.
@MainActor
final class TranscriptFoldViewTests: XCTestCase {

    private let width: CGFloat = 390

    private func height<V: View>(_ view: V) -> CGFloat {
        let host = UIHostingController(rootView: view)
        return host.sizeThatFits(in: CGSize(width: width, height: .greatestFiniteMagnitude)).height
    }

    private let longPrompt = String(repeating: "Process the archive box and report what moved. ", count: 12)

    func testAnUntypedPromptRendersCollapsedAndExpandsToTheFullText() {
        let turn = Turn(role: .user, text: longPrompt)
        let collapsed = height(PromptFoldView(turn: turn, hint: "Scheduled: archive box",
                                              isExpanded: .constant(false)))
        let expanded = height(PromptFoldView(turn: turn, hint: "Scheduled: archive box",
                                             isExpanded: .constant(true)))
        XCTAssertLessThan(collapsed, 60, "collapsed is one compact row, not the prompt")
        XCTAssertGreaterThan(expanded, collapsed * 3, "expanded shows the whole prompt")
    }

    func testTheThinkingRowRendersCollapsedAndExpands() {
        let narration = String(repeating: "Checking the footer format first. ", count: 4)
        let collapsed = height(ThinkingFold(text: narration, isLive: false,
                                            isExpanded: .constant(false)))
        let expanded = height(ThinkingFold(text: narration, isLive: false,
                                           isExpanded: .constant(true)))
        XCTAssertLessThan(collapsed, 40, "collapsed is a small metadata line")
        XCTAssertGreaterThan(expanded, collapsed)
        XCTAssertLessThanOrEqual(expanded, collapsed + ThinkingFold.expandedMaxHeight + 20,
                                 "a long narration scrolls inside its cap rather than pushing the answer away")
    }

    /// The transcript a scheduled run opens on: the prompt folds, the narration folds, and the
    /// answer is a row of its own; a typed message is an ordinary bubble.
    func testAScheduledRunResolvesToPromptThinkingAndAnswerRows() throws {
        let container = try ModelContainer(for: JesseThread.self, Turn.self,
                                           configurations: ModelConfiguration(isStoredInMemoryOnly: true))
        let context = ModelContext(container)
        let thread = JesseThread(mode: .tell)
        thread.sentFor = "Scheduled: archive box"
        context.insert(thread)
        let base = Date(timeIntervalSince1970: 3_000_000)
        let narr = Turn(role: .jesse, text: "Starting now.", createdAt: base.addingTimeInterval(1))
        narr.isNarration = true
        let turns = [Turn(role: .user, text: longPrompt, createdAt: base), narr,
                     Turn(role: .jesse, text: "Moved 3 notes.", createdAt: base.addingTimeInterval(2)),
                     Turn(role: .user, text: "thanks", createdAt: base.addingTimeInterval(3))]
        for t in turns { t.thread = thread; context.insert(t) }
        let items = TranscriptItem.items(thread.orderedTurns)
        XCTAssertEqual(items.count, 3)
        if case .prompt = items[0] {} else { XCTFail("the scheduled prompt folds: \(items[0])") }
        if case .reply(_, let thinking, _) = items[1] { XCTAssertEqual(thinking, "Starting now.") }
        else { XCTFail("\(items[1])") }
        if case .user = items[2] {} else { XCTFail("a typed follow-up is a bubble: \(items[2])") }
    }
}
