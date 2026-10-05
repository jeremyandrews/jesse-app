import Foundation
import Observation
import JesseCore
import JesseConversations
import SwiftData

// Search orchestration for the conversation list, shared by iOS and macOS: the one
// search pass per settled query, and the query-expansion tier on top of it. Sits between
// the live search field and the index: it debounces typing, runs the pass OFF the main
// actor, decides WHEN to ask the injected `QueryExpanding` for alternate terms, caches,
// cancels stale work, and publishes the ranked `result` the list draws.
//
// The contract is a field that never blocks:
//   * DEBOUNCE  the search pass waits ~120 ms of quiet; the model ~300 ms, so a burst of
//     keystrokes is one pass and one model call, not one per character.
//   * OFF MAIN  the pass runs in `ThreadSearchIndex` (an actor). The main actor only
//     stamps the threads (identity, title, `updatedAt`) and receives the answer.
//   * STALE     a query change cancels the in-flight pass and expansion; their answers
//     are never applied. Until the new pass lands, the list keeps the previous result.
//   * GATE      the model is asked for any query of three or more characters, with the
//     tier enabled and the model available (`shouldExpand`). The base hit count is NOT
//     a condition any more: it used to be, and it kept the model from ever running.
//   * CACHE     a session-scoped LRU keyed by the normalized query, so a repeat or a
//     backspaced-then-retyped query is expanded at most once.
//   * PUBLISH   `result` is the ranked answer (title hits, body hits, expansion only
//     hits) with the terms it applied; `isExpanding` is true while the model works.
@MainActor
@Observable
public final class ThreadSearchModel {
    /// Alternate search terms for the live query, as the expander returned them. Empty
    /// when idle, gated off, or the expander returned nothing. The list draws
    /// `result.terms`, the terms its rows were actually matched with.
    public private(set) var activeTerms: [String] = []

    /// True from the moment an expansion is scheduled for the live query until its terms
    /// land (or it is cancelled): the "expanding" state under the search field.
    public private(set) var isExpanding = false

    /// The ranked answer to the last settled query. `.inactive` while the field is blank.
    public private(set) var result: ThreadSearchResult = .inactive

    /// Master on/off for the expansion tier (the Settings toggle). When off, `update`
    /// never calls the expander and any applied terms are dropped.
    public var isEnabled: Bool {
        didSet { if !isEnabled { dropExpansion() } }
    }

    /// Whether the expander can run, and why not, for Settings.
    public var availability: QueryExpansionAvailability { expander.availability }

    private let expander: QueryExpanding
    private var index: ThreadSearchIndex?
    /// The one background build `prepare` starts.
    private var prepareTask: Task<Void, Never>?
    private let debounce: Duration
    private let searchDebounce: Duration
    private let cacheCapacity: Int

    /// LRU cache of normalized query to expansion terms. `lruOrder` is most-recent
    /// last; on capacity the front (least-recent) entry is evicted.
    private var cache: [String: [String]] = [:]
    private var lruOrder: [String] = []

    /// The live query: trimmed as typed (what a result answers), and normalized (the
    /// expansion identity and cache key).
    private var typedQuery = ""
    private var currentQuery = ""
    /// The in-flight expansion and the query it is expanding.
    private var task: Task<Void, Never>?
    private var taskQuery: String?
    /// The in-flight search pass.
    private var searchTask: Task<Void, Never>?
    /// The threads the list holds, as last handed in; stamped when a pass fires.
    private var threads: [JesseThread] = []

    /// `index` nil leaves the search pass out (the expansion tier alone, as its own unit
    /// tests drive it). Debounces and cache size are injectable so tests stay
    /// deterministic and fast.
    public init(expander: QueryExpanding,
                index: ThreadSearchIndex? = nil,
                isEnabled: Bool = true,
                debounce: Duration = .milliseconds(300),
                searchDebounce: Duration = .milliseconds(120),
                cacheCapacity: Int = 32) {
        self.expander = expander
        self.index = index
        self.isEnabled = isEnabled
        self.debounce = debounce
        self.searchDebounce = searchDebounce
        self.cacheCapacity = cacheCapacity
    }

    /// Feed the live query text (and, when they changed, the list's threads). Cheap:
    /// it schedules work and returns. Safe to call on every keystroke.
    public func update(query: String, threads: [JesseThread]? = nil) {
        if let threads { self.threads = threads }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let normalized = trimmed.lowercased()

        // A different query invalidates any in-flight expansion for the old one:
        // cancel it so its (now stale) terms can never be applied.
        if normalized != currentQuery {
            task?.cancel()
            task = nil
            taskQuery = nil
            isExpanding = false
            activeTerms = []
        }
        let queryChanged = trimmed != typedQuery
        currentQuery = normalized
        typedQuery = trimmed

        updateExpansion(normalized)
        if queryChanged { scheduleSearch(after: searchDebounce) }
    }

    /// Give the model its index, built over `container`, if it has none yet. The view
    /// calls this once it has the store; a model without an index runs no pass.
    public func attach(_ container: ModelContainer) {
        if index == nil { index = ThreadSearchIndex(container: container) }
    }

