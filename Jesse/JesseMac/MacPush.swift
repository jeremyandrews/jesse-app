import AppKit
import Foundation
import JesseNetworking
import UserNotifications

// APNs for the Mac app: the bridge's pushes (a Studio run reported, a scheduled job failed, a
// backgrounded turn finished) reach this Mac as well as the iPhone.
//
// Until now the Mac app never registered at all; it posted local banners for its own turns
// only. And the bridge kept ONE token, replaced by each registration, so a Mac that had
// registered would have taken every push away from the phone. The bridge now keeps one entry
// per device, keyed by the id sent here, and pushes them all.
//
// Registration runs on every launch, as the phone's does on every foreground: that is how a
// bridge restart, a re-pairing or a new token is covered. A failure is logged and retried at
// the next launch; nothing on screen depends on it.

/// Who this Mac is to the bridge.
enum MacPushIdentity {
    static let platform = "macos"
    static let deviceIDKey = "jesse.push.deviceID.v1"

    /// A UUID made once and kept: macOS has no `identifierForVendor`.
    static func deviceID(_ defaults: UserDefaults = .standard) -> String {
        if let id = defaults.string(forKey: deviceIDKey), !id.isEmpty { return id }
        let id = UUID().uuidString
        defaults.set(id, forKey: deviceIDKey)
        return id
    }

    static func registration(token: String, defaults: UserDefaults = .standard,
                             bundleID: String? = Bundle.main.bundleIdentifier)
        -> JesseDeviceRegistration {
        JesseDeviceRegistration(token: token, deviceID: deviceID(defaults),
                                platform: platform, topic: bundleID)
    }

    /// The APNs token as the bridge stores it: lowercase hex.
    static func hex(_ token: Data) -> String {
        token.map { String(format: "%02x", $0) }.joined()
    }
}

/// The app delegate: asks AppKit for the APNs token at launch and sends it to the bridge.
@MainActor
final class MacPushRegistrar: NSObject, NSApplicationDelegate {
    /// The paired bridge, read when the token arrives. Set by the app before launch finishes.
    static var config: () -> JesseConfig = { JesseConfig(host: "", port: JesseConfig.defaultPort, token: "") }
    /// False in a unit-test host, which must not register anything anywhere.
    static var isEnabled = true

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard Self.isEnabled, Self.config().isConfigured else { return }
        NSApplication.shared.registerForRemoteNotifications()
    }

    func application(_ application: NSApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let cfg = Self.config()
        guard Self.isEnabled, cfg.isConfigured else { return }
        let registration = MacPushIdentity.registration(token: MacPushIdentity.hex(deviceToken))
        Task {
            do {
                try await JesseBridgeClient(config: cfg).registerDevice(registration)
            } catch {
                NSLog("Jesse: push registration with the bridge failed: \(error.localizedDescription)")
            }
        }
    }

    func application(_ application: NSApplication,
                     didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        // Most often a build signed without the Push Notifications capability.
        NSLog("Jesse: APNs registration failed: \(error.localizedDescription)")
    }
}
