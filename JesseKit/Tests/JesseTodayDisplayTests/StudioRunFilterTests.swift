import XCTest
import JesseNetworking
@testable import JesseTodayDisplay

/// The runs list's filter bar and archiving: which runs each filter shows, the counts on the
/// segments, the remembered filter, where Archive is offered, Undo, and Archive all finished
/// touching only the runs it shows.
@MainActor
final class StudioRunFilterTests: XCTestCase {

    private static let statuses: [StudioRunStatus] = [
        .queued, .running, .paused, .needsYou, .interrupted, .failed, .reported, .cancelled,
        .closed, .unknown("rebasing"),
    ]

    private func run(_ id: String, _ status: StudioRunStatus, strand: String? = "Gd",
                     archivedAt: String? = nil) -> StudioRun {
        StudioRun(runID: id, strand: strand, itemID: id, status: status,
                  createdAt: "2026-11-09T10:00:00.000Z", title: "Run \(id)",
                  archivedAt: archivedAt)
    }

    /// One run of every status for `Gd`, a reported and a needs_you run of `Other`.
    private func fake() -> FakeRuns {
        let fake = FakeRuns()
        fake.list = StudioRunList(
            runs: Self.statuses.map { run(StudioRunSemantics.label($0), $0) }
                + [run("OtherDone", .reported, strand: "Other"),
                   run("OtherAsk", .needsYou, strand: "Other")],
            counts: StudioRunCounts(needsMe: 4, moving: 3, finished: 5, archived: 7))
        fake.archivedList = StudioRunList(
            runs: [run("Old", .reported, archivedAt: "2026-11-01T10:00:00.000Z"),
                   run("OtherOld", .cancelled, strand: "Other",
                       archivedAt: "2026-11-02T10:00:00.000Z")],
            counts: fake.list.counts)
        return fake
    }

    private func ids(_ runs: [StudioRun]) -> Set<String> { Set(runs.map(\.runID)) }

    // MARK: - Membership

    func testEachFilterShowsItsStatusesAndAllIsTheThreeTogether() async {
        let model = StudioRunsModel(makeClient: { [fake = fake()] in fake })
        await model.load()
        XCTAssertEqual(ids(model.runs(filter: .needsMe, strand: "Gd")),
                       ["Needs you", "Interrupted", "Failed"])
        XCTAssertEqual(ids(model.runs(filter: .moving, strand: "Gd")),
                       ["Queued", "Running", "Paused"])
        XCTAssertEqual(ids(model.runs(filter: .finished, strand: "Gd")),
                       ["Reported", "Cancelled", "Closed", "rebasing"])
        let all = model.runs(filter: .all, strand: "Gd")
        XCTAssertEqual(all.count, Self.statuses.count, "every status is under All")
        XCTAssertEqual(ids(all), ids(model.runs(filter: .needsMe, strand: "Gd"))
            .union(ids(model.runs(filter: .moving, strand: "Gd")))
            .union(ids(model.runs(filter: .finished, strand: "Gd"))),
                       "the three filters split All with nothing left over")
        // Every project's: Other's runs too.
        XCTAssertTrue(ids(model.runs(filter: .needsMe, strand: nil)).contains("OtherAsk"))
        XCTAssertFalse(ids(model.runs(filter: .needsMe, strand: "Gd")).contains("OtherAsk"))
        // Archived is empty until it is loaded, then holds only archived runs.
        XCTAssertTrue(model.runs(filter: .archived, strand: nil).isEmpty)
        await model.loadArchived()
        XCTAssertEqual(ids(model.runs(filter: .archived, strand: nil)), ["Old", "OtherOld"])
        XCTAssertEqual(ids(model.runs(filter: .archived, strand: "Gd")), ["Old"])
        XCTAssertTrue(model.runs(filter: .all, strand: nil).allSatisfy { !$0.isArchived })
    }

    func testAnArchivedRunAnOlderBridgeServesInTheDefaultListIsNotUnderAll() {
        let archived = run("A", .reported, archivedAt: "2026-11-01T10:00:00.000Z")
        for f in [StudioRunFilter.needsMe, .moving, .finished, .all] {
            XCTAssertFalse(f.includes(archived), "\(f)")
        }
        XCTAssertTrue(StudioRunFilter.archived.includes(archived))
        XCTAssertFalse(StudioRunFilter.archived.includes(run("B", .reported)))
    }

