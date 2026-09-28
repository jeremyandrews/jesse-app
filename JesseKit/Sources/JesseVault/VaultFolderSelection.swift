import Foundation

// WHICH FOLDERS THE VAULT TAB IS LOOKING AT, as one value that answers the question twice.
//
// The tab used to hold one folder and treat it as a PREFIX, so `Projects/Research` meant
// every note at any depth under it. On a vault whose `Projects/Research/` holds 27 live
// reports and whose `Projects/Research/archive/` holds 887 finished ones, that prefix
// ordered by mtime showed the archive: the newest thirty notes "in" the folder were
// almost all in a folder beneath it.
//
// So a held folder is now EXACT by default (the notes directly inside it), several can be
// held at once, and each one can be widened back to "and everything beneath it". The same
// question is asked in two places — SQL, where the `LIMIT` is, and Swift, where a hit is
// checked on the way out — and a predicate written twice is two predicates that drift. So
// both halves live here, on the one type, and a test proves they agree over every path.

/// **Finished work**: a note anywhere under a directory called `archive`.
///
/// The one rule, for every caller that separates live notes from archived ones: the
/// offline answerer demotes them, the Vault tab folds them under their own header.
public enum VaultArchive {

    /// The directory name that means finished work, anywhere in a path.
    public static let segment = "archive"

    /// Whether `path` is under an `archive` directory at any depth.
    ///
    /// A PATH SEGMENT, not a substring: a note called `Archive-Policy.md` is not archived,
    /// and neither is `Workshop/archive.md`. Only directory components count, folded for
    /// case because the same folder is spelled both ways across a vault this old.
    public static func isArchived(_ path: String) -> Bool {
        path.split(separator: "/").dropLast().contains {
            $0.caseInsensitiveCompare(segment) == .orderedSame
        }
    }

    /// The same test as SQL over `column`. SQLite's `LIKE` folds ASCII case, which is
    /// `isArchived`'s folding for a segment spelled in ASCII. The patterns carry no
    /// caller data, so nothing is bound and nothing needs escaping.
    static func sql(_ column: String) -> String {
        "(\(column) LIKE '\(segment)/%' OR \(column) LIKE '%/\(segment)/%')"
    }
}

/// Which side of the archive line a query wants.
public enum VaultArchiveFilter: Sendable, Equatable {
    /// Only notes outside every `archive` directory.
    case live
    /// Only notes inside one.
    case archived
    /// Both.
    case any

    /// Whether one path is on the wanted side.
    public func includes(_ path: String) -> Bool {
        switch self {
        case .live: return !VaultArchive.isArchived(path)
        case .archived: return VaultArchive.isArchived(path)
        case .any: return true
        }
    }

    /// The SQL condition over `column`, or nil for no condition.
    func sql(_ column: String) -> String? {
        switch self {
        case .live: return "NOT \(VaultArchive.sql(column))"
        case .archived: return VaultArchive.sql(column)
        case .any: return nil
        }
    }
}

/// **The folders the Vault tab is narrowed to.** Empty means the whole vault.
///
/// An ordered set: the order is the order they were picked, which is the order the chips
/// show, and a folder is held at most once.
public struct VaultFolderSelection: Equatable, Hashable, Sendable {

    /// One held folder.
    public struct Entry: Equatable, Hashable, Sendable, Identifiable {
        /// Vault relative, `/` separated, no leading or trailing slash, as
        /// `VaultFolderTree` produces it.
        public let path: String
        /// False: only the notes directly inside `path`. True: every note at any depth.
        public var includesSubfolders: Bool

        public var id: String { path }

        public init(path: String, includesSubfolders: Bool = false) {
            self.path = path
            self.includesSubfolders = includesSubfolders
        }

        /// The folder's own name, the last component of its path.
        public var name: String { VaultFolderCount.name(of: path) }

        /// Whether one note path is in this entry.
        public func includes(_ notePath: String) -> Bool {
            let prefix = path + "/"
            guard notePath.hasPrefix(prefix) else { return false }
            return includesSubfolders || !notePath.dropFirst(prefix.count).contains("/")
        }
    }

    public private(set) var entries: [Entry]

    public init(_ entries: [Entry] = []) {
        var seen = Set<String>()
        self.entries = entries.filter { seen.insert($0.path).inserted }
    }

    /// One folder, exact.
    public init(folder: String) {
        self.init([Entry(path: folder)])
    }

    public var isEmpty: Bool { entries.isEmpty }
    public var count: Int { entries.count }
    public var paths: [String] { entries.map(\.path) }

