import SwiftUI
import AppKit

/// The Chats sidebar's search field: a real `NSSearchField`, drawn at the top of the sidebar
/// column rather than in the window toolbar.
///
/// WHY NOT `.searchable`. This window's content is a `TabView`, and inside one a sidebar
/// `.searchable` is not drawn in the sidebar. SwiftUI turns it into a principal toolbar search
/// item, which cannot be laid out beside the tab picker; it goes into the toolbar's overflow
/// menu and takes every other item in the sidebar section with it, at every window and sidebar
/// width. No placement draws it in the sidebar column (`.toolbar` and `.toolbarPrincipal` make
/// the same principal item, `.automatic` puts it over the detail column), so the field is
/// drawn here, bound to the same text and focus that `SearchFieldDriving` drives the shared
/// search from. `MacSearchVisibilityTests` pins where it lands.
struct MacSidebarSearchField: NSViewRepresentable {
    @Binding var text: String
    /// Whether the caret is in the field. Set it to put the caret there; the field sets it
    /// back as focus comes and goes.
    @Binding var isFocused: Bool
    var prompt: String

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> FocusReportingSearchField {
        let field = FocusReportingSearchField()
        field.placeholderString = prompt
        field.delegate = context.coordinator
        field.target = context.coordinator
        field.action = #selector(Coordinator.searchChanged(_:))
        field.sendsSearchStringImmediately = true
        field.sendsWholeSearchString = false
        field.controlSize = .regular
        field.setAccessibilityLabel(prompt)
        field.onFocusChange = { [weak coordinator = context.coordinator] focused in
            coordinator?.focusChanged(focused)
        }
        field.stringValue = text
        return field
    }

    func updateNSView(_ field: FocusReportingSearchField, context: Context) {
        context.coordinator.parent = self
        if field.placeholderString != prompt { field.placeholderString = prompt }
        if field.stringValue != text { field.stringValue = text }
        if isFocused && !field.hasCaret { field.takeCaret() }
    }

    final class Coordinator: NSObject, NSSearchFieldDelegate {
        var parent: MacSidebarSearchField

        init(_ parent: MacSidebarSearchField) { self.parent = parent }

        func controlTextDidChange(_ note: Notification) {
            guard let field = note.object as? NSSearchField else { return }
            push(field.stringValue)
        }

        /// The clear button and Escape arrive as the field's action, not as a text change.
        @objc func searchChanged(_ sender: NSSearchField) { push(sender.stringValue) }

        private func push(_ value: String) {
            if parent.text != value { parent.text = value }
        }

        func focusChanged(_ focused: Bool) {
            // Out of the view update that caused it: AppKit reports focus from inside
            // `makeFirstResponder`, which `updateNSView` may itself have called.
            DispatchQueue.main.async { [weak self] in
                guard let self, self.parent.isFocused != focused else { return }
                self.parent.isFocused = focused
            }
        }
    }
}

/// An `NSSearchField` that says when the caret arrives and leaves, which `NSSearchField`
/// alone reports only once a key is typed.
final class FocusReportingSearchField: NSSearchField {
    var onFocusChange: ((Bool) -> Void)?
    /// Asked for before the field had a window; honored when it gets one.
    private var wantsCaret = false

    /// Whether the field editor is editing this field right now.
    var hasCaret: Bool {
        guard let editor = window?.firstResponder as? NSText else { return false }
        return editor.delegate === self
    }

    func takeCaret() {
        guard let window else { wantsCaret = true; return }
        wantsCaret = false
        caretAsked = true
        window.makeFirstResponder(self)
        caretAsked = false
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if wantsCaret, window != nil { takeCaret() }
    }

    /// The caret comes here only when someone asks: a click, Tab, or Command F. The window
    /// otherwise hands it to the first view that takes it as soon as it appears, and this
    /// field, at the top of the sidebar, would be that view, so the window would open with
    /// typing going into search.
    override func becomeFirstResponder() -> Bool {
        guard caretAsked || Self.isUserGesture(NSApp.currentEvent) else { return false }
        caretAsked = false
        let became = super.becomeFirstResponder()
        if became { onFocusChange?(true) }
        return became
    }

    private var caretAsked = false

    private static func isUserGesture(_ event: NSEvent?) -> Bool {
        switch event?.type {
        case .leftMouseDown, .rightMouseDown, .otherMouseDown, .keyDown: true
        default: false
        }
    }

    override func textDidEndEditing(_ notification: Notification) {
        super.textDidEndEditing(notification)
        onFocusChange?(false)
    }
}
