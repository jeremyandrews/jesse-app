import Foundation
import SwiftData
import JesseCore

// A realistic conversation store for search performance tests: 500 threads, turn
// counts and body sizes shaped like the real history (short prompts with the odd
// multi-kilobyte routine prompt, replies of a few hundred words, narration on some),
// written to an ON-DISK store and reopened through a fresh container so every turn
// body is a real fault, exactly as the app's list sees it after launch.
//
// Deterministic: a fixed-seed generator, so a timing regression is the code's and
// never the fixture's.
@MainActor
enum SearchFixture {

    /// A store on disk under a temporary directory, filled and saved, then reopened.
    /// The returned context holds no materialized turns yet.
    struct Store {
        let container: ModelContainer
        let context: ModelContext
        let directory: URL

        func threads() throws -> [JesseThread] {
            try context.fetch(FetchDescriptor<JesseThread>(
                sortBy: [SortDescriptor(\.updatedAt, order: .reverse)]))
        }

        func remove() { try? FileManager.default.removeItem(at: directory) }
    }

    static let threadCount = 500

    static func make(threads count: Int = threadCount) throws -> Store {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("search-fixture-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("fixture.store")
        do {
            let writer = try ModelContainer(
                for: jesseCurrentSchema,
                configurations: ModelConfiguration(schema: jesseCurrentSchema, url: url))
            let context = ModelContext(writer)
            var rng = SplitMix(seed: 0x5EA2C4)
            let now = Date(timeIntervalSince1970: 1_790_000_000)
            for i in 0..<count {
                // Spread over about eighteen months, newest first by index.
                let updated = now.addingTimeInterval(-Double(i) * 95_000
                                                     - Double(rng.next(3600)))
                let topics = [Self.topics[rng.next(Self.topics.count)],
                              Self.topics[rng.next(Self.topics.count)]].flatMap { $0 }
                let thread = JesseThread(title: sentence(&rng, topics, words: 4 + rng.next(5),
                                                         topicShare: 40),
                                         mode: .ask, createdAt: updated)
                context.insert(thread)
                let turnCount = turnsFor(&rng)
                for t in 0..<turnCount {
                    let at = updated.addingTimeInterval(Double(t - turnCount) * 60)
                    if t % 2 == 0 {
                        // 1 in 10 prompts is a long routine prompt.
                        let words = rng.next(10) == 0 ? 900 + rng.next(900) : 15 + rng.next(60)
                        thread.turns.append(Turn(role: .user, text: sentence(&rng, topics, words: words),
                                                 createdAt: at))
                    } else {
                        let turn = Turn(role: .jesse,
                                        text: sentence(&rng, topics, words: 120 + rng.next(700)),
                                        createdAt: at)
                        if rng.next(10) < 3 {
                            turn.thinkingText = sentence(&rng, topics, words: 40 + rng.next(160))
                        }
                        thread.turns.append(turn)
                    }
                }
                thread.updatedAt = updated
            }
            try context.save()
        }
        let container = try ModelContainer(
            for: jesseCurrentSchema,
            configurations: ModelConfiguration(schema: jesseCurrentSchema, url: url))
        return Store(container: container, context: ModelContext(container), directory: dir)
    }

    /// 2 to 40 turns, most conversations short, a long tail of long ones.
    private static func turnsFor(_ rng: inout SplitMix) -> Int {
        let r = rng.next(100)
        if r < 50 { return 2 + rng.next(6) }
        if r < 85 { return 8 + rng.next(12) }
        return 20 + rng.next(21)
    }

    /// Words drawn mostly from common English, a share from this thread's own topics,
    /// and a few rare words, so a topic word is common inside its threads and absent
    /// from most others, as in the real history.
    private static func sentence(_ rng: inout SplitMix, _ topics: [String], words: Int,
                                 topicShare: Int = 6) -> String {
        var out: [String] = []
        out.reserveCapacity(words)
        for _ in 0..<words {
            let r = rng.next(100)
            if r < topicShare {
                out.append(topics[rng.next(topics.count)])
            } else if r < topicShare + 4 {
                out.append(rare[rng.next(rare.count)])
            } else {
                out.append(common[rng.next(common.count)])
            }
        }
        return out.joined(separator: " ")
    }

    static let common: [String] = """
    the a an and or but if then when while of to in on at by for with from about \
    into over after before between under again further once here there all any both \
    each few more most other some such no nor not only own same so than too very can \
    will just should now is are was were be been being have has had do does did it \
    this that these those we you they he she i me my our your their what which who \
    how why where yes ok okay sure thanks please also still already maybe because \
    make made get got take took see saw know knew think thought want need look find \
    give tell work call try ask feel seem leave put mean keep let begin show hear \
    time day week month year way thing part place case point fact group number \
    good new first last long great little right big high different small large next \
    early young important public bad able done check update change set start stop
    """.split(whereSeparator: \.isWhitespace).map(String.init)

    /// Topic clusters; each thread draws two. The measured queries' words live here.
    static let topics: [[String]] = [
        ["bridge", "launchd", "restart", "deploy", "health", "endpoint"],
        ["jesse", "app", "simulator", "build", "testflight", "xcode"],
        ["run", "running", "marathon", "pace", "mile", "km"],
        ["diet", "weight", "protein", "calories", "carbs", "weigh-in"],
        ["meeting", "agenda", "notes", "attendees", "followup", "minutes"],
        ["draft", "email", "reply", "voice", "tone", "send"],
        ["vault", "note", "obsidian", "search", "index", "qmd"],
        ["perseido", "fiber", "rebrand", "wordpress", "gandi", "site"],
        ["network", "router", "wifi", "unifi", "vlan", "switch"],
        ["italy", "rome", "travel", "trip", "flight", "hotel"],
        ["kids", "school", "birthday", "aurora", "arlo", "party"],
        ["invoice", "budget", "tax", "bank", "payment", "contract"],
        ["hire", "candidate", "interview", "team", "lead", "engineer"],
        ["drupal", "scolta", "trovato", "ritrovo", "kernel", "plugin"],
        ["currency", "euro", "dollar", "bitcoin", "price", "report"],
        ["philosophy", "book", "chapter", "story", "summary", "essay"],
        ["café", "málaga", "naïve", "résumé", "jalapeño", "crème"],
        ["error", "failure", "crash", "timeout", "latency", "cache"],
        ["swift", "rust", "typescript", "sqlite", "postgres", "redis"],
        ["kamado", "pizza", "oven", "grill", "recipe", "bread"],
        ["workout", "heart", "sleep", "steps", "vo2", "bradycardia"],
        ["k3s", "cluster", "kubernetes", "docker", "proxmox", "node"],
        ["strand", "prompt", "queue", "launch", "outcome", "split"],
        ["calendar", "slack", "whatsapp", "imessage", "fastmail", "gmail"],
    ]

    /// Rare words: 3,000 synthetic syllable words, each seen a handful of times.
    static let rare: [String] = {
        let syllables = ["ka", "lo", "mi", "ne", "su", "ta", "ri", "po", "ve", "zu",
                         "do", "fe", "gi", "ha", "bo"]
        var rng = SplitMix(seed: 7)
        return (0..<3000).map { _ in
            (0..<(2 + rng.next(3))).map { _ in syllables[rng.next(syllables.count)] }.joined()
        }
    }()
}
/// SplitMix64: tiny, fast, and the same sequence on every platform.
struct SplitMix {
    private var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next(_ bound: Int) -> Int {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        z ^= z >> 31
        return Int(z % UInt64(bound))
    }
}
