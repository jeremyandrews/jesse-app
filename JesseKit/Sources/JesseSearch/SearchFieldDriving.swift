import SwiftUI

/// The conversation search field and the changes that drive `ThreadSearchModel` from it,
/// shared by the iPhone list and the Mac sidebar: gaining focus builds the index and
/// warms the model, a keystroke feeds the query, the Settings toggle re-drives it, and a
/// reply landing (the newest thread's stamp moves) or a thread added or deleted while a
/// search shows re-runs the pass, which rebuilds only what changed.
///
/// A modifier of its own so the screens' `body` stays within what the type checker can
/// solve, and so both platforms wire the field identically.
///
/// WHO DRAWS THE FIELD. By default this modifier does, with `.searchable`, which is what
/// the iPhone uses. The Mac draws its own field in the sidebar column and passes
/// `presentsField: false`: inside the window's `TabView`, a `.searchable` on the sidebar
/// is not drawn in the sidebar at all but becomes a principal toolbar search item, which
/// cannot share the toolbar with the tab picker and takes the whole sidebar section of
/// the toolbar into the overflow menu with it. Either way the wiring below is the same.
public struct SearchFieldDriving: ViewModifier {
    /// The field's placeholder, on both platforms.
    public static let prompt = "Search conversations"

    @Binding var text: String
    let placement: SearchFieldPlacement
    let presentsField: Bool
    /// The caller's view of the field's focus, so a command can put the caret in it. Nil
    /// when the caller has no use for it (the iPhone).
    let focus: Binding<Bool>?
    let threadCount: Int
    let newestStamp: Date?
    let expansionEnabled: Bool
    let onFocus: () -> Void
    let onQuery: (String) -> Void
    let onThreadsChanged: () -> Void

    @FocusState private var focused: Bool

    public init(text: Binding<String>, placement: SearchFieldPlacement = .automatic,
                presentsField: Bool = true, focus: Binding<Bool>? = nil,
                threadCount: Int, newestStamp: Date?, expansionEnabled: Bool,
                onFocus: @escaping () -> Void,
                onQuery: @escaping (String) -> Void,
                onThreadsChanged: @escaping () -> Void) {
        self._text = text
        self.placement = placement
        self.presentsField = presentsField
        self.focus = focus
        self.threadCount = threadCount
        self.newestStamp = newestStamp
        self.expansionEnabled = expansionEnabled
        self.onFocus = onFocus
        self.onQuery = onQuery
        self.onThreadsChanged = onThreadsChanged
    }

    /// Whether the field has the caret, whoever draws it.
    private var isFocused: Bool {
        presentsField ? focused : (focus?.wrappedValue ?? false)
    }

    public func body(content: Content) -> some View {
        field(content)
            .onChange(of: isFocused) { _, nowFocused in
                if nowFocused { onFocus() }
            }
            .onChange(of: text) { _, newValue in onQuery(newValue) }
            .onChange(of: threadCount) { _, _ in onThreadsChanged() }
            .onChange(of: newestStamp) { _, _ in onThreadsChanged() }
            // Toggling the tier re-drives the model (which drops its terms when off).
            .onChange(of: expansionEnabled) { _, _ in onQuery(text) }
    }

    @ViewBuilder
    private func field(_ content: Content) -> some View {
        if presentsField {
            content
                .searchable(text: $text, placement: placement, prompt: Self.prompt)
                .searchFocused($focused)
                // The caller's binding and the field's own focus, kept in step both ways.
                // With no binding (the iPhone) neither change has anything to do.
                .onChange(of: focus?.wrappedValue ?? false) { _, wanted in
                    if focus != nil, wanted != focused { focused = wanted }
                }
                .onChange(of: focused) { _, nowFocused in
                    if let focus, focus.wrappedValue != nowFocused { focus.wrappedValue = nowFocused }
                }
        } else {
            content
        }
    }
}
