import SwiftUI

/// Settings › Vault folder › Owner name: the field that writes `OfflineLookupSettings.ownerNameKey`.
///
/// ONE VIEW, TWO SCREENS. The iPhone's `SettingsView` and the Mac's `MacSettingsView` both
/// embed it in their Vault folder section, directly under the on-device answer toggle. It
/// lives here rather than in either app because the only setter for the key used to live
/// in `PromptStore`, which the Mac target cannot see, and so neither screen had a field:
/// on 2026-10-21 an offline answer told the owner to set a name "in Settings", and there was
/// nowhere to set it.
///
/// Two rows: the field, and a caption row under it. A ROW, not a section `footer:` — a long
/// footer ellipsises to one line on macOS, and the caption is what says the field takes
/// several forms.
///
/// WRITTEN WHEN EDITING ENDS — on submit, when the field loses focus, and when the view
/// disappears — straight through `OfflineLookupSettings.ownerName`'s setter, the way the
/// toggle beside it writes straight to `OfflineLookupSettings`, not through the screen's
/// Save button. The write is `commit(_:to:)`, so a test can drive it without a window.
public struct OwnerNameSettingsRows: View {
    nonisolated public static let label = "Owner name"
    nonisolated public static let prompt = "Jeremy, Jeremiah, Andrews"
    nonisolated public static let caption = "How your notes name you. Separate several forms with commas, "
        + "the one you go by first. On-device answers use it for questions about \"me\" and "
        + "\"my\"."

    private let settings: OfflineLookupSettings
    @State private var text: String
    @FocusState private var focused: Bool

    public init(settings: OfflineLookupSettings = OfflineLookupSettings()) {
        self.settings = settings
        _text = State(initialValue: settings.ownerName ?? "")
    }

    /// Store `text` through the one setter, and answer what the field should now show:
    /// the stored, trimmed value, or "" when the key was removed.
    @discardableResult
    nonisolated public static func commit(_ text: String, to settings: OfflineLookupSettings) -> String {
        settings.ownerName = text
        return settings.ownerName ?? ""
    }

    public var body: some View {
        field
            .onAppear { text = settings.ownerName ?? "" }
            .onDisappear(perform: store)
        Text(Self.caption)
            #if os(macOS)
            .font(.caption)
            #else
            .font(.footnote)
            #endif
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// On the Mac a `Form` row puts a text field's title to its left, as it does for every
    /// other field in that window. On the iPhone a `Form` hides a text field's title and
    /// shows only the prompt, which would leave a filled field with no name at all; the
    /// title goes on its own line above the field instead, so the field keeps the row's
    /// full width for several forms.
    @ViewBuilder
    private var field: some View {
        #if os(macOS)
        textField
        #else
        VStack(alignment: .leading, spacing: 4) {
            Text(Self.label)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            textField
        }
        #endif
    }

    private var textField: some View {
        TextField(Self.label, text: $text, prompt: Text(Self.prompt))
            .focused($focused)
            .autocorrectionDisabled()
            #if os(iOS)
            .textContentType(.name)
            .textInputAutocapitalization(.words)
            #endif
            .onSubmit(store)
            .onChange(of: focused) { _, isFocused in
                if !isFocused { store() }
            }
    }

    private func store() {
        text = Self.commit(text, to: settings)
    }
}
