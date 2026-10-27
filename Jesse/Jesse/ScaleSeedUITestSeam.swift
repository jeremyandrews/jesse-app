import Foundation
import SwiftData
import JesseCore
import JesseConversations
import JesseVault

/// The UI-test seam for measuring navigation cost at a realistic scale, and nothing else.
///
/// "Switching around the app has become slow" is a claim about cost that grows with the
/// owner's data, and an earlier fix in this repo measured clean in the simulator on a
/// small store and was still slow on the phone. So the measurement has to run against a
/// store the size of a real one: this seeds it.
///
/// `JESSE_UITEST_SCALE=<conversations>` arms it. The store is then opened at its own file,
/// one per scale (`pf1-scale-1000.store`), so the 1,000 conversation store and the 5
/// conversation control are each seeded ONCE per simulator install and reused by every
/// later launch, and neither one ever touches the ordinary store. What it seeds, for a
/// count above 5:
///
///  * that many conversations, newest first, lengths drawn from 1 to 400 turns (median
///    20), replies of real Markdown length (median 2,500 characters, about one in a
///    hundred over 20,000) with fences, tables and lists;
///  * one conversation of exactly 200 turns on top, named `probeTitle`, which the tests
///    open and back out of;
///  * one in ten archived, one in twenty five a favorite, one in fifteen unread, one in
///    fifty relayed from the watch, and four with an undelivered (`.failed`) message;
///  * dates spread over two years, so all but the newest few fall into month folders;
///  * every AI title current (`titleSourceKey` matching the content), as on a phone that
///    has been in use for a while;
///  * a vault of 5,000 notes in the app's own Documents folder, held as the vault folder.
///
/// The control (`5`) is the 200 turn conversation and four short ones, and the same vault.
///
/// Every conversation's ids, flags and clocks are a pure function of its index, so the
/// UI test's stub bridge can serve the matching `GET /jesse/conversations` list without
/// knowing anything the app knows: see `conversationId(_:)` and `NavigationCostUITests`.
///
/// ABSENT, which is every ordinary launch, `scale` is nil and nothing here runs. Compiled
/// out of Release entirely: the seeding code itself is inside `#if DEBUG`, so a shipped
/// build carries none of it, on the same terms as `JESSE_UITEST_BRIDGE`.
@MainActor
enum ScaleSeedUITestSeam {

    /// The number of conversations a UI test asked for, read once. Nil in every ordinary
    /// launch and in every Release build.
    nonisolated static let scale: Int? = {
        #if DEBUG
        guard let raw = ProcessInfo.processInfo.environment["JESSE_UITEST_SCALE"],
              let count = Int(raw), count > 0 else { return nil }
        return count
        #else
        return nil
        #endif
    }()

    /// The store file for the armed scale, or nil (the ordinary store) when unarmed.
    nonisolated static var storeURL: URL? {
        guard let scale else { return nil }
        let support = URL.applicationSupportDirectory
        try? FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        return support.appendingPathComponent("pf1-scale-\(scale).store")
    }

    /// The 200 turn conversation's name, in its row and its navigation bar. The UI tests
    /// duplicate it, because a UI test runs in its own process and cannot import the app.
    static let probeTitle = "Scale probe, 200 turns"

    /// The conversation id for seeded index `i`, as the stub bridge also derives it.
    nonisolated static func conversationId(_ i: Int) -> String {
        String(format: "%08x-0000-4000-8000-777777777777", i)
    }

    /// Seed the armed scale's store if it is empty. A no-op when unarmed.
    static func arm(context: ModelContext) {
        #if DEBUG
        guard let scale else { return }
        seedVaultIfNeeded()
        let held = (try? context.fetchCount(FetchDescriptor<JesseThread>())) ?? 0
        guard held == 0 else { return }
        seed(count: scale, context: context)
        #endif
    }

    #if DEBUG

    // MARK: - Conversations

    /// The rules the stub bridge mirrors. Pure functions of the index, so the list the
    /// stub serves agrees with the store field for field and a sync pass changes nothing.
    nonisolated static func isArchived(_ i: Int) -> Bool { i % 10 == 3 }
    nonisolated static func isFavorite(_ i: Int) -> Bool { i % 25 == 5 }
    nonisolated static func isUnread(_ i: Int) -> Bool { i % 15 == 1 }
    nonisolated static func isWatch(_ i: Int) -> Bool { i % 50 == 7 }
    nonisolated static func hasFailedSend(_ i: Int, of count: Int) -> Bool {
        count > 5 && [2, 6, 41, 301].contains(i)
    }
    nonisolated static func title(_ i: Int) -> String {
        i == 0 ? "Scale probe, 200 turns" : "Seeded conversation \(i): \(topics[i % topics.count])"
    }
    nonisolated static func sessionId(_ i: Int) -> String { "pf1-sess-\(i)" }
    /// A fixed clock rather than `now`, so the stub needs no shared state with the app.
    nonisolated static func lastReplyMs(_ i: Int) -> Int { 1_700_000_000_000 + i }
    nonisolated static func readThroughMs(_ i: Int) -> Int { isUnread(i) ? 0 : lastReplyMs(i) }
    nonisolated static func flagClockMs(_ i: Int) -> Int { 1_700_000_000_000 }

