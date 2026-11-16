import SwiftUI
import AppKit

// The menu bar's Conversations menu, and Command F.
//
// The Chats sidebar toolbar keeps only the three actions used every day (Refresh, the morning
// routine, New Chat). What left it (the favorites filter and Archive) is here, with the same
// shortcuts, and so are the two that stayed, listed so every Chats shortcut can be found in
// one place. Settings stays in the app menu, where it already was.
//
// HOW A MENU ITEM REACHES A TAB. A `Commands` body cannot see a window's state, so each
// window publishes one `MacWindowCommands` as a focused scene value: the menu reads the
// focused window's, and only that window acts. The menu never does the work itself. It
// files a request, and the view that owns the state (`MacRootView` for the list,
// `MacShellView` for the tab and the search fields) carries it out with the code its own
// controls already call.

/// One window's command state: what its Chats tab says the menu may offer, and the requests
/// the menu files for that tab to carry out.
@MainActor
@Observable
final class MacWindowCommands {
    /// The Chats tab's state, as the menu needs it. Nil while that tab is not on screen, which
    /// is what disables its items.
    struct Conversations: Equatable {
        var favoritesShown: Bool
        /// The selected conversation's archived state; nil with nothing selected.
        var selectionArchived: Bool?
        /// Whether a new conversation can be started (the Mac is paired).
        var canStartChat: Bool
    }

    enum Request: Equatable {
        case toggleFavorites
        case archiveSelected
        case newChat
        case refresh
    }

    /// A request, unique each time it is filed, so filing the same one twice is two changes.
    struct Filed: Equatable {
        let id = UUID()
        let request: Request
    }

    var conversations: Conversations?
    /// The Chats tab's next request, consumed by `MacRootView`.
    var pending: Filed?

    /// Which tab is on screen, kept by `MacShellView` so the search item can say what it
    /// searches.
    var tab: MacShellView.Tab = .chats
    /// Command F, consumed by `MacShellView`.
    var searchRequest: UUID?
    /// Set by `MacShellView` once the Chats tab is on screen; `MacRootView` puts the caret in
    /// its field and clears it.
    var chatsSearchFocusPending = false

    func file(_ request: Request) { pending = Filed(request: request) }

    func requestSearch() { searchRequest = UUID() }
}

extension FocusedValues {
    /// The key window's command state.
    @Entry var macWindowCommands: MacWindowCommands?
}

/// The Conversations menu.
struct MacConversationCommands: Commands {
    var body: some Commands {
        CommandMenu("Conversations") { MacConversationMenuItems() }
    }
}

struct MacConversationMenuItems: View {
    @FocusedValue(\.macWindowCommands) private var commands

    var body: some View {
        let chats = commands?.conversations
        Button(commands?.tab == .vault ? "Search Notes" : "Search Conversations") {
            // Finding text inside a view that has its own find bar stays with that view.
            if MacFindBarForwarding.forwardToFirstResponder() { return }
            commands?.requestSearch()
        }
        .keyboardShortcut("f", modifiers: .command)
        .disabled(commands == nil)

        Divider()

        // NO ⌘N HERE: File > New Window holds ⌘N in the menu bar (SwiftUI drops a second
        // claim), so New Chat answers ⌘N from its toolbar button, which the window asks
        // before the menu bar, exactly as before. ⌘R is on both; the button answers first.
        Button("New Chat") { commands?.file(.newChat) }
            .disabled(chats?.canStartChat != true)
        Button("Refresh") { commands?.file(.refresh) }
            .keyboardShortcut("r", modifiers: .command)
            .disabled(chats == nil)

        Divider()

        Toggle("Show Favorites", isOn: Binding(
            get: { chats?.favoritesShown == true },
            set: { _ in commands?.file(.toggleFavorites) }))
            .keyboardShortcut("f", modifiers: [.command, .shift])
            .disabled(chats == nil)
        // ⇧⌘A IS SHARED with Ops > Away Mode, and always has been: on the toolbar button it
        // archived while a conversation was selected and fell through to Away Mode
        // otherwise. A disabled menu item still swallows its shortcut, so this item carries
        // ⇧⌘A only while it can act, and the menu keeps the old behavior exactly.
        Button(chats?.selectionArchived == true ? "Unarchive Selected" : "Archive Selected") {
            commands?.file(.archiveSelected)
        }
        .keyboardShortcut(chats?.selectionArchived != nil
                              ? KeyboardShortcut("a", modifiers: [.command, .shift]) : nil)
        .disabled(chats?.selectionArchived == nil)
    }
}

/// Command F inside a text view that shows a find bar belongs to that text view. No view in
/// this app turns the find bar on today, so this forwards nothing yet; it is here so one that
/// does keeps its own Find.
enum MacFindBarForwarding {
    static func forwardToFirstResponder() -> Bool {
        guard let textView = NSApp.keyWindow?.firstResponder as? NSTextView,
              textView.usesFindBar, !textView.isFieldEditor else { return false }
        let sender = NSMenuItem()
        sender.tag = NSTextFinder.Action.showFindInterface.rawValue
        textView.performTextFinderAction(sender)
        return true
    }
}
