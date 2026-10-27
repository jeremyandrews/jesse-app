import Foundation
import os

// The BOOKMARK STORE: the one place that knows how this device holds on to the
// folder the user picked.
//
// The app has never had this. Everything vault-shaped went through the bridge
// (`GET /jesse/today`, the detail endpoint, the chat turns), so with the bridge
// unreachable not one note could be opened, even though Obsidian keeps a complete
// synced copy of the same vault on this very device. A folder picked in a document
// picker is reachable for exactly as long as that picker's URL lives unless a
// SECURITY-SCOPED BOOKMARK is taken, which is what this file takes and persists.
//
// Two platform differences, both load-bearing:
//
//   * macOS needs `.withSecurityScope` when CREATING the bookmark and
//     `.withSecurityScope` when RESOLVING it. iOS has no such option for a
//     document-picker URL — passing one there produces a bookmark that does not
//     resolve — so iOS uses the default options on both sides.
//   * `startAccessingSecurityScopedResource()` is REFERENCE COUNTED. Every start
//     must be paired with a stop or the process leaks a kernel resource per call,
//     which is why `withAccess` exists and why callers should prefer it.
//
// The value itself holds NO cached state: every call reads the bookmark back out of
// `UserDefaults`, which is what makes it a `Sendable` value usable from the main actor
// (the Settings row) and from a background task (the scanner). The one thing kept
// between calls is process wide and behind a lock: the access `resolve` holds, one per
// folder (`AccessLedger`), so asking where the folder stands neither re-resolves the
// bookmark nor starts another access every time it is asked.

/// Where this device stands with respect to the vault folder.
public enum VaultFolderStatus: Sendable {
    /// No folder has ever been picked on this device (or it was forgotten).
    case notSet
    /// A folder is held and resolved to this URL.
    case ready(URL)
    /// A bookmark exists but the folder it names is gone or has moved beyond what
    /// the bookmark can follow. The user has to pick it again.
    case stale
    /// A bookmark exists and resolving it failed for some other reason.
    ///
    /// The common cause on macOS is not corruption but IDENTITY: a security-scoped
    /// bookmark is bound to the code signature of the app that created it, so a build
    /// signed differently from the one that picked the folder cannot resolve the
    /// bookmark it left behind and reports it as malformed. Observed directly while
    /// building this: rebuilding an unsigned binary turned a working bookmark into
    /// "the file couldn't be opened because it isn't in the correct format".
    case unreadable(String)

    /// A short line for the diagnostics screen and the Settings row.
    ///
    /// Every failing state names the SAME ACTION, because it is the same action:
    /// pick the folder again. A status line on a diagnostics screen that reports a
    /// Foundation error string and leaves the reader to infer what to do is a line
    /// that has not done its job.
    public var display: String {
        switch self {
        case .notSet: return "Not set"
        case .ready(let url): return "Ready — \(url.lastPathComponent)"
        case .stale: return "Stale — pick the folder again"
        case .unreadable(let reason): return "Pick the folder again — \(reason)"
        }
    }

    /// True when the folder has to be picked again before anything can be read.
    public var needsPicking: Bool {
        switch self {
        case .ready: return false
        case .notSet, .stale, .unreadable: return true
        }
    }

    /// The resolved URL when there is one. Nil in every other state.
    public var url: URL? {
        if case .ready(let url) = self { return url }
        return nil
    }

    public var isReady: Bool { url != nil }
}

public enum VaultFolderError: Error, CustomStringConvertible {
    case noFolderHeld
    case bookmarkStale
    case accessDenied(String)

    public var description: String {
        switch self {
        case .noFolderHeld: return "No vault folder has been picked on this device."
        case .bookmarkStale: return "The saved folder bookmark is stale — pick the folder again."
        case .accessDenied(let reason): return "The folder could not be opened: \(reason)"
        }
    }
}

