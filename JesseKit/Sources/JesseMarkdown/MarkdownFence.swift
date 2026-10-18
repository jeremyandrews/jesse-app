import Foundation

// WHERE A FENCED CODE BLOCK OPENS AND WHERE IT CLOSES, as one public rule.
//
// Two parsers read fences: `MarkdownBlockParser` here, for the vault reader, and the app's
// reply parser, for Jesse's answers on the phone and the Mac. The reply parser used to keep a
// rule of its own (any line starting with three backticks toggled a block), which closed a
// block on an info string line, never knew a tilde fence or a four backtick one, and so cut a
// quoted fence in half. One rule, here, is what keeps the two from disagreeing again.

/// An OPEN fence: the character it was made of, how many of them, and the info string after.
public struct MarkdownFence: Equatable, Sendable {
    /// A backtick or a tilde.
    public let character: Character
    /// How many of `character` opened the block. A closing run must be at least this long.
    public let length: Int
    /// Whatever followed the opening run, trimmed: the language tag, or "".
    public let info: String

    /// The fence `line` opens, or nil when it opens none: three or more backticks or tildes,
    /// after any leading whitespace.
    public static func opening(_ line: String) -> MarkdownFence? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") else { return nil }
        let character = Character(trimmed.hasPrefix("```") ? "`" : "~")
        let length = trimmed.prefix { $0 == character }.count
        let info = String(trimmed.drop { $0 == character }).trimmingCharacters(in: .whitespaces)
        return MarkdownFence(character: character, length: length, info: info)
    }

    /// CommonMark's closing fence rule: at least as many of the SAME fence character as
    /// opened the block, and nothing after them but spaces.
    ///
    /// "Nothing after them" is load bearing rather than pedantry. A line reading
    /// ```` ```swift ```` is an OPENING fence's info string, so it cannot close one, and a
    /// parser that let it would silently swallow the word `swift`, which is exactly the kind
    /// of disappearance the fuzz test exists to catch, and did.
    public func isClosed(by line: String) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        let run = trimmed.prefix { $0 == character }.count
        return run >= length && run == trimmed.count
    }
}