    nonisolated private static let topics = [
        "kiln controller wiring", "Q3 hiring plan", "marathon taper", "fiber rebrand copy",
        "vault index design", "Italian residency forms", "pizza oven hydration",
        "bridge deploy runbook", "strand queue cleanup", "vendor contract review",
        "garden irrigation", "search expansion terms", "watch relay retries",
        "board meeting prep", "BBQ brisket timing", "client SOW draft",
    ]

    /// How many turns conversation `i` has: log-normal, median 20, clamped to 1...400.
    private static func turnCount(_ i: Int, count: Int, rng: inout SeedRandom) -> Int {
        if i == 0 { return 200 }
        if count <= 5 { return [4, 10, 20, 2][(i - 1) % 4] }
        let z = rng.normal()
        return min(400, max(1, Int((log(20.0) + 1.1 * z).exp.rounded())))
    }

    /// When conversation `i` was last active: the newest few today and this week (so
    /// they are loose rows), the rest spread over two years of month folders.
    private static func updatedAt(_ i: Int, now: Date) -> Date {
        let minutes: Double = i < 6 ? Double(i) * 40 : 60 * 24 * (Double(i - 5) * 0.73 + 2)
        return now.addingTimeInterval(-minutes * 60)
    }

    private static func seed(count: Int, context: ModelContext) {
        var rng = SeedRandom(seed: 0x5EED_0000 &+ UInt64(count))
        let now = Date()
        var threads: [JesseThread] = []
        for i in 0..<count {
            let stamp = updatedAt(i, now: now)
            let n = turnCount(i, count: count, rng: &rng)
            let thread = JesseThread(title: title(i), mode: .ask,
                                     createdAt: stamp.addingTimeInterval(-Double(n) * 90))
            thread.conversationId = conversationId(i)
            thread.sessionId = sessionId(i)
            thread.aiTitle = title(i)
            thread.registeredAt = thread.createdAt
            if isWatch(i) { thread.origin = ThreadOrigin.watch.rawValue }
            if isFavorite(i) {
                thread.isFavorite = true
                thread.favoritedAt = stamp
                thread.favoriteUpdatedMs = flagClockMs(i)
            }
            if isArchived(i) {
                thread.isArchived = true
                thread.archivedAt = stamp
                thread.archivedUpdatedMs = flagClockMs(i)
            }
            thread.lastReplyMs = lastReplyMs(i)
            thread.readThroughMs = readThroughMs(i)
            thread.readUpdatedMs = isUnread(i) ? 0 : flagClockMs(i)
            context.insert(thread)
            var lastUser: Turn?
            for t in 0..<n {
                let user = t % 2 == 0
                let text = user ? userText(i, t, rng: &rng) : replyText(i, t, rng: &rng)
                let turn = Turn(role: user ? .user : .jesse, text: text,
                                createdAt: stamp.addingTimeInterval(-Double(n - t) * 90))
                turn.thread = thread
                context.insert(turn)
                if user { lastUser = turn }
            }
            // Last, so nothing above (an insert, a turn) moves it.
            thread.updatedAt = stamp
            if hasFailedSend(i, of: count), let lastUser {
                context.insert(OutboxItem(threadID: thread.id, turnID: lastUser.id,
                                          text: lastUser.text, mode: .ask, voice: false,
                                          state: .failed, createdAt: lastUser.createdAt))
            }
            threads.append(thread)
            if i % 50 == 49 { try? context.save() }
        }
        try? context.save()
        // Every title current, as on a phone in use: the key is the shared function's own,
        // computed once over the saved turns.
        for thread in threads {
            thread.titleSourceKey = threadContentKey(for: thread)
        }
        try? context.save()
    }

    private static func userText(_ i: Int, _ t: Int, rng: inout SeedRandom) -> String {
        let length = Int(rng.uniform(40, 420))
        return fill("Question \(t / 2 + 1) about \(topics[(i + t) % topics.count]). ",
                    from: userCorpus, start: i + t, to: length)
    }

    /// A reply of real Markdown length: log-normal, median 2,500 characters.
    private static func replyText(_ i: Int, _ t: Int, rng: inout SeedRandom) -> String {
        let length = min(32_000, max(80, Int((log(2_500.0) + 0.9 * rng.normal()).exp)))
        return fill("## Reply \(t / 2 + 1) on \(topics[(i + t) % topics.count])\n\n",
                    from: replyCorpus, start: i + t, to: length)
    }

