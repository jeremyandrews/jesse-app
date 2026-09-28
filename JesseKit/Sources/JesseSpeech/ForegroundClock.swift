import Foundation

// Time that counts only while the app is in front of its owner.
//
// A Studio run is followed by polling, and a poll that goes unanswered is "lost contact".
// On a phone, the commonest reason a poll goes unanswered is not the network at all: iOS
// suspended the app because the owner locked the screen or switched away. Nothing is lost
// in that case, the Studio is still working, and the run must not be given up on because
// a suspended process could not ask about it. So the transcriber measures silence on THIS
// clock, which stands still while the app is in the background, and the lost-contact
// tolerance means "this long in the foreground without an answer".
//
// It is also the gate for this device's own engine, which needs the foreground: when the
// fallback is needed while the app is in the background, the run waits here for the owner
// to come back rather than failing.

/// What the transcriber needs to know about the app's place on screen.
public protocol ForegroundGating: Sendable {
    /// Seconds of foreground time, monotonic. Stands still while the app is in the background.
    func now() -> TimeInterval
    /// Whether the app is in the foreground right now.
    var isActive: Bool { get }
    /// Returns at once if the app is in the foreground, otherwise when it next comes back.
    func waitUntilActive() async
}

/// The app's foreground clock, told by the app when it enters and leaves the background.
///
/// A lock rather than an actor because `now()` is read synchronously from inside the
/// transcriber's polling loop, and the app flips it from a UIKit notification.
public final class ForegroundClock: ForegroundGating, @unchecked Sendable {
    private let lock = NSLock()
    private var accumulated: TimeInterval = 0
    private var activeSince: TimeInterval?
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private let uptime: @Sendable () -> TimeInterval

    public init(active: Bool = true,
                uptime: @escaping @Sendable () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.uptime = uptime
        self.activeSince = active ? uptime() : nil
    }

    /// The app's one clock. Starts active; the app corrects it at launch, before any run
    /// can start or resume, when it was launched into the background.
    public static let shared = ForegroundClock()

    public func now() -> TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        return accumulated + (activeSince.map { uptime() - $0 } ?? 0)
    }

    public var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return activeSince != nil
    }

    /// The app entered the foreground (`true`) or the background (`false`).
    public func setActive(_ active: Bool) {
        lock.lock()
        let t = uptime()
        var resume: [CheckedContinuation<Void, Never>] = []
        if active, activeSince == nil {
            activeSince = t
            resume = waiters
            waiters = []
        } else if !active, let since = activeSince {
            accumulated += t - since
            activeSince = nil
        }
        lock.unlock()
        for waiter in resume { waiter.resume() }
    }

    public func waitUntilActive() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            if activeSince != nil {
                lock.unlock()
                continuation.resume()
                return
            }
            waiters.append(continuation)
            lock.unlock()
        }
    }
}

/// Always in the foreground, on the uptime clock: the Mac, and every caller that has no
/// background to speak of. Exactly the behaviour before the foreground clock existed.
public struct AlwaysForeground: ForegroundGating {
    private let clock: @Sendable () -> TimeInterval

    public init(now: @escaping @Sendable () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.clock = now
    }

    public func now() -> TimeInterval { clock() }
    public var isActive: Bool { true }
    public func waitUntilActive() async {}
}