    func testAnEmptyFilterSaysSoInOnePlainLine() {
        XCTAssertEqual(StudioRunFilter.needsMe.emptyLine, "Nothing needs you.")
        for f in StudioRunFilter.allCases {
            XCTAssertFalse(f.emptyLine.isEmpty)
            XCTAssertFalse(f.emptyLine.contains("\n"))
        }
    }

    // MARK: - Counts

    func testTheWholeListShowsTheBridgesCountsAndAProjectCountsItsOwn() async {
        let model = StudioRunsModel(makeClient: { [fake = fake()] in fake })
        XCTAssertNil(model.count(filter: .needsMe, strand: "Gd"), "nothing before a load")
        await model.load()
        XCTAssertEqual(model.count(filter: .needsMe, strand: nil), 4)
        XCTAssertEqual(model.count(filter: .moving, strand: nil), 3)
        XCTAssertEqual(model.count(filter: .finished, strand: nil), 5)
        XCTAssertEqual(model.count(filter: .all, strand: nil), 12)
        XCTAssertEqual(model.count(filter: .archived, strand: nil), 7)
        XCTAssertEqual(model.count(filter: .needsMe, strand: "Gd"), 3)
        XCTAssertEqual(model.count(filter: .finished, strand: "Gd"), 4)
        XCTAssertEqual(model.count(filter: .all, strand: "Gd"), Self.statuses.count)
        XCTAssertNil(model.count(filter: .archived, strand: "Gd"), "unknown until loaded")
        await model.loadArchived()
        XCTAssertEqual(model.count(filter: .archived, strand: "Gd"), 1)
        XCTAssertEqual(StudioRunFilter.needsMe.label(count: 4), "Needs me 4")
        XCTAssertEqual(StudioRunFilter.all.label(count: nil), "All")
    }

    func testAnOlderBridgesListIsCountedFromTheRunsItServes() async {
        let fake = FakeRuns()
        fake.list = StudioRunList(runs: [run("A", .needsYou), run("B", .reported),
                                         run("C", .reported)])
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        XCTAssertEqual(model.count(filter: .needsMe, strand: nil), 1)
        XCTAssertEqual(model.count(filter: .finished, strand: nil), 2)
        XCTAssertEqual(model.count(filter: .all, strand: nil), 3)
        XCTAssertNil(model.count(filter: .archived, strand: nil))
    }

    // MARK: - The remembered filter

