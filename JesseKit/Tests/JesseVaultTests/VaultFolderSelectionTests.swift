import XCTest
@testable import JesseVault

// THE VAULT TAB'S FOLDER FILTER: exact by default, several folders at once, archives
// folded away. Driven over a real temporary vault and a real SQLite index, because the
// claims are about SQL — that the predicate is in the query, that it is escaped, and that
// it says exactly what the Swift predicate beside it says.

@MainActor
final class VaultFolderSelectionTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!
    private var root: URL!
    private var databaseDirectory: URL!

    override func setUp() async throws {
        try await super.setUp()
        suiteName = "jesse.vault.folder-selection.tests.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        root = VaultFixture.makeDirectory()
        databaseDirectory = VaultFixture.makeDirectory()
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suiteName)
        VaultFixture.cleanUp(root)
        VaultFixture.cleanUp(databaseDirectory)
        try await super.tearDown()
    }

    // MARK: - Fixture

    /// A research folder with one live note, an archive and a topic subfolder; a folder
    /// whose name is a `LIKE` pattern, and a sibling that pattern would match unescaped;
    /// a note at the root; and an `Archive-Policy.md` that is NOT archived.
    private static let fixturePaths = [
        "Projects/Research/a.md",
        "Projects/Research/archive/b.md",
        "Projects/Research/topic/c.md",
        "Projects/Research-Old/d.md",
        "Odd%_Folder/e.md",
        "OddXYFolder/f.md",
        "Odd%_Folder/Archive/g.md",
        "Policies/Archive-Policy.md",
        "Today.md",
    ]

    private func writeFixture() {
        let base = Date(timeIntervalSince1970: 1_700_000_000)
        for (offset, path) in Self.fixturePaths.enumerated() {
            VaultFixture.write("# \(path)\n\nThe bisque schedule.\n", to: path, in: root)
            VaultFixture.touch(path, in: root, date: base.addingTimeInterval(Double(offset) * 60))
        }
    }

    private func makeIndex() throws -> VaultIndex {
        let index = try VaultIndex(url: VaultIndex.databaseURL(forRoot: root,
                                                               in: databaseDirectory))
        let file = VaultFile(root: root)
        try index.reindex(scan: VaultScanner().scan(root: root),
                          read: { try file.read(relativePath: $0) })
        return index
    }

    private func makeSource() throws -> VaultIndexSource {
        let folder = VaultFolder(defaults: defaults, key: "test.bookmark")
        try folder.adopt(url: root)
        return VaultIndexSource(folder: folder, container: databaseDirectory)
    }

    private func paths(_ files: [VaultIndexedFile]) -> Set<String> { Set(files.map(\.path)) }

    // MARK: - The predicate

    func testAnExactFolderIsOnlyTheNotesDirectlyInsideIt() throws {
        writeFixture()
        let index = try makeIndex()
        let research = VaultFolderSelection(folder: "Projects/Research")

        XCTAssertEqual(paths(index.recentFiles(limit: 100, in: research)),
                       ["Projects/Research/a.md"],
                       "not the archive, not the topic folder, not Research-Old")
    }

    func testWideningAFolderBringsInEverythingBeneathIt() throws {
        writeFixture()
        let index = try makeIndex()
        var research = VaultFolderSelection(folder: "Projects/Research")
        research.setIncludesSubfolders(true, for: "Projects/Research")

        XCTAssertEqual(paths(index.recentFiles(limit: 100, in: research)),
                       ["Projects/Research/a.md", "Projects/Research/archive/b.md",
                        "Projects/Research/topic/c.md"],
                       "and still not Research-Old, which only shares a prefix")
        XCTAssertEqual(paths(index.recentFiles(limit: 100, in: research, archive: .live)),
                       ["Projects/Research/a.md", "Projects/Research/topic/c.md"])
        XCTAssertEqual(paths(index.recentFiles(limit: 100, in: research, archive: .archived)),
                       ["Projects/Research/archive/b.md"])
    }

    /// **A folder named like a `LIKE` pattern cannot widen the match.** Unescaped,
    /// `Odd%_Folder/%` would also match `OddXYFolder/f.md`.
    func testAWildcardInAFolderNameIsMatchedLiterally() throws {
        writeFixture()
        let index = try makeIndex()

        XCTAssertEqual(paths(index.recentFiles(limit: 100,
                                               in: VaultFolderSelection(folder: "Odd%_Folder"))),
                       ["Odd%_Folder/e.md"])
        var widened = VaultFolderSelection(folder: "Odd%_Folder")
        widened.setIncludesSubfolders(true, for: "Odd%_Folder")
        XCTAssertEqual(paths(index.recentFiles(limit: 100, in: widened)),
                       ["Odd%_Folder/e.md", "Odd%_Folder/Archive/g.md"])
    }

    /// **The SQL and the Swift say the same thing**, over every fixture path, for every
    /// shape of selection and every side of the archive line. They are two spellings of
    /// one predicate, and this is what keeps them one.
    func testTheSQLAndTheSwiftPredicatesAgreeOverEveryPath() throws {
        writeFixture()
        let index = try makeIndex()
        let all = index.allPaths()
        XCTAssertEqual(Set(all), Set(Self.fixturePaths))

        var widened = VaultFolderSelection(folder: "Projects/Research")
        widened.setIncludesSubfolders(true, for: "Projects/Research")
        var two = VaultFolderSelection(folder: "Projects/Research")
        two.insert("Projects/Research/archive")
        var mixed = VaultFolderSelection(folder: "Odd%_Folder")
        mixed.insert("Projects", includesSubfolders: true)
        let selections = [
            VaultFolderSelection(),
            VaultFolderSelection(folder: "Projects/Research"),
            VaultFolderSelection(folder: "Projects"),
            VaultFolderSelection(folder: "Odd%_Folder"),
            VaultFolderSelection(folder: "Nowhere"),
            widened, two, mixed,
        ]
        for selection in selections {
            for archive in [VaultArchiveFilter.any, .live, .archived] {
                let filter = VaultPathFilter(folders: selection, archive: archive)
                XCTAssertEqual(paths(index.recentFiles(limit: 1_000, filter: filter)),
                               Set(all.filter(filter.includes)),
                               "\(selection.entries) \(archive)")
                XCTAssertEqual(Set(index.chunks(filter: filter).map(\.path)),
                               Set(all.filter(filter.includes)),
                               "chunks: \(selection.entries) \(archive)")
                XCTAssertEqual(
                    Set(index.search(expression: "\"bisque\"*", limit: 1_000, filter: filter)
                        .map(\.path)),
                    Set(all.filter(filter.includes)),
                    "search: \(selection.entries) \(archive)")
            }
        }
    }

    /// The archive test is a directory SEGMENT, folded for case, in both halves.
    func testTheArchiveRuleIsADirectorySegment() {
        XCTAssertTrue(VaultArchive.isArchived("Projects/Research/archive/b.md"))
        XCTAssertTrue(VaultArchive.isArchived("archive/b.md"))
        XCTAssertTrue(VaultArchive.isArchived("Odd%_Folder/Archive/g.md"))
        XCTAssertFalse(VaultArchive.isArchived("Policies/Archive-Policy.md"))
        XCTAssertFalse(VaultArchive.isArchived("Workshop/archive.md"))
        XCTAssertFalse(VaultArchive.isArchived("Projects/archived/x.md"))
        XCTAssertEqual(VaultRetriever.isArchived("Projects/drafts/archive/x.md"),
                       VaultArchive.isArchived("Projects/drafts/archive/x.md"),
                       "the answerer asks the same rule")
    }

    func testTheSelectionIsAnOrderedSet() {
        var selection = VaultFolderSelection()
        selection.toggle("B")
        selection.toggle("A")
        selection.insert("B", includesSubfolders: true)
        XCTAssertEqual(selection.paths, ["B", "A"], "picking order, each folder once")
        XCTAssertEqual(selection.entry(for: "B")?.includesSubfolders, true)
        selection.toggle("B")
        XCTAssertEqual(selection.paths, ["A"])
        XCTAssertEqual(VaultFolderSelection([.init(path: "A"), .init(path: "A")]).count, 1)
    }

    // MARK: - The screen's model

    /// **THE BUG.** A folder whose archive holds 200 notes, every one newer than its 5
    /// live ones, held with its subfolders. The recents are the newest 30 by mtime, and
    /// on `main` that was 30 archived notes and no live one. The live notes are asked
    /// for on their own now, with their own LIMIT, and the archive waits under its header.
    func testTwoHundredNewerArchivedNotesCannotCrowdOutFiveLiveOnes() async throws {
        let old = Date(timeIntervalSince1970: 1_700_000_000)
        var live: Set<String> = []
        for i in 1...5 {
            let path = "Projects/Research/Live-\(i).md"
            VaultFixture.write("# Live \(i)\n", to: path, in: root)
            VaultFixture.touch(path, in: root, date: old.addingTimeInterval(Double(i)))
            live.insert(path)
        }
        for i in 1...200 {
            let path = "Projects/Research/archive/Done-\(i).md"
            VaultFixture.write("# Done \(i)\n", to: path, in: root)
            VaultFixture.touch(path, in: root, date: old.addingTimeInterval(86_400 + Double(i)))
        }
        let model = VaultBrowserModel(source: try makeSource())
        await model.indexer.reindexNow()
        model.refresh()

        var research = VaultFolderSelection(folder: "Projects/Research")
        research.setIncludesSubfolders(true, for: "Projects/Research")
        model.folders = research

        XCTAssertEqual(paths(model.recents), live, "every live note, and nothing else")
        XCTAssertEqual(model.archivedRecents.count, VaultBrowserModel.archivedLimit)
        XCTAssertTrue(model.archivedRecents.allSatisfy { VaultArchive.isArchived($0.path) })
        XCTAssertEqual(model.archivedRecents.first?.path,
                       "Projects/Research/archive/Done-200.md", "newest first")

        // Exact, the folder is its own five notes and no archive at all.
        model.folders = VaultFolderSelection(folder: "Projects/Research")
        XCTAssertEqual(paths(model.recents), live)
        XCTAssertEqual(model.archivedRecents, [])
    }

    /// Adding the archive folder beside the live one puts its notes under the Archived
    /// header, never among the live ones.
    func testAnArchiveFolderPickedBesideItsParentFoldsAway() async throws {
        writeFixture()
        let model = VaultBrowserModel(source: try makeSource())
        await model.indexer.reindexNow()
        model.refresh()

        var selection = VaultFolderSelection(folder: "Projects/Research")
        selection.insert("Projects/Research/archive")
        model.folders = selection

        XCTAssertEqual(paths(model.recents), ["Projects/Research/a.md"])
        XCTAssertEqual(paths(model.archivedRecents), ["Projects/Research/archive/b.md"])
    }

    /// **A held folder can go without taking the others with it.** Only the vanished
    /// one is dropped, and the sentence names it.
    func testAVanishedFolderIsDroppedAloneAndNamed() async throws {
        writeFixture()
        let model = VaultBrowserModel(source: try makeSource())
        await model.indexer.reindexNow()
        var selection = VaultFolderSelection(folder: "Projects/Research")
        selection.insert("Projects/Research-Old")
        model.folders = selection

        try FileManager.default.removeItem(at: root.appendingPathComponent("Projects/Research-Old"))
        await model.indexer.reindexNow()
        model.refresh()

        XCTAssertEqual(model.folders.paths, ["Projects/Research"])
        XCTAssertEqual(model.lastError,
                       "Projects/Research-Old is not in the vault any more, so the filter dropped it.")
        XCTAssertEqual(paths(model.recents), ["Projects/Research/a.md"])
    }

    /// A typed search under a selection ranks live notes before archived ones: demoted,
    /// never dropped.
    func testATypedSearchRanksLiveBeforeArchivedUnderASelection() throws {
        writeFixture()
        let index = try makeIndex()
        var research = VaultFolderSelection(folder: "Projects/Research")
        research.setIncludesSubfolders(true, for: "Projects/Research")

        let hits = VaultSearcher(index: index, folders: research).base("bisque").hits.map(\.path)

        XCTAssertEqual(Set(hits), ["Projects/Research/a.md", "Projects/Research/archive/b.md",
                                   "Projects/Research/topic/c.md"])
        XCTAssertEqual(hits.last, "Projects/Research/archive/b.md")
    }

    // MARK: - The widening tier

    /// **The expansion tier cannot leave a two folder selection.** The typed query is
    /// thin, the expander offers a term found in a note outside both folders, and that
    /// note must not appear, nor the term be named.
    func testTheExpansionTierNeverReachesOutsideATwoFolderSelection() async throws {
        VaultFixture.write("# Kiln\n\nThe arch is rebuilt.\n", to: "Workshop/Kiln.md", in: root)
        VaultFixture.write("# Glaze\n\nCopper red.\n", to: "Studio/Glaze.md", in: root)
        VaultFixture.write("# Oven\n\nThe bread oven is separate.\n",
                           to: "Bicycle/Oven.md", in: root)
        VaultFixture.write("# Oven notes\n\nAnother oven.\n",
                           to: "Workshop/deeper/Oven.md", in: root)
        let index = try makeIndex()
        var selection = VaultFolderSelection(folder: "Workshop")
        selection.insert("Studio")
        let expander = ExpandTo(["oven"])

        let outcome = await VaultSearcher(index: index, folders: selection)
            .search("kiln", expander: expander)

        XCTAssertEqual(outcome.hits.map(\.path), ["Workshop/Kiln.md"],
                       "not Bicycle, and not the exact folder's subfolder either")
        XCTAssertEqual(outcome.expansionTerms, [])

        // The same term, with the subfolder widened in, does contribute: the guard is
        // the selection, not a dry expander.
        selection.setIncludesSubfolders(true, for: "Workshop")
        let widened = await VaultSearcher(index: index, folders: selection)
            .search("kiln", expander: expander)
        XCTAssertEqual(widened.hits.map(\.path), ["Workshop/Kiln.md", "Workshop/deeper/Oven.md"])
        XCTAssertEqual(widened.expansionTerms, ["oven"])
    }
}

/// An expander that always offers the same terms.
private struct ExpandTo: VaultQueryExpanding {
    let terms: [String]
    init(_ terms: [String]) { self.terms = terms }
    func expand(_ query: String) async -> [String] { terms }
}
