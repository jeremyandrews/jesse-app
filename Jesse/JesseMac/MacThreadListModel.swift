import Foundation
import JesseCore
import JesseConversations
import JesseSearch

// The Mac sidebar's pure list seam. It wraps the shared `threadListLayout` so the
// Mac list is grouped / favorited / date-sectioned by exactly the same code the
// iPhone drives its list from (never a bare @Query sort), and so the wiring is
// unit-testable without a view host: switching `scope` flips the layout between
// the full sectioned view and the flat favorites view, and `expandedFolders`
// drives month-folder disclosure through the same pure helper the tests pin.
//
// It also owns the search the iPhone has: `searchText` is the typed query, and `search`
// is the shared `ThreadSearchModel` that runs the pass off the main actor and widens it
// with on-device query expansion. The settled, ranked answer is narrowed to the scope by
// the same `threadSearchLayout` the iPhone uses, so searching composes with the
// favorites / archived scopes for free. The expander is injected so tests use a fake and never
// depend on a real on-device model.
struct MacThreadListModel {

    /// Sidebar scope. `.all` is the whole history (date-sectioned, month buckets
    /// rendered as collapsible folders); `.favorites` is just starred conversations
    /// as one flat, newest-first list; `.archived` is just the conversations the user
    /// has hidden from the main list, also flat, and the one place to restore them.
    /// `.all` and `.favorites` both EXCLUDE archived threads. Archive state is
    /// local-first and converged across devices by the bridge flags, matching favorites.
    enum Scope: Hashable {
        case all
        case favorites
        case archived
    }

    var scope: Scope = .all

    /// Month folders the user has opened. Day sections (today / yesterday / the one
    /// weekday) are always expanded; month buckets default collapsed (absent here).
    var expandedFolders: Set<ThreadSection> = []

    /// The live typed query (Tier 1). Not persisted: a fresh launch starts unfiltered.
    var searchText: String = ""

    /// The shared on-device expansion orchestrator (Tier 2): debounce / gate / cache
    /// / cancel, publishing `activeTerms` the layout unions with the typed query. A
    /// reference type, so mutating `searchText`/`scope` on this struct keeps the same
    /// live instance (its `activeTerms` drive the view through Observation).
    let search: ThreadSearchModel

    /// Inject the expander (production: the FoundationModels-backed on-device model,
    /// passed by the view; tests: a fake) plus the Settings-driven enabled flag and,
    /// for tests, a shorter debounce. The default is the INERT `NoExpansion` so the
    /// scope/folder tests that call `MacThreadListModel()` never spin up the real
    /// on-device model, which is unavailable in CI (the search brief requires tests
    /// not to depend on it). This is best-practice, not a crash workaround: the abort
    /// a real expander once caused in a bare test host was the MainActor-isolated
    /// deinit, now fixed at the source (see `FoundationModelExpander` /
    /// `ThreadSearchModel`). The Mac view constructs with `FoundationModelExpander()`.
    init(searchExpander: QueryExpanding = NoExpansion(),
         searchEnabled: Bool = true,
         searchDebounce: Duration = .milliseconds(300),
         passDebounce: Duration = .milliseconds(120)) {
        self.search = ThreadSearchModel(expander: searchExpander,
                                        isEnabled: searchEnabled,
                                        debounce: searchDebounce,
                                        searchDebounce: passDebounce)
    }

    /// The settled search answer narrowed to the active scope, in rank order (title hits,
    /// body hits, then expansion only hits, each newest first), or nil when no search
    /// has landed (idle, or the first pass for this search is still running, when the
    /// sidebar keeps its ordinary layout). Matching never happens here: the pass ran off
    /// the main actor in `ThreadSearchIndex`.
    func searchRows(_ threads: [JesseThread]) -> [(thread: JesseThread, hit: ThreadSearchHit)]? {
        let typed = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !typed.isEmpty, search.result.isActive else { return nil }
        return threadSearchLayout(threads, result: search.result,
                                  favoritesOnly: scope == .favorites,
                                  archivedOnly: scope == .archived)
    }

    /// Build the sidebar layout. While a search shows, it is the flat ranked list from
    /// `searchRows`. Otherwise it is the shared pure layout: `.favorites` collapses to
    /// the flat starred list, `.archived` to the flat hidden list, and `.all` is the
    /// full date-sectioned layout with collapsible month folders. `now`/`calendar` are
    /// injected so classification is deterministic in tests (and read live in the view).
    func layout(_ threads: [JesseThread], now: Date, calendar: Calendar) -> ThreadListLayout {
        if let found = searchRows(threads) { return .flat(found.map(\.thread)) }
        return threadListLayout(threads,
                                favoritesOnly: scope == .favorites,
                                archivedOnly: scope == .archived,
                                searchQueries: [],
                                expanded: expandedFolders,
                                now: now,
                                calendar: calendar)
    }

    /// Feed the live query into the shared search model: keep the expansion tier's
    /// master switch in sync with Settings, then let the model debounce, run the pass
    /// off the main actor, and gate, cache and cancel the expansion. With the tier off
    /// the pass still runs on the typed query alone and the expander is never called.
    /// Mutates the `search` reference, not this struct.
    func updateSearch(_ threads: [JesseThread], enabled: Bool) {
        search.isEnabled = enabled
        search.update(query: searchText, threads: threads)
    }

    /// Flip the favorites filter (the keyboard-shortcut / segmented-control action).
    mutating func toggleFavoritesScope() {
        scope = (scope == .favorites) ? .all : .favorites
    }

    /// Flip a month folder's expanded state through the shared pure helper, so a
    /// disclosure tap does exactly what the JesseConversations tests pin.
    mutating func toggleFolder(_ section: ThreadSection) {
        expandedFolders = foldersAfterToggling(section, in: expandedFolders)
    }

    /// Star / unstar a conversation. A thin seam over `JesseThread.toggleFavorite`
    /// so the view's star action has one testable entry point; the view persists the
    /// context and best-effort pushes the change to the bridge afterwards (this only
    /// mutates the model object).
    func toggleFavorite(_ thread: JesseThread, now: Date = .now) {
        thread.toggleFavorite(now: now)
    }

    /// Archive / restore a conversation. A thin seam over `JesseThread.toggleArchived`
    /// (stamping/clearing `archivedAt`) so the view's archive action and its keyboard
    /// shortcut share one testable entry point; the view persists the context and
    /// best-effort pushes the change to the bridge afterwards. Local-first, converged
    /// across devices by the bridge flags (last-writer-wins).
    func toggleArchived(_ thread: JesseThread, now: Date = .now) {
        thread.toggleArchived(now: now)
    }
}
