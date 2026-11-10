import XCTest
import JesseNetworking
@testable import Jesse_Mac

/// The Mac's registration with the bridge: a device id made once and kept, the `macos`
/// platform, and the Mac app's own bundle id as the topic its pushes need.
@MainActor
final class MacPushIdentityTests: XCTestCase {
    private var defaults: UserDefaults!
    private let suite = "MacPushIdentityTests"

    override func setUp() {
        super.setUp()
        defaults = UserDefaults(suiteName: suite)
        defaults.removePersistentDomain(forName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    func testTheDeviceIDIsMadeOnceAndKept() {
        let first = MacPushIdentity.deviceID(defaults)
        XCTAssertNotNil(UUID(uuidString: first))
        XCTAssertEqual(MacPushIdentity.deviceID(defaults), first)
    }

    func testTheRegistrationNamesThisMacItsPlatformAndItsTopic() throws {
        let reg = MacPushIdentity.registration(token: "ab12", defaults: defaults,
                                               bundleID: "com.tag1.JesseMac")
        XCTAssertEqual(reg, JesseDeviceRegistration(token: "ab12",
                                                    deviceID: MacPushIdentity.deviceID(defaults),
                                                    platform: "macos",
                                                    topic: "com.tag1.JesseMac"))
        let body = try JesseBridgeClient.encodeBody(reg)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: String])
        XCTAssertEqual(Set(obj.keys), ["token", "device_id", "platform", "topic"])
    }

    func testTheTokenIsSentAsLowercaseHex() {
        XCTAssertEqual(MacPushIdentity.hex(Data([0x0A, 0xFF, 0x00])), "0aff00")
    }
}
