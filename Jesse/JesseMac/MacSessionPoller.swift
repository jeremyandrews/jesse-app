import SwiftUI
import AppKit
import SwiftData
import JesseNetworking

// Keeping the Mac's conversation list current without a reload click.
//
// The phone looks current because iOS suspends it: every return to the foreground swings
// `scenePhase` and pulls the list. A Mac window left open never leaves `.active`, so before this
// the sidebar was pulled at launch, on a wake, and on ⌘R, and a conversation started on the
// phone sat invisible on the desk until someone pressed reload.
//
// So the Mac polls, cheaply: every tick is the existing `MacCoordinator.refreshSessions`, ETag
// conditioned, so an unchanged list is a `304` with no body and no store writes. The cadence is
// the pure `MacSessionPollRule` below, which is what the tests assert; `MacSessionPoller` is the
// loop that follows it, and `macSessionPolling()` is where a window attaches it.
//
// DOUBLE FIRING. Every AUTOMATIC trigger (launch, a return to `.active`, a wake, the app becoming
// frontmost) goes through `refreshSessionsUnlessRecent`, which drops a second call that lands
// within `MacSessionPollRule.debounce` of the last one's start. The poller measures its own
// cadence from that same start, so a refresh from any trigger pushes the next tick back by a
// whole interval. What is left (a click during a pull) is the coordinator's re-entrancy guard.

/// The timing rule, pure, so the cadence is asserted rather than assumed.
enum MacSessionPollRule {
    /// Between pulls while Jesse is the frontmost app: a conversation started elsewhere shows up
    /// within about this long.
    static let frontmostInterval: TimeInterval = 30
    /// Between pulls while Jesse is running but another app is frontmost. Activation pulls at
    /// once, so this only bounds how stale the list can be the moment the window is looked at.
    static let backgroundInterval: TimeInterval = 300
    /// The ceiling the backoff grows to on consecutive failures.
    static let backoffCap: TimeInterval = 300
    /// Two automatic refreshes starting closer together than this are one refresh.
    static let debounce: TimeInterval = 2

    /// The delay before the next list pull, or nil for "do not poll".
    ///
    /// Nil while the Mac is asleep and while the bridge is KNOWN unreachable (`.unknown` is a
    /// cold launch before the first probe, and polls). Otherwise the base interval for the
    /// app's activation state, doubled per consecutive failure and capped at `backoffCap`; a
    /// completed round trip resets `failures` to zero, which resets the cadence.
    static func nextDelay(appActive: Bool, reachability: BridgeReachability,
                          failures: Int, asleep: Bool = false) -> TimeInterval? {
        guard !asleep, reachability != .unreachable else { return nil }
        let base = appActive ? frontmostInterval : backgroundInterval
        // Bounded exponent: past 2^10 the cap has long since won, and an unbounded shift is
        // how a counter that reached 64 would wrap.
        let factor = pow(2, Double(min(max(failures, 0), 10)))
        return min(base * factor, max(base, backoffCap))
    }

    /// How long is still left of `delay`, measured from the most recent pull. Nil `since` means
    /// nothing has been pulled yet, so the whole delay remains.
    static func remaining(delay: TimeInterval, since last: Date?, now: Date) -> TimeInterval {
        guard let last else { return delay }
        return max(0, last.addingTimeInterval(delay).timeIntervalSince(now))
    }

    /// Whether an automatic refresh asked for at `now` lands inside the debounce window of the
    /// last one's start. A clock that went backwards is not "recent".
    static func isDebounced(lastStart: Date?, now: Date) -> Bool {
        guard let lastStart else { return false }
        let gap = now.timeIntervalSince(lastStart)
        return gap >= 0 && gap < debounce
    }
}

/// The loop that follows `MacSessionPollRule`: one per window, started by the window's `.task`
/// and so cancelled with it. Structured concurrency, not a `Timer`: the wait is a `Task.sleep`
/// that a nudge (activation, reachability, sleep, wake) cuts short so the rule is re-read at once.
@MainActor
final class MacSessionPoller {
    // A test host may release this off the main actor; same pattern as the dashboard models.
    nonisolated deinit {}

    /// What the loop reads and does. Closures rather than the singletons themselves, so a test
    /// drives the loop with nothing real behind it.
    struct Inputs {
        var isAppActive: @MainActor () -> Bool
        var reachability: @MainActor () -> BridgeReachability
        var failures: @MainActor () -> Int
        /// When the last list pull from ANY trigger started (`lastSessionRefreshStartedAt`).
        var lastRefreshStart: @MainActor () -> Date?
        /// One tick: `refreshSessions`, nothing else.
        var refresh: @MainActor () async -> Void
        /// Ask for a reachability probe. Used only while the bridge is known unreachable, so the
        /// poller has a way back to `.reachable` on a window nobody is touching.
        var reprobe: @MainActor () -> Void
        var now: @MainActor () -> Date = { Date() }
    }

    /// Set between `NSWorkspace.willSleepNotification` and `didWakeNotification`.
    private(set) var isAsleep = false
    /// Ticks this poller has run. For tests and nothing else.
    private(set) var tickCount = 0

    /// When this poller last ticked (or started). The cadence runs from the later of this and the
    /// coordinator's last refresh start, which matters when a tick never reaches the bridge (an
    /// unpaired app): without it the loop would find "nothing pulled yet" and spin.
    private var lastTickAt: Date?
    private var waiter: CheckedContinuation<Void, Never>?
    private var waitElapsed = false
    private var pendingNudge = false