    func testTheFilterIsRememberedUnderOneKeyAndDefaultsToAll() throws {
        XCTAssertEqual(StudioRunFilter.initial, .all)
        XCTAssertEqual(StudioRunFilter.restored(nil), .all)
        XCTAssertEqual(StudioRunFilter.restored("bogus"), .all)
        let suite = "StudioRunFilterTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        for f in StudioRunFilter.allCases {
            // What `@AppStorage(StudioRunFilter.storageKey)` writes for a String enum.
            defaults.set(f.rawValue, forKey: StudioRunFilter.storageKey)
            XCTAssertEqual(StudioRunFilter.restored(
                defaults.string(forKey: StudioRunFilter.storageKey)), f)
        }
        XCTAssertEqual(StudioRunFilter.segments, [.needsMe, .moving, .finished, .all])
        XCTAssertFalse(StudioRunFilter.segments.contains(.archived))
    }

    // MARK: - Where Archive is offered

    func testArchiveIsOfferedOnEveryRowThatIsNotMovingAndUnarchiveOnArchivedRowsOnly() {
        for status in Self.statuses {
            let r = run("X", status)
            XCTAssertEqual(StudioRunSemantics.canArchive(r), !status.isMoving, "\(status)")
            XCTAssertFalse(StudioRunSemantics.canUnarchive(r), "\(status)")
        }
        let archived = run("X", .reported, archivedAt: "2026-11-01T10:00:00.000Z")
        XCTAssertFalse(StudioRunSemantics.canArchive(archived))
        XCTAssertTrue(StudioRunSemantics.canUnarchive(archived))
    }

    func testTheDetailOffersArchiveExceptOnAMovingRunAndUnarchiveOnAnArchivedOne() async {
        let fake = fake()
        for status in Self.statuses {
            let m = StudioRunDetailModel(runID: "X", run: run("X", status), makeClient: { fake })
            XCTAssertEqual(m.canArchive, !status.isMoving, "\(status)")
            XCTAssertFalse(m.canUnarchive)
        }
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        let detail = model.detailModel(for: "Reported")
        XCTAssertTrue(detail.canArchive)
        await detail.archive()
        XCTAssertTrue(detail.canUnarchive)
        XCTAssertFalse(detail.canArchive)
        XCTAssertFalse(ids(model.runs).contains("Reported"), "the list caught up")
        await detail.unarchive()
        XCTAssertTrue(detail.canArchive)
        XCTAssertTrue(ids(model.runs).contains("Reported"))
    }

    // MARK: - Archiving from the list

    func testAnArchiveTakesTheRowOutAtOnceAndUndoPutsItBack() async {
        let fake = fake()
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        let ask = model.runs.first { $0.runID == "Needs you" }!
        await model.archive(ask)
        XCTAssertEqual(fake.archived, ["Needs you"])
        XCTAssertFalse(ids(model.runs).contains("Needs you"))
        XCTAssertEqual(model.undo?.runIDs, ["Needs you"])
        XCTAssertEqual(model.undo?.text, "Archived \u{201C}Run Needs you\u{201D}.")
        await model.undoArchive()
        XCTAssertEqual(fake.unarchived, ["Needs you"])
        XCTAssertNil(model.undo)
        // Back in the list, closed: unarchive clears the flag and nothing else.
        XCTAssertEqual(model.runs.first { $0.runID == "Needs you" }?.status, .closed)
        XCTAssertEqual(StudioRunsModel.undoSeconds, 5)
    }

    func testTheUndoBannerGoesOnlyForTheArchiveItNames() async {
        let fake = fake()
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        await model.archive(model.runs.first { $0.runID == "Reported" }!)
        let first = model.undo!.id
        await model.archive(model.runs.first { $0.runID == "Cancelled" }!)
        model.dismissUndo(first)
        XCTAssertEqual(model.undo?.runIDs, ["Cancelled"], "a stale timer leaves a newer banner")
        model.dismissUndo(model.undo!.id)
        XCTAssertNil(model.undo)
    }

    func testAMovingRowIsNeverArchivedAndARefusalIsShownVerbatimAndTheRowComesBack() async {
        let fake = fake()
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        await model.archive(model.runs.first { $0.runID == "Running" }!)
        XCTAssertEqual(fake.archived, [], "not offered, not sent")
        let refusal = "Failed is running; a queued, running or paused run cannot be archived"
        fake.archiveRefusals["Failed"] = refusal
        await model.archive(model.runs.first { $0.runID == "Failed" }!)
        XCTAssertEqual(model.alert, refusal)
        XCTAssertNil(model.undo)
        XCTAssertTrue(ids(model.runs).contains("Failed"))
    }

    func testUnarchiveFromArchivedPutsTheRunBack() async {
        let fake = fake()
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        await model.loadArchived()
        await model.unarchive(model.runs(filter: .archived, strand: nil).first { $0.runID == "Old" }!)
        XCTAssertEqual(fake.unarchived, ["Old"])
        XCTAssertFalse(ids(model.runs(filter: .archived, strand: nil)).contains("Old"))
        XCTAssertTrue(ids(model.runs(filter: .all, strand: nil)).contains("Old"))
    }

    func testArchiveAllFinishedArchivesOnlyTheRunsShown() async {
        let fake = fake()
        let model = StudioRunsModel(makeClient: { fake })
        await model.load()
        let shown = ids(model.runs(filter: .finished, strand: "Gd"))
        await model.archiveAllFinished(strand: "Gd")
        XCTAssertEqual(Set(fake.archived), shown)
        XCTAssertEqual(fake.archived.count, shown.count)
        XCTAssertFalse(fake.archived.contains("OtherDone"), "another project's run stays")
        for id in ["Needs you", "Interrupted", "Failed", "Queued", "Running", "Paused"] {
            XCTAssertFalse(fake.archived.contains(id), id)
        }
        XCTAssertEqual(model.undo?.text, "Archived 4 runs.")
        XCTAssertEqual(Set(model.undo?.runIDs ?? []), shown)
        XCTAssertTrue(model.runs(filter: .finished, strand: "Gd").isEmpty)
        XCTAssertFalse(model.runs(filter: .finished, strand: nil).isEmpty, "Other's stays")
        XCTAssertEqual(StudioRunsList.archiveAllQuestion(4), "Archive 4 finished runs?")
        XCTAssertEqual(StudioRunsList.archiveAllQuestion(1), "Archive 1 finished run?")
    }
}
