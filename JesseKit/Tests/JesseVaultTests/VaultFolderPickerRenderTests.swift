#if os(iOS)
import XCTest
import SwiftUI
import UIKit
@testable import JesseVault

// **The folder filter, drawn**, at the smallest iPhone width, at the default text size and
// at the largest accessibility size. The pictures attached to the pull request came from
// here: the REAL picker and the REAL Vault screen over a real index of an invented vault,
// hosted in a UIKit window on the simulator and drawn with `drawHierarchy`. No app launch,
// no UI automation, nothing on a device.
//
// iOS only, and fenced, for a reason `StrandBoardRenderTests` does not share: that test
// renders plain stacks through `ImageRenderer`, which draws a `List` or a
// `NavigationStack` as blank, and both of those ARE what is under test here. Only a
// window draws them, and the window that can is UIKit's.
//
// SKIPPED unless `VAULT_PICKER_PNG_DIR` names a directory (through xcodebuild, as
// `TEST_RUNNER_VAULT_PICKER_PNG_DIR`): it writes files and waits on layout, which is a
// cost a gate run has no use for.

@MainActor
final class VaultFolderPickerRenderTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!
    private var root: URL!
    private var databaseDirectory: URL!
    private var outputDirectory: URL!

    /// iPhone SE (3rd generation), the narrowest iPhone iOS 26 runs on.
    private let phone = CGSize(width: 375, height: 667)

    override func setUp() async throws {
        try await super.setUp()
        guard let dir = ProcessInfo.processInfo.environment["VAULT_PICKER_PNG_DIR"] else {
            throw XCTSkip("set VAULT_PICKER_PNG_DIR to render the folder filter")
        }
        outputDirectory = URL(fileURLWithPath: dir, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory,
                                                withIntermediateDirectories: true)
        suiteName = "jesse.vault.picker.render.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        root = VaultFixture.makeDirectory()
        databaseDirectory = VaultFixture.makeDirectory()
        writeVault()
    }

    override func tearDown() async throws {
        defaults?.removePersistentDomain(forName: suiteName)
        if let root { VaultFixture.cleanUp(root) }
        if let databaseDirectory { VaultFixture.cleanUp(databaseDirectory) }
        try await super.tearDown()
    }

    /// An invented vault shaped like a real one: a research folder whose archive dwarfs
    /// it, drafts with their own archive, and a few shallow folders.
    private func writeVault() {
        let old = Date(timeIntervalSince1970: 1_780_000_000)
        var n = 0
        func note(_ path: String, archivedNewer: Bool = false) {
            n += 1
            let title = (path.split(separator: "/").last.map(String.init) ?? path)
                .replacingOccurrences(of: ".md", with: "")
                .replacingOccurrences(of: "-", with: " ")
            VaultFixture.write("# \(title)\n\nNotes.\n", to: path, in: root)
            let offset = archivedNewer ? 1_000_000 + Double(n) * 60 : Double(n) * 60
            VaultFixture.touch(path, in: root, date: old.addingTimeInterval(offset))
        }
        for name in ["Kiln-Firing-Curves", "Glaze-Chemistry", "Clay-Body-Shrinkage",
                     "Wood-Ash-Glazes", "Reduction-Atmospheres", "Slip-Casting"] {
            note("Projects/Research/\(name).md")
        }
        for i in 1...40 { note("Projects/Research/archive/Old-Report-\(i).md", archivedNewer: true) }
        for i in 1...3 { note("Projects/Research/Studio-Lighting/Lamp-\(i).md") }
        for i in 1...4 { note("Projects/drafts/Draft-\(i).md") }
        for i in 1...12 { note("Projects/drafts/archive/Sent-\(i).md", archivedNewer: true) }
        for i in 1...2 { note("Projects/Workshop-Renovation/Plan-\(i).md") }
        for i in 1...5 { note("People/Suppliers/Supplier-\(i).md") }
        for i in 1...3 { note("People/Studio/Potter-\(i).md") }
        for i in 1...4 { note("Knowledge/Cookbook/Recipe-\(i).md") }
        for i in 1...2 { note("Strands/Strand-\(i).md") }
        note("Inbox/Capture.md")
    }

    private func makeModel() async throws -> VaultBrowserModel {
        let folder = VaultFolder(defaults: defaults, key: "test.bookmark")
        try folder.adopt(url: root)
        let model = VaultBrowserModel(source: VaultIndexSource(folder: folder,
                                                               container: databaseDirectory))
        await model.indexer.reindexNow()
        model.refresh()
        return model
    }

    // MARK: - Drawing

    /// `height` stretches the window past a phone's for the list pictures, so the
    /// Archived section is in the frame rather than below the fold; the width stays a
    /// phone's, which is what the layout is being checked against.
    ///
    /// AT THE LARGEST ACCESSIBILITY SIZE the window is always tall. In this sceneless
    /// window a `.searchable` list that overflows draws NO rows at all — a stock `List`
    /// of plain `Text` with nothing but `.searchable` on it does the same, so it is the
    /// harness and not the view — and a window tall enough to hold the list is the only
    /// way to draw it.
    private func render<V: View>(_ view: V, name: String, height: CGFloat? = nil) async throws {
        for (suffix, category, tall) in [("default", UIContentSizeCategory.large, height),
                                         ("ax5", .accessibilityExtraExtraExtraLarge,
                                          max(height ?? 0, 2_600))] {
            let host = UIHostingController(rootView: view)
            host.traitOverrides.preferredContentSizeCategory = category
            // A package's test host connects no scene, so the window is the old
            // sceneless kind; it draws what a scene's window draws.
            let frame = CGRect(x: 0, y: 0, width: phone.width, height: tall ?? phone.height)
            let window = UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }.first
                .map { scene -> UIWindow in
                    let window = UIWindow(windowScene: scene)
                    window.frame = frame
                    return window
                } ?? UIWindow(frame: frame)
            window.rootViewController = host
            window.makeKeyAndVisible()
            // SwiftUI lays a List out over several passes; a render taken on the first
            // is a list with no rows.
            for _ in 0..<12 {
                try await Task.sleep(for: .milliseconds(250))
                host.view.setNeedsLayout()
                host.view.layoutIfNeeded()
            }
            // `layer.render`, not `drawHierarchy`: the latter draws from the screen
            // server, and a sceneless window in a test host has nothing there to draw.
            let image = UIGraphicsImageRenderer(bounds: window.bounds).image { context in
                window.layer.render(in: context.cgContext)
            }
            window.isHidden = true
            let data = try XCTUnwrap(image.pngData())
            let url = outputDirectory.appendingPathComponent("\(name)-\(suffix).png")
            try data.write(to: url)
            print("VAULT PICKER PNG: \(url.path)")
            XCTAssertGreaterThan(distinctColours(image), 8, "\(name) \(suffix) is blank")
        }
    }

    /// How many distinct colours a sample of the image holds: a blank render is one or
    /// two, and a size check on the PNG passes a white page.
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

    private func picker(_ model: VaultBrowserModel, selection: VaultFolderSelection,
                        trail: [String] = [], filter: String = "") -> some View {
        VaultFolderPicker(folders: model.folderCounts, selection: selection,
                          filter: filter, trail: trail, onDone: { _ in }, onCancel: {})
    }

    func testThePickerAtTheRoot() async throws {
        let model = try await makeModel()
        try await render(picker(model, selection: VaultFolderSelection()), name: "1-picker-root")
    }

    func testThePickerDrilledIntoResearch() async throws {
        let model = try await makeModel()
        try await render(picker(model, selection: VaultFolderSelection(folder: "Projects/Research"),
                                trail: ["Projects", "Projects/Research"]),
                         name: "2-picker-research")
    }

    func testThePickerWithTwoFoldersSelected() async throws {
        let model = try await makeModel()
        var selection = VaultFolderSelection(folder: "Projects/Research")
        selection.insert("Projects/drafts", includesSubfolders: true)
        try await render(picker(model, selection: selection, trail: ["Projects"]),
                         name: "3-picker-two-selected")
    }

    func testThePickerSearchingFlat() async throws {
        let model = try await makeModel()
        try await render(picker(model, selection: VaultFolderSelection(folder: "Projects/Research"),
                                filter: "archive"),
                         name: "4-picker-search")
    }

    func testTheChipsAndTheArchivedSectionCollapsed() async throws {
        let model = try await makeModel()
        var selection = VaultFolderSelection(folder: "Projects/Research")
        selection.insert("Projects/Research/archive")
        model.folders = selection
        try await render(VaultBrowserView(model: model, showsArchived: false),
                         name: "5-list-archived-collapsed", height: 1_000)
    }

    func testTheChipsAndTheArchivedSectionExpanded() async throws {
        let model = try await makeModel()
        var selection = VaultFolderSelection(folder: "Projects/drafts")
        selection.insert("Projects/Workshop-Renovation")
        selection.setIncludesSubfolders(true, for: "Projects/drafts")
        model.folders = selection
        try await render(VaultBrowserView(model: model, showsArchived: true),
                         name: "6-list-archived-expanded", height: 2_200)
    }
}
#endif