    /// `head`, then whole blocks of `corpus` until `length` is reached, starting at a
    /// varying block so neighbouring replies differ.
    private static func fill(_ head: String, from corpus: [String], start: Int,
                             to length: Int) -> String {
        var out = head
        var k = start
        while out.count < length {
            out += corpus[k % corpus.count]
            k += 1
        }
        return out
    }

    private static let userCorpus = [
        "Can you check what we decided last week and whether it still holds? ",
        "Also pull anything from the vault that mentions it. ",
        "Keep it short, I am on the phone. ",
        "What is still open, and who owns it? ",
    ]

    private static let replyCorpus = [
        """
        The short answer is yes, with one condition: the change has to land before the \
        vendor call, because the quote they sent assumes the old schedule and the new one \
        moves two of the deliveries by a week. Everything else in the plan holds as written.

        """,
        """
        - **Owner:** you, until Thursday, then the ops lead.
        - **Blocked on:** the signed amendment, which is in the drafts folder.
        - **Next step:** read the amendment and reply with any changes.

        """,
        """
        | Item | Status | Due |
        |---|---|---|
        | Contract amendment | Drafted | Thu |
        | Deposit | Paid | Done |
        | Delivery window | Moved | Next week |
        | Site visit | Not booked | Fri |

        """,
        """
        ```swift
        func schedule(_ items: [Item], from start: Date) -> [Slot] {
            var cursor = start
            return items.map { item in
                defer { cursor = cursor.addingTimeInterval(item.duration) }
                return Slot(item: item, start: cursor)
            }
        }
        ```

        """,
        """
        ### What changed since last time

        Two things moved. The first is the budget line for the second quarter, which went \
        up because the hosting renewal came in higher than the estimate; the second is the \
        hiring plan, where the senior role is now a contractor for the first three months. \
        Neither one changes the decision, but both change the numbers in the summary.

        """,
        """
        > Note from the vault: the original decision is recorded in the project file, with \
        > the reasoning and the two alternatives that were rejected.

        """,
    ]

    // MARK: - The vault

    /// 5,000 notes in the app's Documents folder, written once, and held as the vault
    /// folder. A folder inside the container needs no security scope, which
    /// `VaultFolder.resolve` already allows for.
    private static func seedVaultIfNeeded() {
        let root = URL.documentsDirectory.appendingPathComponent("pf1-vault", isDirectory: true)
        let marker = root.appendingPathComponent(".seeded")
        let fm = FileManager.default
        if !fm.fileExists(atPath: marker.path) {
            let folders = ["Projects/Tag1", "Projects/Personal", "Projects/drafts",
                           "Knowledge/People", "Knowledge/Cookbook", "Knowledge/Reference",
                           "Inbox", "Projects/Research"]
            for n in 0..<5_000 {
                let folder: String
                let name: String
                if n < 40 {
                    folder = VaultStrandRecord.folder
                    name = "Strand-\(n)"
                } else {
                    folder = folders[n % folders.count]
                    name = "Note-\(n)"
                }
                let dir = root.appendingPathComponent(folder, isDirectory: true)
                try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
                let body = """
                ---
                updated: 2026-10-\(String(format: "%02d", n % 27 + 1))
                ---
                # \(name.replacingOccurrences(of: "-", with: " "))

                Seeded note \(n) about \(topics[n % topics.count]). \
                \(String(repeating: "A line of ordinary note text that the index chunks. ", count: 20))

                - [ ] An open item in note \(n)
                - [x] A finished item

                """
                try? body.write(to: dir.appendingPathComponent("\(name).md"),
                                atomically: false, encoding: .utf8)
            }
            fm.createFile(atPath: marker.path, contents: Data())
        }
        let folder = VaultFolder()
        if !folder.hasBookmark { try? folder.adopt(url: root) }
    }

    #endif
}

#if DEBUG
/// A small deterministic generator (SplitMix64), so a seeded store is the same store on
/// every simulator and the before and after measurements compare like with like.
private struct SeedRandom {
    var state: UInt64
    init(seed: UInt64) { state = seed }

    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }

    mutating func unit() -> Double { Double(next() >> 11) / Double(1 << 53) }

    mutating func uniform(_ lo: Double, _ hi: Double) -> Double { lo + (hi - lo) * unit() }

    /// A standard normal draw (Box Muller).
    mutating func normal() -> Double {
        let u1 = max(unit(), .leastNonzeroMagnitude)
        let u2 = unit()
        return (-2 * log(u1)).squareRoot() * cos(2 * .pi * u2)
    }
}

private extension Double {
    var exp: Double { Foundation.exp(self) }
}
#endif
