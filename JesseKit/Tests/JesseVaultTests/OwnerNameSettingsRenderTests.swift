#if os(iOS)
import XCTest
import SwiftUI
import UIKit
@testable import JesseVault

// **The owner-name field, drawn**, at the narrowest iPhone width, light appearance. The
// pictures attached to the pull request came from here: the REAL shared rows inside a
// `Form` section titled as both Settings screens title it, and the REAL offline reply a
// first-person question gets with no name set, hosted in a UIKit window on the simulator.
// No app launch, no UI automation, nothing on a device. Every value is invented.
//
// SKIPPED unless `OWNER_NAME_PNG_DIR` names a directory (through xcodebuild, as
// `TEST_RUNNER_OWNER_NAME_PNG_DIR`): it writes files and waits on layout, which is a cost a
// gate run has no use for. Same window and drawing as `VaultFolderPickerRenderTests`.

@MainActor
final class OwnerNameSettingsRenderTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!
    private var outputDirectory: URL!

    /// iPhone SE (3rd generation), the narrowest iPhone iOS 26 runs on.
    private let phone = CGSize(width: 375, height: 667)

    override func setUp() async throws {
        try await super.setUp()
        guard let dir = ProcessInfo.processInfo.environment["OWNER_NAME_PNG_DIR"] else {
            throw XCTSkip("set OWNER_NAME_PNG_DIR to render the owner-name field")
        }
        outputDirectory = URL(fileURLWithPath: dir, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory,
                                                withIntermediateDirectories: true)
        suiteName = "jesse.owner.name.render.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    }

    override func tearDown() async throws {
        defaults?.removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    private func render<V: View>(_ view: V, name: String) async throws {
        let host = UIHostingController(rootView: view)
        host.overrideUserInterfaceStyle = .light
        host.traitOverrides.preferredContentSizeCategory = .large
        let frame = CGRect(origin: .zero, size: phone)
        let window = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }.first
            .map { scene -> UIWindow in
                let window = UIWindow(windowScene: scene)
                window.frame = frame
                return window
            } ?? UIWindow(frame: frame)
        window.overrideUserInterfaceStyle = .light
        window.rootViewController = host
        window.makeKeyAndVisible()
        for _ in 0..<12 {
            try await Task.sleep(for: .milliseconds(250))
            host.view.setNeedsLayout()
            host.view.layoutIfNeeded()
        }
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { context in
            window.layer.render(in: context.cgContext)
        }
        window.isHidden = true
        let data = try XCTUnwrap(image.pngData())
        let url = outputDirectory.appendingPathComponent("\(name).png")
        try data.write(to: url)
        print("OWNER NAME PNG: \(url.path)")
        XCTAssertGreaterThan(distinctColours(image), 8, "\(name) is blank")
    }

    /// How many distinct colours a sample of the image holds: a blank render is one or two.
    private func distinctColours(_ image: UIImage) -> Int {
        guard let cg = image.cgImage,
              let data = cg.dataProvider?.data,
              let bytes = CFDataGetBytePtr(data) else { return 0 }
        var seen = Set<UInt32>()
        let step = max(1, cg.height / 200)
        for y in stride(from: 0, to: cg.height, by: step) {
            for x in stride(from: 0, to: cg.width, by: max(1, cg.width / 100)) {
                let at = y * cg.bytesPerRow + x * (cg.bitsPerPixel / 8)
                seen.insert(UInt32(bytes[at]) << 16 | UInt32(bytes[at + 1]) << 8
                            | UInt32(bytes[at + 2]))
            }
        }
        return seen.count
    }

    private func section() -> some View {
        Form {
            Section {
                OwnerNameSettingsRows(settings: OfflineLookupSettings(defaults: defaults))
            } header: {
                Text("Vault folder")
            }
        }
    }

    func testTheFieldFilled() async throws {
        OwnerNameSettingsRows.commit("Jeremy, Jeremiah, Andrews",
                                     to: OfflineLookupSettings(defaults: defaults))
        try await render(section(), name: "owner-name-1-filled")
    }

    func testTheFieldEmptyShowsThePrompt() async throws {
        try await render(section(), name: "owner-name-2-empty")
    }

    func testTheNoticeAFirstPersonQuestionGetsWithNoName() async throws {
        let settings = OfflineLookupSettings(defaults: defaults)
        let notice = try XCTUnwrap(OfflineLookupReply.ownerNameNotice(
            question: "What is my birthday?", ownerName: settings.ownerName))
        let reply = OfflineLookupReply.body(.abstained, queued: true, notice: notice)
        try await render(ScrollView {
            Text(reply)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
        }, name: "owner-name-3-notice")
    }
}
#endif
