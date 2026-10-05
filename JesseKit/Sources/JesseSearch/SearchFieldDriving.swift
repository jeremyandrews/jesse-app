import SwiftUI

/// The conversation search field and the changes that drive `ThreadSearchModel` from it,
/// shared by the iPhone list and the Mac sidebar: gaining focus builds the index and
/// warms the model, a keystroke feeds the query, the Settings toggle re-drives it, and a
/// reply landing (the newest thread's stamp moves) or a thread added or deleted while a
/// search shows re-runs the pass, which rebuilds only what changed.
///
/// A modifier of its own so the screens' `body` stays within what the type checker can
/// solve, and so both platforms wire the field identically.
public struct SearchFieldDriving: ViewModifier {
    @Binding var text: String
    let placement: SearchFieldPlacement
    let threadCount: Int
    let newestStamp: Date?
    let expansionEnabled: Bool
    let onFocus: () -> Void
    let onQuery: (String) -> Void
    let onThreadsChanged: () -> Void

    @FocusState private var focused: Bool

    public init(text: Binding<String>, placement: SearchFieldPlacement = .automatic,
                threadCount: Int, newestStamp: Date?, expansionEnabled: Bool,
                onFocus: @escaping () -> Void,
                onQuery: @escaping (String) -> Void,
                onThreadsChanged: @escaping () -> Void) {
        self._text = text
        self.placement = placement
        self.threadCount = threadCount
        self.newestStamp = newestStamp
        self.expansionEnabled = expansionEnabled
        self.onFocus = onFocus
        self.onQuery = onQuery
        self.onThreadsChanged = onThreadsChanged
    }

    public func body(content: Content) -> some View {
        content
            .searchable(text: $text, placement: placement, prompt: "Search conversations")
            .searchFocused($focused)
            .onChange(of: focused) { _, isFocused in
                if isFocused { onFocus() }
            }
            .onChange(of: text) { _, newValue in onQuery(newValue) }
            .onChange(of: threadCount) { _, _ in onThreadsChanged() }
            .onChange(of: newestStamp) { _, _ in onThreadsChanged() }
            // Toggling the tier re-drives the model (which drops its terms when off).
            .onChange(of: expansionEnabled) { _, _ in onQuery(text) }
    }
}