    /// Run until the calling task is cancelled.
    ///
    /// The first tick is a full interval after the start: launch has its own refresh (which
    /// prunes empty threads first), and a tick racing it would sync before the prune.
    func run(_ inputs: Inputs) async {
        lastTickAt = inputs.now()
        while !Task.isCancelled {
            let reachability = inputs.reachability()
            guard let delay = MacSessionPollRule.nextDelay(
                appActive: inputs.isAppActive(), reachability: reachability,
                failures: inputs.failures(), asleep: isAsleep) else {
                if reachability == .unreachable && !isAsleep {
                    // Known unreachable: no list pulls, but ask for a probe at the backoff cap so
                    // a bridge that comes back is noticed. The probe flips the state, the view
                    // nudges, and the cadence resumes.
                    if await wait(MacSessionPollRule.backoffCap) { inputs.reprobe() }
                } else {
                    await wait(nil)
                }
                continue
            }
            let last = [inputs.lastRefreshStart(), lastTickAt].compactMap { $0 }.max()
            let left = MacSessionPollRule.remaining(delay: delay, since: last, now: inputs.now())
            if left > 0.05 {
                // Whether the wait elapsed or was cut short, go round again: the inputs may have
                // changed, and a refresh from another trigger may have moved the start.
                await wait(left)
                continue
            }
            await tick(inputs)
            // Always give the actor back between ticks. A refresh that returns without
            // suspending (an unpaired app) must not turn the loop into a spin on the main actor.
            await Task.yield()
        }
    }

    /// One tick: the existing `refreshSessions`, through its ETag and its re-entrancy guard.
    func tick(_ inputs: Inputs) async {
        tickCount += 1
        lastTickAt = inputs.now()
        await inputs.refresh()
    }

    /// Re-read the rule now. Called on activation, deactivation, a reachability change, sleep
    /// and wake.
    func nudge() {
        if let w = waiter {
            waiter = nil
            w.resume()
        } else {
            pendingNudge = true
        }
    }

    func willSleep() { isAsleep = true; nudge() }
    func didWake() { isAsleep = false; nudge() }

    /// Suspend for `seconds` (nil: until nudged), or until nudged or cancelled. True only when
    /// the full delay elapsed.
    @discardableResult
    private func wait(_ seconds: TimeInterval?) async -> Bool {
        if pendingNudge {
            pendingNudge = false
            return false
        }
        waitElapsed = false
        let timer: Task<Void, Never>? = seconds.map { s in
            Task { @MainActor [weak self] in
                guard (try? await Task.sleep(for: .seconds(s))) != nil else { return }
                self?.resumeFromTimer()
            }
        }
        await withTaskCancellationHandler {
            await withCheckedContinuation { waiter = $0 }
        } onCancel: {
            Task { @MainActor [weak self] in self?.nudge() }
        }
        timer?.cancel()
        return waitElapsed
    }

    private func resumeFromTimer() {
        guard let w = waiter else { return }
        waiter = nil
        waitElapsed = true
        w.resume()
    }
}

extension View {
    /// Keep this window's conversation list current: poll on `MacSessionPollRule`, and pull at
    /// once when Jesse becomes the frontmost app. Attach once per window, at the window's root,
    /// so it runs whichever tab is on screen.
    func macSessionPolling() -> some View {
        modifier(MacSessionPollingModifier())
    }
}

private struct MacSessionPollingModifier: ViewModifier {
    @Environment(MacCoordinator.self) private var coordinator
    @Environment(\.modelContext) private var context
    @State private var poller = MacSessionPoller()

    func body(content: Content) -> some View {
        content
            .task {
                let coordinator = coordinator
                let context = context
                await poller.run(MacSessionPoller.Inputs(
                    isAppActive: { NSApplication.shared.isActive },
                    reachability: { BridgeReachabilityModel.shared.state },
                    failures: { coordinator.sessionRefreshFailures },
                    lastRefreshStart: { coordinator.lastSessionRefreshStartedAt },
                    refresh: { await coordinator.refreshSessions(context: context) },
                    reprobe: {
                        BridgeReachabilityModel.shared.refresh(config: coordinator.configStore.config)
                    }))
            }
            .onChange(of: BridgeReachabilityModel.shared.state) { _, _ in poller.nudge() }
            // Switching back to Jesse from another app pulls at once, outside the cadence.
            // Through the debounce: the window's `scenePhase` return (`onReconnect`) often
            // fires in the same instant.
            .onReceive(NotificationCenter.default
                .publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                    poller.nudge()
                    Task { await coordinator.refreshSessionsUnlessRecent(context: context) }
                }
            .onReceive(NotificationCenter.default
                .publisher(for: NSApplication.didResignActiveNotification)) { _ in
                    poller.nudge()
                }
            // `NSWorkspace`'s own center: the sleep and wake notifications are posted only there.
            .onReceive(NSWorkspace.shared.notificationCenter
                .publisher(for: NSWorkspace.willSleepNotification)) { _ in
                    poller.willSleep()
                }
            .onReceive(NSWorkspace.shared.notificationCenter
                .publisher(for: NSWorkspace.didWakeNotification)) { _ in
                    // The wake's pull is `onReconnect`'s; this only lets the cadence resume,
                    // measured from that pull.
                    poller.didWake()
                }
    }
}