    public func contains(_ folder: String) -> Bool {
        entries.contains { $0.path == folder }
    }

    public func entry(for folder: String) -> Entry? {
        entries.first { $0.path == folder }
    }

    /// Hold `folder`, or change how it is held if it already is.
    public mutating func insert(_ folder: String, includesSubfolders: Bool = false) {
        if let at = entries.firstIndex(where: { $0.path == folder }) {
            entries[at].includesSubfolders = includesSubfolders
        } else {
            entries.append(Entry(path: folder, includesSubfolders: includesSubfolders))
        }
    }

    public mutating func remove(_ folder: String) {
        entries.removeAll { $0.path == folder }
    }

    /// Hold it exact if it is not held; let it go if it is.
    public mutating func toggle(_ folder: String) {
        if contains(folder) { remove(folder) } else { insert(folder) }
    }

    /// Widen or narrow a held folder. Holds it first when it is not held, because asking
    /// for a folder's subfolders is asking for the folder.
    public mutating func setIncludesSubfolders(_ value: Bool, for folder: String) {
        insert(folder, includesSubfolders: value)
    }

    public mutating func removeAll() { entries = [] }

    /// **Whether one note path is in the selection.** Every path is when nothing is held.
    public func includes(_ notePath: String) -> Bool {
        isEmpty || entries.contains { $0.includes(notePath) }
    }

    /// **The same predicate as SQL**, over `column`, with the values to bind in order, or
    /// nil when nothing is held.
    ///
    /// Per entry: the folder's prefix as an escaped `LIKE`, then an exact case compare of
    /// the same prefix, then (when exact) a `NOT LIKE` for anything a directory deeper.
    /// The case compare is there because SQLite's `LIKE` folds ASCII case and Swift's
    /// `hasPrefix` does not, and `Apple/` and `apple/` are two folders to a vault synced
    /// from a case sensitive disk. Every value is bound; none is spliced into the SQL.
    public func sql(_ column: String) -> (clause: String, arguments: [String])? {
        guard !isEmpty else { return nil }
        var clauses: [String] = []
        var arguments: [String] = []
        for entry in entries {
            let prefix = entry.path + "/"
            var clause = "(\(column) LIKE ? ESCAPE '\\' AND substr(\(column), 1, length(?)) = ?"
            arguments += [VaultIndex.likePrefix(prefix), prefix, prefix]
            if !entry.includesSubfolders {
                clause += " AND \(column) NOT LIKE ? ESCAPE '\\'"
                arguments.append(VaultIndex.likePrefix(prefix) + "/%")
            }
            clauses.append(clause + ")")
        }
        return ("(" + clauses.joined(separator: " OR ") + ")", arguments)
    }
}

/// **Everything that narrows one index query**, as SQL and as Swift from one value.
///
/// The old `underPrefix` narrowing (the Strands scope, one strand's path) and the Vault
/// tab's folder selection and archive side, ANDed. Every condition goes into the query
/// itself, for the reason `VaultIndex.recentFiles(limit:underPrefix:)` gives: a `LIMIT`
/// applied before a Swift side filter answers a narrow question with a wide top N.
public struct VaultPathFilter: Sendable, Equatable {
    /// A plain path prefix, matched as `underPrefix` always has been, or nil.
    public var prefix: String?
    public var folders: VaultFolderSelection
    public var archive: VaultArchiveFilter

    public init(prefix: String? = nil, folders: VaultFolderSelection = VaultFolderSelection(),
                archive: VaultArchiveFilter = .any) {
        self.prefix = prefix
        self.folders = folders
        self.archive = archive
    }

    public static let everything = VaultPathFilter()

    public func includes(_ path: String) -> Bool {
        if let prefix, !path.hasPrefix(prefix) { return false }
        return folders.includes(path) && archive.includes(path)
    }

    /// The conditions ANDed, and the values to bind, or nil for no condition at all.
    func sql(_ column: String) -> (clause: String, arguments: [String])? {
        var clauses: [String] = []
        var arguments: [String] = []
        if let prefix {
            clauses.append("\(column) LIKE ? ESCAPE '\\'")
            arguments.append(VaultIndex.likePrefix(prefix))
        }
        if let selection = folders.sql(column) {
            clauses.append(selection.clause)
            arguments += selection.arguments
        }
        if let archived = archive.sql(column) { clauses.append(archived) }
        guard !clauses.isEmpty else { return nil }
        return (clauses.joined(separator: " AND "), arguments)
    }
}