    /// Build the index in the background ahead of the first search (the search field
    /// gained focus), so the first query's pass finds every document ready. The first
    /// build reads every turn once; after it, passes rebuild only changed threads.
    public func prepare(_ threads: [JesseThread]) {
        self.threads = threads
        guard let index, prepareTask == nil else { return }
        let stamps = threads.map(ThreadSearchStamp.init)
        prepareTask = Task(priority: .utility) { await index.refresh(stamps) }
    }

    /// The list's threads changed (a reply landed, a thread was added or deleted) while
    /// a search may be showing: re-run the pass over them.
    public func threadsChanged(_ threads: [JesseThread]) {
        self.threads = threads
        if !typedQuery.isEmpty { scheduleSearch(after: searchDebounce) }
    }

    /// `nonisolated` so releasing the model never hops to the main actor: under the
    /// module's MainActor-default isolation the synthesized deinit would otherwise be
    /// MainActor-isolated, and destroying an instance off the main actor (as a test
    /// host or a background release can) would route through the isolated-deinit
    /// executor hop, which aborts. The stored properties still release normally after
    /// this body, and the tasks capture `self` weakly, so a dropped model leaves
    /// nothing running against it.
    nonisolated deinit {}

    /// Warm the expander (on search-field focus) so the first query doesn't pay
    /// cold-start latency. Forwards to the injected expander's optional `prewarm`.
    public func prewarm() {
        expander.prewarm()
    }

    /// Clear all state (search dismissed). Cancels any in-flight work.
    public func clear() {
        task?.cancel()
        task = nil
        taskQuery = nil
        searchTask?.cancel()
        searchTask = nil
        currentQuery = ""
        typedQuery = ""
        activeTerms = []
        isExpanding = false
        result = .inactive
    }

    /// Test hook: await the in-flight expansion (if any).
    public func awaitPendingExpansion() async {
        await task?.value
    }

    /// Test hook: await everything in flight, expansion and the pass it triggers, so
    /// assertions run against the settled `result`.
    public func settle() async {
        await task?.value
        await searchTask?.value
    }

    // MARK: - Internals

    private func updateExpansion(_ normalized: String) {
        // Tier disabled (Settings toggle off) or search idle -> never call the model.
        guard isEnabled, !normalized.isEmpty else {
            activeTerms = []
            return
        }
        // Gate: a trivial query, or no model to ask.
        guard shouldExpand(query: normalized, enabled: isEnabled,
                           available: expander.availability.isAvailable) else {
            activeTerms = []
            return
        }
        // Cache hit -> apply immediately, no expander call; the pass picks the terms up.
        if let cached = cachedTerms(for: normalized) {
            activeTerms = cached
            return
        }
        // Already expanding exactly this query -> let it finish (no duplicate call).
        if taskQuery == normalized, task != nil { return }

        // Miss -> debounce, then a single expander call; apply only if still current.
        taskQuery = normalized
        isExpanding = true
        task = Task { [weak self] in
            guard let self else { return }
            try? await Task.sleep(for: self.debounce)
            if Task.isCancelled { return }
            let terms = await self.expander.expand(normalized)
            if Task.isCancelled { return }
            self.applyExpansion(terms, for: normalized)
        }
    }

    /// Fold the expander's result into the cache and, if the user is still on this
    /// query, publish it and re-run the pass so the widened set lands.
    private func applyExpansion(_ terms: [String], for query: String) {
        store(terms, for: query)
        guard query == currentQuery else { return }
        taskQuery = nil
        isExpanding = false
        activeTerms = terms
        if !terms.isEmpty { scheduleSearch(after: .zero) }
    }

    /// The tier was switched off: drop terms and re-run the pass without them.
    private func dropExpansion() {
        task?.cancel()
        task = nil
        taskQuery = nil
        isExpanding = false
        let hadTerms = !activeTerms.isEmpty || !result.terms.isEmpty
        activeTerms = []
        if hadTerms { scheduleSearch(after: .zero) }
    }

    /// The one search pass: after `delay`, stamp the threads, run the index off the main
    /// actor with the current terms, and publish if the query is still the live one.
    private func scheduleSearch(after delay: Duration) {
        searchTask?.cancel()
        searchTask = nil
        guard !typedQuery.isEmpty else {
            result = .inactive
            return
        }
        guard let index else { return }
        let query = typedQuery
        searchTask = Task { [weak self] in
            if delay > .zero { try? await Task.sleep(for: delay) }
            guard let self, !Task.isCancelled else { return }
            let stamps = self.threads.map(ThreadSearchStamp.init)
            let terms = self.activeTerms
            let answer = await index.search(stamps, query: query, terms: terms)
            guard !Task.isCancelled, query == self.typedQuery else { return }
            self.result = answer
        }
    }

    private func cachedTerms(for key: String) -> [String]? {
        guard let terms = cache[key] else { return nil }
        touch(key)
        return terms
    }

    private func store(_ terms: [String], for key: String) {
        cache[key] = terms
        touch(key)
        // Evict least-recently-used entries beyond capacity.
        while lruOrder.count > cacheCapacity {
            let evict = lruOrder.removeFirst()
            cache.removeValue(forKey: evict)
        }
    }

    /// Move `key` to the most-recent end of the LRU order.
    private func touch(_ key: String) {
        if let i = lruOrder.firstIndex(of: key) { lruOrder.remove(at: i) }
        lruOrder.append(key)
    }
}