/// Holds one folder across relaunches and reboots, and hands it out only inside a
/// balanced access bracket.
///
/// `@unchecked Sendable` for ONE reason, named rather than waved through:
/// `UserDefaults` is documented thread-safe but is not annotated `Sendable` in the
/// SDK, so the compiler cannot see what the documentation says. Nothing else here is
/// mutable — there is no cached URL, no lazily resolved bookmark, no stored status —
/// which is what makes the claim true rather than merely asserted.
public struct VaultFolder: @unchecked Sendable {
    /// The single `UserDefaults` key. One folder per device by design: the vault is
    /// one folder, and a list of them would be a feature nobody asked for.
    public static let defaultsKey = "jesse.vault.folderBookmark"

    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = VaultFolder.defaultsKey) {
        self.defaults = defaults
        self.key = key
    }

    /// True when a bookmark is stored, whatever it currently resolves to.
    public var hasBookmark: Bool { defaults.data(forKey: key) != nil }

    /// Take a persistent bookmark to `url` — the URL a document picker or an open
    /// panel just handed over.
    ///
    /// The access bracket here is BALANCED: this method starts access only so the
    /// bookmark can be created, and stops again before returning. The caller gets
    /// live access by going through `resolve()` or `withAccess` afterwards, which is
    /// the same path every later launch takes, so the first run is not a special case
    /// that happens to work for a reason later runs do not share.
    public func adopt(url: URL) throws {
        let scoped = Self.startAccess(url)
        defer { if scoped { Self.stopAccess(url) } }
        do {
            let data = try Self.bookmarkData(for: url)
            defaults.set(data, forKey: key)
        } catch {
            throw VaultFolderError.accessDenied(error.localizedDescription)
        }
    }

    /// Rebuild the folder URL from the stored bookmark, refreshing it when the system
    /// reports it stale, and HOLD access to it.
    ///
    /// The hold lasts the life of this launch: this is the "hold the folder" call the
    /// Settings row, the diagnostics screen, the Vault tab and the Today tab use, and a
    /// stop on return would take the access away again before anything could read a
    /// file. Code that reads or writes should use `withAccess`, which is balanced; this is
    /// for asking "where do we stand".
    ///
    /// ONE HOLD PER FOLDER, NOT ONE PER CALL. This used to start access on every call and
    /// never stop it, and it is called from a body (the Today tab asks it on every
    /// evaluation), from every Vault tab appearance and from every activation, so the
    /// grants piled up for as long as the app ran, and each call also paid a bookmark
    /// resolve on the main thread. Now the first call resolves and starts access, and
    /// every later call with the same stored bookmark answers from that hold. A
    /// different bookmark (a new folder picked) releases the old hold before taking the
    /// new one, and `forget()` releases it, so every start is paired with a stop.
    public func resolve() -> VaultFolderStatus {
        guard let data = defaults.data(forKey: key) else {
            Self.ledger.release(key)
            return .notSet
        }
        if let held = Self.ledger.held(key, for: data) {
            // One stat, so a folder deleted or moved since it was held is not reported
            // ready: it falls through to a fresh resolve, which says what became of it.
            if FileManager.default.fileExists(atPath: held.url.path) { return .ready(held.url) }
            Self.ledger.release(key)
        }
        var stale = false
        let url: URL
        do {
            url = try Self.resolveBookmark(data, stale: &stale)
        } catch {
            return .unreadable(error.localizedDescription)
        }
        var heldData = data
        if stale {
            // A stale bookmark can often be re-minted from the URL it still produced.
            // If that works the user never sees the staleness; if it does not, it is
            // a real "pick it again", not a transient error.
            guard Self.startAccess(url) else { return .stale }
            defer { Self.stopAccess(url) }
            guard let refreshed = try? Self.bookmarkData(for: url) else { return .stale }
            defaults.set(refreshed, forKey: key)
            heldData = refreshed
        }
        guard Self.ledger.hold(key, data: heldData, url: url) else {
            // A folder inside the app's own container needs no security scope and
            // legitimately returns false here, so this is not automatically a failure:
            // report ready when the folder is actually reachable.
            return FileManager.default.fileExists(atPath: url.path)
                ? .ready(url)
                : .stale
        }
        return .ready(url)
    }

    /// The current status without any intent to read — the Settings row's value.
    public var status: VaultFolderStatus { resolve() }

    /// Drop the bookmark. The folder itself is untouched.
    public func forget() {
        defaults.removeObject(forKey: key)
        Self.ledger.release(key)
    }

    /// How many security scoped accesses this process has started through `VaultFolder` and
    /// not stopped. A test hook: `resolve` holds at most one per folder however often it is
    /// called, and `forget()` gives that one back.
    public static var outstandingAccessCount: Int { accessCount.withLock { $0 } }

    private static let accessCount = OSAllocatedUnfairLock(initialState: 0)

    /// Every start in this type goes through here, so `outstandingAccessCount` is the truth.
    static func startAccess(_ url: URL) -> Bool {
        let started = url.startAccessingSecurityScopedResource()
        if started { accessCount.withLock { $0 += 1 } }
        return started
    }

    /// The matching stop. Called only for a start that returned true.
    static func stopAccess(_ url: URL) {
        url.stopAccessingSecurityScopedResource()
        accessCount.withLock { $0 -= 1 }
    }

    /// The process wide record of `resolve`'s holds, keyed by defaults key.
    static let ledger = AccessLedger()

    final class AccessLedger: @unchecked Sendable {
        struct Held {
            let data: Data
            let url: URL
            /// Whether `startAccessingSecurityScopedResource` returned true, and so owes a
            /// stop. False for a folder inside the app's own container.
            let scoped: Bool
        }

        private let lock = NSLock()
        private var holds: [String: Held] = [:]

        /// The hold for `key`, if it was taken for exactly this bookmark.
        func held(_ key: String, for data: Data) -> Held? {
            lock.withLock { holds[key].flatMap { $0.data == data ? $0 : nil } }
        }

        /// Start access to `url` and record it as `key`'s hold, releasing any earlier one.
        /// Returns whether access was started (false for an unscoped folder, which is
        /// still recorded so later calls skip the resolve).
        func hold(_ key: String, data: Data, url: URL) -> Bool {
            let scoped = VaultFolder.startAccess(url)
            let previous = lock.withLock {
                let old = holds[key]
                holds[key] = Held(data: data, url: url, scoped: scoped)
                return old
            }
            if let previous, previous.scoped { VaultFolder.stopAccess(previous.url) }
            return scoped
        }

        func release(_ key: String) {
            let previous = lock.withLock { holds.removeValue(forKey: key) }
            if let previous, previous.scoped { VaultFolder.stopAccess(previous.url) }
        }
    }

    /// Run `body` with the folder URL inside a BALANCED security-scope bracket.
    ///
    /// This is the only access path that cannot leak: start and stop are paired on
    /// every exit, including a throw from `body`.
    public func withAccess<T>(_ body: (URL) throws -> T) throws -> T {
        guard let data = defaults.data(forKey: key) else { throw VaultFolderError.noFolderHeld }
        var stale = false
        let url: URL
        do {
            url = try Self.resolveBookmark(data, stale: &stale)
        } catch {
            throw VaultFolderError.accessDenied(error.localizedDescription)
        }
        if stale, Self.startAccess(url) {
            if let refreshed = try? Self.bookmarkData(for: url) {
                defaults.set(refreshed, forKey: key)
            }
            Self.stopAccess(url)
        }
        let scoped = Self.startAccess(url)
        defer { if scoped { Self.stopAccess(url) } }
        if !scoped, !FileManager.default.fileExists(atPath: url.path) {
            throw VaultFolderError.bookmarkStale
        }
        return try body(url)
    }

    // MARK: - The two platform-conditional halves

    /// Creation options. macOS requires `.withSecurityScope` for a bookmark that
    /// survives relaunch; iOS has no equivalent for a document-picker URL and a
    /// default-options bookmark is the persistent one there.
    private static func bookmarkData(for url: URL) throws -> Data {
        #if os(macOS)
        return try url.bookmarkData(options: [.withSecurityScope],
                                    includingResourceValuesForKeys: nil,
                                    relativeTo: nil)
        #else
        return try url.bookmarkData(options: [],
                                    includingResourceValuesForKeys: nil,
                                    relativeTo: nil)
        #endif
    }

    /// Resolution options, symmetric with `bookmarkData(for:)` — an asymmetry here
    /// is the classic way a bookmark resolves to a URL that cannot be opened.
    private static func resolveBookmark(_ data: Data, stale: inout Bool) throws -> URL {
        #if os(macOS)
        return try URL(resolvingBookmarkData: data,
                       options: [.withSecurityScope],
                       relativeTo: nil,
                       bookmarkDataIsStale: &stale)
        #else
        return try URL(resolvingBookmarkData: data,
                       options: [],
                       relativeTo: nil,
                       bookmarkDataIsStale: &stale)
        #endif
    }
}
