import Foundation
import SwiftData
import JesseCore

/// The screenshot seam for the transcript folds (the Prompt and Thinking rows), and nothing
/// else.
///
/// What the folds look like can only be seen in the running app, and what they look like
/// OPEN needs a tap. This seam supplies both without driving the UI: it seeds one scheduled
/// run conversation (an untyped prompt, two narration turns, an answer) and posts the same
/// push tap `ThreadLandingUITestSeam` does, so the app opens it the way a notification would.
/// Its value says which fold starts open, so each state is one headless launch and one
/// `simctl io screenshot`, with no UI automation.
///
/// `JESSE_UITEST_FOLDS=collapsed|prompt|thinking` arms it. ABSENT, which is every ordinary
/// launch, `mode` is nil and `arm(context:)` returns at once. Compiled out of Release, and a
/// launch environment is set only by a debugger, an XCTest runner or `simctl launch`, never
/// by anything a shipped build meets: the same terms as `ThreadLandingUITestSeam`.
@MainActor
enum TranscriptFoldUITestSeam {
    enum Mode: String { case collapsed, prompt, thinking }

    /// Which fold starts open, or nil in every ordinary launch. Read ONCE.
    static let mode: Mode? = {
        #if DEBUG
        guard let raw = ProcessInfo.processInfo.environment["JESSE_UITEST_FOLDS"] else {
            return nil
        }
        return Mode(rawValue: raw.trimmingCharacters(in: .whitespacesAndNewlines))
        #else
        return nil
        #endif
    }()

    private static var armed = false
    private static var openAtStart: Set<UUID> = []

    /// The folds the seeded conversation opens with. Empty for every other conversation and
    /// in every ordinary launch, so the detail view's own rule (all folds closed on open)
    /// stands.
    static func initialOpenFolds(for threadID: UUID) -> Set<UUID> {
        guard mode != nil, seededThreadID == threadID else { return [] }
        return openAtStart
    }

    private static var seededThreadID: UUID?

    /// Seed the conversation and post the tap that opens it.
    static func arm(context: ModelContext) {
        guard let mode, !armed else { return }
        armed = true
        let thread = JesseThread(title: "Archive box", mode: .tell)
        let conversationId = JesseThread.mintConversationId()
        thread.conversationId = conversationId
        // As the bridge's conversation list records a scheduled fire: the opening turn is
        // folded because the CONVERSATION says what sent it, exactly the synced path.
        thread.sentFor = "Scheduled: archive box"
        context.insert(thread)
        let start = Date().addingTimeInterval(-120)
        let prompt = Turn(role: .user, text: """
            Process the archive box. For every note under Inbox/ whose archive footer is \
            checked, move it to the matching archive folder, update any links that pointed \
            at it, and record what moved. Skip anything unchecked. Do not delete anything; \
            relocations are a single move. When you are done, report what moved and what you \
            skipped and why, one line each. If nothing is checked, reply with the quiet \
            sentinel and nothing else.
            """, createdAt: start)
        let narration1 = Turn(role: .jesse, text: "Starting now: I'll find the checked archive boxes first.",
                              createdAt: start.addingTimeInterval(5))
        narration1.isNarration = true
        let narration2 = Turn(role: .jesse, text: "Checking the footer format before moving anything.",
                              createdAt: start.addingTimeInterval(20))
        narration2.isNarration = true
        let answer = Turn(role: .jesse, text: """
            Moved 3 notes to `Inbox/archive/`:

            - **Dentist reminder**: done, link from Today.md updated.
            - **Router firmware note**: done.
            - **Lunch ideas**: done.

            Skipped 1: *Perseido invoice* is checked but still linked from an open Dashboard item.
            """, createdAt: start.addingTimeInterval(60))
        for turn in [prompt, narration1, narration2, answer] {
            turn.thread = thread
            context.insert(turn)
        }
        try? context.save()
        seededThreadID = thread.id
        switch mode {
        case .collapsed: openAtStart = []
        case .prompt: openAtStart = [prompt.id]
        case .thinking: openAtStart = [answer.id]
        }
        PushRouter.shared.pendingTap = PushTap(jobId: nil, conversationId: conversationId)
    }
}
