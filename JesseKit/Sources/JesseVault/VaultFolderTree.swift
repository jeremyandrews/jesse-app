import Foundation

// THE VAULT'S FOLDERS, DERIVED RATHER THAN STORED.
//
// The index holds one column that matters here: `files.path`, a vault relative path with
// `/` separators and no leading slash, exactly as `VaultScanner` produces it. Every folder
// in the vault, and the number of notes under each, is a function of that column — so this
// is a fold over the paths the index already has, and NOT a new table.
//
// That is a deliberate choice and worth stating, because the obvious alternative is a
// `folders` table maintained by the indexer. It would cost a schema change, and a schema
// change costs every device on the previous version a FULL REBUILD of its index (7,600
// notes re-read and re-chunked) to learn a value that can be recomputed from a column
// those devices already carry. Folding 7,600 strings in memory is microseconds; the walk
// that would replace it is minutes of somebody's morning.

/// One folder in the vault, and how many notes are under it.
public struct VaultFolderCount: Equatable, Sendable, Identifiable {
    /// The folder's vault relative path: `/` separated, no leading or trailing slash.
    public let path: String
    /// Every note under it, at ANY depth — so `Projects` counts the drafts inside
    /// `Projects/drafts/archive/` too. What a folder held "with subfolders" shows.
    public let noteCount: Int
    /// The notes DIRECTLY inside it, no deeper: what a folder held exactly shows, and
    /// the number the picker leads with, because a picker that says 915 over a folder
    /// whose own list holds 27 is describing a different folder.
    public let directCount: Int
    /// The folders directly inside it. Nonzero is what earns a row its chevron.
    public let subfolderCount: Int

    public var id: String { path }

    public init(path: String, noteCount: Int, directCount: Int, subfolderCount: Int) {
        self.path = path
        self.noteCount = noteCount
        self.directCount = directCount
        self.subfolderCount = subfolderCount
    }

    /// The folder's own name: the last component of its path.
    public var name: String { Self.name(of: path) }

    /// The folder it is inside, or nil for a folder at the vault root.
    public var parent: String? { Self.parent(of: path) }

    public static func name(of path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }

    public static func parent(of path: String) -> String? {
        guard let slash = path.lastIndex(of: "/") else { return nil }
        return String(path[..<slash])
    }
}

/// Every folder in a set of note paths, with its count. Pure.
public enum VaultFolderTree {

    /// The folders implied by `paths`, sorted and total-ordered.
    ///
    /// `paths` are note paths as the index stores them; the index holds `.md` files and
    /// nothing else, so every path counted is a note. A file at the vault ROOT belongs to
    /// no folder and contributes nothing — there is no synthetic "/" row, because the row
    /// for "everything" is the picker's own root and two of them would be one too many.
    ///
    /// ONE FOLD for all three counts: each note adds one to every folder above it and one
    /// to its own folder's direct count, and each folder, once known, adds one to its
    /// parent's subfolder count.
    public static func folders(fromPaths paths: [String]) -> [VaultFolderCount] {
        var counts: [String: Int] = [:]
        var direct: [String: Int] = [:]
        for path in paths {
            let parts = path.split(separator: "/", omittingEmptySubsequences: true)
            // The last component is the file; a path with only one component is a note at
            // the vault root.
            guard parts.count > 1 else { continue }
            var prefix = ""
            for part in parts.dropLast() {
                prefix = prefix.isEmpty ? String(part) : prefix + "/" + part
                counts[prefix, default: 0] += 1
            }
            direct[prefix, default: 0] += 1
        }
        var subfolders: [String: Int] = [:]
        for folder in counts.keys {
            if let parent = VaultFolderCount.parent(of: folder) {
                subfolders[parent, default: 0] += 1
            }
        }
        return counts
            .map { VaultFolderCount(path: $0.key, noteCount: $0.value,
                                    directCount: direct[$0.key] ?? 0,
                                    subfolderCount: subfolders[$0.key] ?? 0) }
            .sorted(by: isOrderedBefore)
    }

    /// The folders directly inside `parent` (nil for the vault root), in `folders`'
    /// order: one drill down level of the picker.
    public static func children(of parent: String?,
                                in folders: [VaultFolderCount]) -> [VaultFolderCount] {
        folders.filter { $0.parent == parent }
    }

    /// Case insensitive first, so `Projects` and `people` read as a person expects them
    /// to; case sensitive second, so the order is TOTAL and two folders differing only in
    /// case cannot swap places between two calls. `lowercased()` rather than a localized
    /// compare on purpose: the order must not depend on the device's locale, or a test
    /// passes here and fails on a phone set to Turkish.
    static func isOrderedBefore(_ a: VaultFolderCount, _ b: VaultFolderCount) -> Bool {
        let (la, lb) = (a.path.lowercased(), b.path.lowercased())
        if la != lb { return la < lb }
        return a.path < b.path
    }
}
