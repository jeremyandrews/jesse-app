import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

// THE BOARD OPENS IN THE LENS THE OWNER LAST PICKED, on this device, across relaunches.
//
// The bug this pins: the lens lived only on `StrandsModel`, so every launch started at
// `Most recent` and an owner who reads the board as a tree re-picked `Tree` every time.
//
// The trap it also pins is the reason the tests below separate `sortKey` from
// `effectiveSortKey`. `Tree` is offered only once a snapshot that serves `parent` has
// arrived, so `effectiveSortKey` is `Most recent` on every cold launch no matter what was
// chosen. Persisting what the MENU SHOWS would therefore overwrite a remembered `Tree`
// with the fallback before the board had even loaded. Only the chosen key is stored.
@MainActor
final class StrandsLensMemoryTests: XCTestCase {

    /// A scratch defaults domain, cleared at both ends, exactly as the badge filter's
    /// own relaunch test drives one: a test must never read or write this machine's
    /// preferences.
    private func scratch(_ name: String = "strands-lens-memory-tests") throws -> UserDefaults {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defaults.removePersistentDomain(forName: name)
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        return defaults
    }

    private func strand(_ slug: String, servesParent: Bool = true) -> Strand {
        Strand(slug: slug, title: slug, state: .active, updated: "2026-09-26",
               parent: nil, servesParent: servesParent)
    }

    /// Nothing stored is the board as it has always opened: the server's own order,
    /// which is the only lens every bridge can serve.
    func testStrandsLensDefaultsToMostRecent() throws {
        let defaults = try scratch()
        XCTAssertEqual(TodayViewPreferences(defaults: defaults).strandsLens, .mostRecent)
    }

    /// The whole point. A fresh store over the same domain is exactly what the next
    /// launch reads.
    func testStrandsLensSurvivesARelaunch() throws {
        let defaults = try scratch()
        TodayViewPreferences(defaults: defaults).strandsLens = .tree

        let afterRelaunch = TodayViewPreferences(defaults: defaults)
        XCTAssertEqual(afterRelaunch.strandsLens, .tree)

        // And the shell hands it straight to the model, which is all either shell does.
        let model = StrandsModel(makeClient: { NeverStrands() })
        model.sortKey = afterRelaunch.strandsLens
        XCTAssertEqual(model.sortKey, .tree)

        TodayViewPreferences(defaults: defaults).strandsLens = .group
        XCTAssertEqual(TodayViewPreferences(defaults: defaults).strandsLens, .group)
    }

    /// A lens dropped in some future build leaves a string nothing answers to. It reads
    /// as `Most recent` rather than trapping: an unreadable preference must not be a
    /// device that cannot draw its board.
    func testUnknownStoredLensFallsBackToMostRecent() throws {
        let defaults = try scratch()
        defaults.set("bogus", forKey: TodayViewPreferences.strandsLensKey)
        XCTAssertEqual(TodayViewPreferences(defaults: defaults).strandsLens, .mostRecent)
    }

    /// THE REASON THE CHOSEN KEY IS STORED AND THE EFFECTIVE ONE IS NOT. A board with no
    /// snapshot cannot offer `Tree`, so it DRAWS `Most recent` while still HOLDING the
    /// choice, and returns to `Tree` the moment a board that serves parents lands. A
    /// shell that wrote `effectiveSortKey` back would have replaced the stored `Tree`
    /// with `Most recent` in the gap.
    func testStoredTreeIsKeptWhileTheBoardCannotOfferIt() async {
        let board = StrandsSnapshot(strands: [strand("Jesse"), strand("Tag1")])
        let model = StrandsModel(makeClient: { FixedStrands(snapshot: board) })
        model.sortKey = .tree

        XCTAssertNil(model.snapshot)
        XCTAssertEqual(model.effectiveSortKey, .mostRecent, "nothing to build a tree from yet")
        XCTAssertEqual(model.sortKey, .tree, "but the choice is untouched")

        await model.load()

        XCTAssertEqual(model.effectiveSortKey, .tree)
        XCTAssertEqual(model.sortKey, .tree)
        XCTAssertNotNil(model.groups.first?.treeRows)
    }
}

// MARK: - Fakes

private struct FixedStrands: StrandsProviding {
    let snapshot: StrandsSnapshot
    func getStrands(ifNoneMatch: String?) async throws -> StrandsFetchResult { .snapshot(snapshot) }
    func getStrand(slug: String) async throws -> StrandDetail { StrandDetail(markdown: "# \(slug)") }
}

private struct NeverStrands: StrandsProviding {
    func getStrands(ifNoneMatch: String?) async throws -> StrandsFetchResult { .notModified }
    func getStrand(slug: String) async throws -> StrandDetail { StrandDetail(markdown: "") }
}
