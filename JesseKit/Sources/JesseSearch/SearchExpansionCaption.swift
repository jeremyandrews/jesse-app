import SwiftUI

/// The line under the search field that says what the on-device tier is doing, shared
/// by the iPhone list and the Mac sidebar.
///
/// While the model works it reads "Expanding search…"; once terms land it names them
/// ("Also searching: span, overpass"), which is what explains a row that contains none
/// of the typed words. When the tier is off, the model is unavailable, or the query is
/// too short to expand, it shows nothing: Settings is where those states are explained.
public struct SearchExpansionCaption: View {
    private let model: ThreadSearchModel
    private let searchActive: Bool

    public init(model: ThreadSearchModel, searchActive: Bool) {
        self.model = model
        self.searchActive = searchActive
    }

    public var body: some View {
        if searchActive && model.result.isActive && !model.result.terms.isEmpty {
            Text("Also searching: \(model.result.terms.joined(separator: ", "))")
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else if searchActive && model.isExpanding {
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text("Expanding search…")
            }
            .font(.caption)
            .foregroundStyle(.tertiary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        }
    }
}

/// The Settings footer for the expansion toggle: why the tier is idle, when it is.
public enum SearchExpansionStatus {
    /// Nil when the tier is on and the model is ready; otherwise one sentence.
    public static func explanation(enabled: Bool,
                                   availability: QueryExpansionAvailability) -> String? {
        guard enabled else {
            return "Off. Search matches only the words you type."
        }
        if case .unavailable(let reason) = availability {
            return "Not running: \(reason) Search matches only the words you type."
        }
        return nil
    }
}
