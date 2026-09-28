import SwiftUI

// THE FOLDER PICKER, drilled down rather than drawn as a tree.
//
// The vault is five folders deep with long names, and an indented tree gives up a level
// of width per depth: on a phone the fifth level is a checkbox and three letters. So this
// is the iOS pattern for a hierarchy on a small screen, the one Files and Settings use:
// one level at a time, a chevron into the next, the back button out. Selecting is a
// separate tap from drilling, because the folder a person most wants is often one with
// subfolders of its own.
//
// Typing in the search field flattens it: every folder at any depth whose path matches,
// so a deep folder is two taps away without walking to it.
//
// The picker edits a DRAFT. Done applies it and Cancel (or a swipe down) throws it away,
// which gives a clean undo and asks the index one question instead of one per tap.

/// The Vault tab's folder picker: a drill down list with a selection circle per row.
struct VaultFolderPicker: View {
    private let folders: [VaultFolderCount]
    private let byPath: [String: VaultFolderCount]
    /// Each level's rows, keyed by the parent's path, `""` for the vault root. Built once:
    /// a vault has about a thousand folders and a level is drawn on every tap.
    private let children: [String: [VaultFolderCount]]
    private let onDone: (VaultFolderSelection) -> Void
    private let onCancel: () -> Void

    @State private var draft: VaultFolderSelection
    /// The picker's own filter. Not the vault query: this one filters the list of
    /// folders by path, and it starts empty on every open, so the sheet never opens
    /// already hiding most of what it is there to show.
    @State private var filter: String
    /// The drill down path: the folders pushed above the root, outermost first.
    @State private var trail: [String]

    /// `filter` and `trail` are for a render test that needs a state a tap would reach;
    /// the screen opens the picker at the root with nothing typed.
    init(folders: [VaultFolderCount],
         selection: VaultFolderSelection,
         filter: String = "",
         trail: [String] = [],
         onDone: @escaping (VaultFolderSelection) -> Void,
         onCancel: @escaping () -> Void) {
        self.folders = folders
        self.byPath = Dictionary(folders.map { ($0.path, $0) }, uniquingKeysWith: { a, _ in a })
        self.children = Dictionary(grouping: folders) { $0.parent ?? "" }
        self.onDone = onDone
        self.onCancel = onCancel
        _draft = State(initialValue: selection)
        _filter = State(initialValue: filter)
        _trail = State(initialValue: trail)
    }

    var body: some View {
        NavigationStack(path: $trail) {
            level(nil)
                .navigationDestination(for: String.self) { level($0) }
        }
        #if os(macOS)
        // Sized to fit a Mac window rather than to fill a screen: the sheet is the
        // window's child, and one taller than the window runs off its bottom edge.
        .frame(minWidth: 380, idealWidth: 460, maxWidth: 640,
               minHeight: 360, idealHeight: 520, maxHeight: 720)
        #endif
    }

    private var trimmedFilter: String {
        filter.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// One level of the drill down, or the flat search list while something is typed.
    private func level(_ parent: String?) -> some View {
        List {
            if !trimmedFilter.isEmpty {
                let matches = folders.filter { $0.path.localizedCaseInsensitiveContains(trimmedFilter) }
                if matches.isEmpty {
                    Text("No folder matches \u{201C}\(trimmedFilter)\u{201D}.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                ForEach(matches) { folder in
                    row(folder, name: folder.path, isFlat: true)
                }
            } else {
                // The folder itself, first on every level below the root, so a folder
                // with subfolders can be chosen from inside it as well as from above.
                if let parent, let this = byPath[parent] {
                    row(this, name: "This folder", isThisFolder: true)
                }
                ForEach(children[parent ?? ""] ?? []) { folder in
                    row(folder, name: folder.name)
                }
            }
        }
        #if os(macOS)
        .listStyle(.inset)
        #else
        .listStyle(.plain)
        #endif
        .navigationTitle(parent.map(VaultFolderCount.name(of:)) ?? "Folders")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .searchable(text: $filter, prompt: "Filter folders")
        .toolbar {
            // Cancel on the root only: below it the leading slot is the back button, and
            // a swipe down cancels from any level.
            if parent == nil {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: onCancel)
                }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { onDone(draft) }
                    .fontWeight(.semibold)
            }
        }
        .safeAreaInset(edge: .bottom) { selectionBar }
    }

