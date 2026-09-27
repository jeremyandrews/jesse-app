import Foundation

/// A running turn's text as the thread shows it live: the narration so far and the answer so
/// far, never the one as if it were the other.
///
/// The cut is the one the bridge draws for the finished reply: text a tool call closed is
/// narration, and the text since the last tool call is the answer (or the next narration, if
/// another tool call follows it). A newer bridge's `reset` names the cut itself; a
/// commentary block arrives whole on its own frame. Against an older bridge the same rule
/// runs on its `activity` frames, so the live view splits the same way either way.
///
/// Pure and value-typed, so both apps fold the same events into the same two strings and a
/// test can drive it frame by frame.
public struct LiveReply: Equatable, Sendable {
    public private(set) var narration: [String] = []
    public private(set) var answer: String = ""

    public init() {}

    /// Fold one stream event in. Terminal events change nothing: the finished reply replaces
    /// the live text wholesale.
    public mutating func apply(_ event: JesseStreamEvent) {
        switch event {
        case .reset(let text):
            narration = []
            answer = text
        case .resetSplit(let n, let a):
            let n = n.trimmingCharacters(in: .whitespacesAndNewlines)
            narration = n.isEmpty ? [] : [n]
            answer = a
        case .delta(let chunk):
            answer += chunk
        case .activity:
            let closed = answer.trimmingCharacters(in: .whitespacesAndNewlines)
            if !closed.isEmpty { narration.append(closed) }
            answer = ""
        case .narration(let block):
            let b = block.trimmingCharacters(in: .whitespacesAndNewlines)
            if !b.isEmpty { narration.append(b) }
        case .done, .failed, .cancelled:
            break
        }
    }

    /// The narration as the Thinking row shows it, one paragraph per block. Empty when the
    /// turn has narrated nothing yet.
    public var thinking: String { narration.joined(separator: "\n\n") }
}
