import XCTest
@testable import JesseVault

// THE FOLDER LIST, as a fold over paths and nothing else. No disk, no index, no actor.

final class VaultFolderTreeTests: XCTestCase {

    func testANoteCountsTowardEveryFolderAboveIt() {
        let folders = VaultFolderTree.folders(fromPaths: ["Projects/drafts/archive/x.md"])

        XCTAssertEqual(folders.map(\.path),
                       ["Projects", "Projects/drafts", "Projects/drafts/archive"])
        XCTAssertEqual(folders.map(\.noteCount), [1, 1, 1],
                       "one note, counted once at every depth above it")
    }

    func testCountsAddUpAcrossSiblings() {
        let folders = VaultFolderTree.folders(fromPaths: [
            "Projects/drafts/one.md",
            "Projects/drafts/two.md",
            "Projects/Research/three.md",
        ])

        XCTAssertEqual(folders, [
            VaultFolderCount(path: "Projects", noteCount: 3, directCount: 0, subfolderCount: 2),
            VaultFolderCount(path: "Projects/drafts", noteCount: 2, directCount: 2,
                             subfolderCount: 0),
            VaultFolderCount(path: "Projects/Research", noteCount: 1, directCount: 1,
                             subfolderCount: 0),
        ])
    }

    /// A note at the vault root is in no folder. There is no synthetic root row.
    func testANoteAtTheRootContributesNoFolder() {
        XCTAssertEqual(VaultFolderTree.folders(fromPaths: ["Today.md"]), [])
    }

    func testAnEmptyInputGivesAnEmptyOutput() {
        XCTAssertEqual(VaultFolderTree.folders(fromPaths: []), [])
    }

    /// THE PREFIX TRAP, stated at the pure layer: `Work` and `Workshop` are two folders,
    /// and one is a string prefix of the other. They must never be merged.
    func testAFolderWhoseNameIsAPrefixOfAnotherStaysItsOwnFolder() {
        let folders = VaultFolderTree.folders(fromPaths: ["Work/a.md", "Workshop/b.md"])

        XCTAssertEqual(folders, [
            VaultFolderCount(path: "Work", noteCount: 1, directCount: 1, subfolderCount: 0),
            VaultFolderCount(path: "Workshop", noteCount: 1, directCount: 1, subfolderCount: 0),
        ])
    }

    func testTheOrderIsCaseInsensitiveAndStableAcrossTwoCalls() {
        let paths = ["zebra/a.md", "Apple/b.md", "apple/c.md", "Banana/d.md"]

        let first = VaultFolderTree.folders(fromPaths: paths)
        let second = VaultFolderTree.folders(fromPaths: paths.reversed())

        XCTAssertEqual(first.map(\.path), ["Apple", "apple", "Banana", "zebra"],
                       "case insensitive first, then case sensitive to break the tie")
        XCTAssertEqual(first, second,
                       "the same paths in any order give the same list, every time")
    }

    // MARK: - Direct and subfolder counts

    /// **The count the picker leads with is the folder's OWN notes.** A research folder
    /// with 3 live reports and a 5 note archive beneath it reads as 3, with one subfolder,
    /// and only its recursive count says 8.
    func testDirectCountIsAFoldersOwnNotesAndNoteCountIsEverythingBeneath() {
        let paths = ["Projects/Research/a.md", "Projects/Research/b.md",
                     "Projects/Research/c.md"]
            + (1...5).map { "Projects/Research/archive/old-\($0).md" }
        let research = VaultFolderTree.folders(fromPaths: paths)
            .first { $0.path == "Projects/Research" }

        XCTAssertEqual(research?.directCount, 3)
        XCTAssertEqual(research?.noteCount, 8)
        XCTAssertEqual(research?.subfolderCount, 1)
    }

    /// A folder holding only folders has no notes of its own, and counts each child
    /// folder once however many notes are in it; grandchildren are not children.
    func testSubfolderCountIsOnlyTheFoldersDirectlyInside() {
        let folders = VaultFolderTree.folders(fromPaths: [
            "Projects/drafts/one.md",
            "Projects/drafts/two.md",
            "Projects/drafts/archive/old.md",
            "Projects/Research/three.md",
            "Projects/Research/topic/deep/four.md",
        ])
        let byPath = Dictionary(uniqueKeysWithValues: folders.map { ($0.path, $0) })

        XCTAssertEqual(byPath["Projects"]?.directCount, 0)
        XCTAssertEqual(byPath["Projects"]?.subfolderCount, 2, "drafts and Research")
        XCTAssertEqual(byPath["Projects/drafts"]?.subfolderCount, 1)
        XCTAssertEqual(byPath["Projects/Research"]?.subfolderCount, 1, "topic, not deep")
        XCTAssertEqual(byPath["Projects/Research/topic"]?.directCount, 0)
        XCTAssertEqual(byPath["Projects/Research/topic"]?.subfolderCount, 1)
        XCTAssertEqual(byPath["Projects/Research/topic/deep"]?.directCount, 1)
        XCTAssertEqual(byPath["Projects/Research/topic/deep"]?.subfolderCount, 0)
    }

    /// One drill down level: the folders directly inside a parent, the root's for nil.
    func testChildrenAreOneLevelOfTheTree() {
        let folders = VaultFolderTree.folders(fromPaths: [
            "Projects/drafts/one.md", "Projects/Research/topic/two.md", "People/three.md",
        ])

        XCTAssertEqual(VaultFolderTree.children(of: nil, in: folders).map(\.path),
                       ["People", "Projects"])
        XCTAssertEqual(VaultFolderTree.children(of: "Projects", in: folders).map(\.path),
                       ["Projects/drafts", "Projects/Research"])
        XCTAssertEqual(VaultFolderTree.children(of: "Projects/Research", in: folders)
                        .map(\.name), ["topic"])
    }
}