    /// "N selected" and Clear, pinned under every level.
    private var selectionBar: some View {
        HStack {
            Text(Self.selectedSentence(draft.count))
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Button("Clear") { draft.removeAll() }
                .disabled(draft.isEmpty)
                .frame(minHeight: 44)
                .accessibilityLabel("Clear the selection")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 4)
        // Capped: at the largest accessibility sizes an uncapped bar is a quarter of a
        // small phone's screen, taken from the list it is there to serve.
        .dynamicTypeSize(...DynamicTypeSize.accessibility1)
        .background(.bar)
    }

    /// One folder: the selection circle, its name and counts, and a chevron into it when
    /// it has subfolders. Two buttons, so selecting and drilling are separate targets.
    private func row(_ folder: VaultFolderCount, name: String,
                     isFlat: Bool = false, isThisFolder: Bool = false) -> some View {
        let entry = draft.entry(for: folder.path)
        let isSelected = entry != nil
        let widened = entry?.includesSubfolders ?? false
        let showsChevron = !isFlat && !isThisFolder && folder.subfolderCount > 0
        let counts = Self.countsLine(folder, includesSubfolders: widened,
                                     showsSubfolders: !isThisFolder)
        return HStack(spacing: 0) {
            Button {
                draft.toggle(folder.path)
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                        .font(.title3)
                        .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(name)
                            .foregroundStyle(.primary)
                            .lineLimit(isFlat ? 3 : 2)
                            .truncationMode(isFlat ? .head : .tail)
                        Text(counts)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 8)
                }
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(folder.path)
            .accessibilityLabel(isThisFolder ? "This folder, \(folder.name), \(counts)"
                                             : "\(name), \(counts)")
            .accessibilityAddTraits(isSelected ? [.isSelected] : [])
            .accessibilityActions {
                if folder.subfolderCount > 0 {
                    Button(widened ? "This folder only" : "Include subfolders") {
                        draft.setIncludesSubfolders(!widened, for: folder.path)
                    }
                }
            }

            if showsChevron {
                Button {
                    trail.append(folder.path)
                } label: {
                    Image(systemName: "chevron.right")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .frame(minWidth: 44, minHeight: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .help("Open \(folder.path)")
                .accessibilityLabel("Open \(folder.name)")
            }
        }
        .contextMenu {
            if folder.subfolderCount > 0 {
                Button {
                    draft.setIncludesSubfolders(!widened, for: folder.path)
                } label: {
                    Label(widened ? "This folder only" : "Include subfolders",
                          systemImage: widened ? "folder" : "folder.badge.plus")
                }
            }
            if isSelected {
                Button(role: .destructive) {
                    draft.remove(folder.path)
                } label: {
                    Label("Deselect", systemImage: "xmark.circle")
                }
            }
        }
    }

    /// The row's secondary line: its own notes, its subfolders, and when it is held with
    /// its subfolders, how many notes that brings in.
    nonisolated static func countsLine(_ folder: VaultFolderCount, includesSubfolders: Bool,
                                       showsSubfolders: Bool = true) -> String {
        var parts = [plural(folder.directCount, "note")]
        if showsSubfolders, folder.subfolderCount > 0 {
            parts.append(plural(folder.subfolderCount, "folder"))
        }
        var line = parts.joined(separator: ", ")
        if includesSubfolders {
            line += ", \(plural(folder.noteCount, "note")) with subfolders"
        }
        return line
    }

    nonisolated static func selectedSentence(_ count: Int) -> String {
        count == 0 ? "No folder selected" : "\(count) selected"
    }

    nonisolated static func plural(_ count: Int, _ noun: String) -> String {
        count == 1 ? "1 \(noun)" : "\(count) \(noun)s"
    }
}
