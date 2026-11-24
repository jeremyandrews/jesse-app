import XCTest
@testable import JesseNetworking

/// `awaiting_merge` (DJ12): a run whose only remaining blocker is its owner merging a pull
/// request, carrying `merge_wait`. `studio-run-awaiting-merge.json` is the bridge's own
/// serialization of run GD5 waiting on jeremyandrews/jesse-app#362, rewritten by
/// `JESSE_RUN_FIXTURE_WRITE=1 cargo test the_app_awaiting_merge_fixture`; these tests read only
/// what that run always serves.
final class StudioRunsAwaitingMergeWireTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)",
                                                  withExtension: "json"))
        return try Data(contentsOf: url)
    }

    /// The fixture with `edit` applied to its top level object, re-serialized.
    private func edited(_ edit: (inout [String: Any]) -> Void) throws -> Data {
        var object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: fixture("studio-run-awaiting-merge"))
                as? [String: Any])
        edit(&object)
        return try JSONSerialization.data(withJSONObject: object)
    }

    func testAwaitingMergeDecodesAndEncodesItsWireWord() throws {
        XCTAssertEqual(StudioRunStatus(wire: "awaiting_merge"), .awaitingMerge)
        XCTAssertEqual(StudioRunStatus.awaitingMerge.wire, "awaiting_merge")
        let decoded = try JSONDecoder().decode([StudioRunStatus].self,
                                               from: Data(#"["awaiting_merge"]"#.utf8))
        XCTAssertEqual(decoded, [.awaitingMerge])
        let encoded = try JSONEncoder().encode([StudioRunStatus.awaitingMerge])
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), #"["awaiting_merge"]"#)
    }

    /// The bridge's predicates for `awaiting_merge`: grouped with `needs_you` (live, active,
    /// needs me, not moving, not archivable, not cancellable).
    func testAwaitingMergeIsGroupedWithNeedsYouInEveryPredicate() {
        let s = StudioRunStatus.awaitingMerge
        XCTAssertFalse(s.isMoving)
        XCTAssertTrue(s.isLive)
        XCTAssertTrue(s.isActive)
        XCTAssertFalse(s.isFinished)
        XCTAssertTrue(s.needsMe)
        XCTAssertFalse(s.isArchivable)
        XCTAssertFalse(s.isCancellable)
        let needsYou = StudioRunStatus.needsYou
        XCTAssertEqual([s.isMoving, s.isLive, s.isActive, s.isFinished, s.needsMe,
                        s.isArchivable, s.isCancellable],
                       [needsYou.isMoving, needsYou.isLive, needsYou.isActive,
                        needsYou.isFinished, needsYou.needsMe, needsYou.isArchivable,
                        needsYou.isCancellable])
    }

    func testAnAwaitingMergeRunDecodesWithItsMergeWait() throws {
        let run = try StudioRun.decode(from: fixture("studio-run-awaiting-merge"))
        XCTAssertEqual(run.runID, "GD5")
        XCTAssertEqual(run.status, .awaitingMerge)
        XCTAssertFalse(run.isArchived)
        let wait = try XCTUnwrap(run.mergeWait)
        XCTAssertEqual(wait.url, "https://github.com/jeremyandrews/jesse-app/pull/362")
        XCTAssertEqual(wait.owner, "jeremyandrews")
        XCTAssertEqual(wait.repo, "jesse-app")
        XCTAssertEqual(wait.number, 362)
        XCTAssertEqual(wait.title,
                       "Bridge 0.217.1: read a quoted devjob-priority followed by a comment")
        // The times are when the bridge recorded the fixture: present, never pinned.
        XCTAssertNotNil(wait.since)
        XCTAssertNotNil(wait.lastCheckAt)
        XCTAssertNotNil(wait.lastPushAt)
        XCTAssertNil(wait.ended)
        XCTAssertNil(wait.endedAt)
        XCTAssertFalse(wait.hasEnded)
        XCTAssertEqual(wait.reference, "jeremyandrews/jesse-app#362")
        XCTAssertEqual(wait.webURL?.absoluteString,
                       "https://github.com/jeremyandrews/jesse-app/pull/362")
        XCTAssertEqual(run.rounds.last?.outcome, .awaitingMerge)
    }

    /// The optional keys may be absent or null; an ended wait carries how it ended.
    func testAMinimalAndAnEndedMergeWaitDecode() throws {
        let minimal = try StudioRun.decode(from: edited {
            $0["merge_wait"] = ["url": "https://github.com/jeremyandrews/k3s/pull/412",
                                "last_check_at": NSNull(), "last_push_at": NSNull()]
        })
        let wait = try XCTUnwrap(minimal.mergeWait)
        XCTAssertNil(wait.owner)
        XCTAssertNil(wait.number)
        XCTAssertNil(wait.title)
        XCTAssertNil(wait.lastCheckAt)
        XCTAssertNil(wait.lastPushAt)
        XCTAssertEqual(wait.reference, "jeremyandrews/k3s#412", "read from the URL")

        let ended = try StudioRun.decode(from: edited {
            $0["status"] = "queued"
            var w = $0["merge_wait"] as? [String: Any] ?? [:]
            w["ended"] = "merged at 2026-11-24T10:00:00Z as abc123"
            w["ended_at"] = "2026-11-24T10:00:00Z"
            $0["merge_wait"] = w
        })
        XCTAssertEqual(ended.status, .queued)
        XCTAssertEqual(ended.mergeWait?.ended, "merged at 2026-11-24T10:00:00Z as abc123")
        XCTAssertEqual(ended.mergeWait?.endedAt, "2026-11-24T10:00:00Z")
        XCTAssertEqual(ended.mergeWait?.hasEnded, true)
    }

    /// A malformed `merge_wait` never fails the run: it is simply none.
    func testAMalformedMergeWaitDecodesToNilAndTheRunStillDecodes() throws {
        let malformed: [Any] = [
            "https://github.com/jeremyandrews/jesse-app/pull/362",
            ["owner": "jeremyandrews", "number": 362] as [String: Any],
            ["url": 362] as [String: Any],
            [1, 2, 3],
        ]
        for value in malformed {
            let run = try StudioRun.decode(from: edited { $0["merge_wait"] = value })
            XCTAssertEqual(run.runID, "GD5", "\(value)")
            XCTAssertEqual(run.status, .awaitingMerge)
            XCTAssertNil(run.mergeWait, "\(value)")
        }
        // A wrong typed optional key loses only that key.
        let odd = try StudioRun.decode(from: edited {
            $0["merge_wait"] = ["url": "https://github.com/o/r/pull/7", "number": "seven",
                                "title": 12] as [String: Any]
        })
        XCTAssertEqual(odd.mergeWait?.url, "https://github.com/o/r/pull/7")
        XCTAssertNil(odd.mergeWait?.number)
        XCTAssertNil(odd.mergeWait?.title)
        XCTAssertEqual(odd.mergeWait?.reference, "o/r#7")
    }

    /// Every older record decodes as before, with no merge wait.
    func testOlderRecordsDecodeWithoutAMergeWait() throws {
        for name in ["studio-run-needs-you", "studio-run-needs-you-served", "studio-run-paused",
                     "studio-run-reviewing-served", "studio-run-reviewed-served",
                     "studio-run-two-reviews-served", "cloud-run-detail"] {
            let run = try StudioRun.decode(from: fixture(name))
            XCTAssertNil(run.mergeWait, name)
            XCTAssertNotEqual(run.status, .awaitingMerge, name)
        }
        for name in ["studio-runs-list", "studio-runs-list-archived", "studio-runs-list-0.203",
                     "studio-runs-list-cloud"] {
            let list = try StudioRunList.decode(from: fixture(name))
            XCTAssertFalse(list.runs.isEmpty, name)
            XCTAssertTrue(list.runs.allSatisfy { $0.mergeWait == nil }, name)
        }
    }

    /// A status this build has never heard of is still `.unknown`, never a failed decode.
    func testAnUnknownStatusStillDecodesAsUnknown() throws {
        let run = try StudioRun.decode(from: edited { $0["status"] = "awaiting_review_merge" })
        XCTAssertEqual(run.status, .unknown("awaiting_review_merge"))
        XCTAssertEqual(StudioRunStatus(wire: "awaiting-merge"), .unknown("awaiting-merge"))
        XCTAssertEqual(StudioRunStatus(wire: "AWAITING_MERGE"), .unknown("AWAITING_MERGE"))
        XCTAssertNotNil(run.mergeWait, "the merge wait decodes whatever the status")
    }

    /// A list carrying an awaiting-merge run beside the others.
    func testAListWithAnAwaitingMergeRunDecodesIt() throws {
        var list = try XCTUnwrap(JSONSerialization.jsonObject(
            with: fixture("studio-runs-list")) as? [String: Any])
        let run = try XCTUnwrap(JSONSerialization.jsonObject(
            with: fixture("studio-run-awaiting-merge")))
        list["runs"] = ((list["runs"] as? [Any]) ?? []) + [run]
        let decoded = try StudioRunList.decode(
            from: JSONSerialization.data(withJSONObject: list))
        let waiting = try XCTUnwrap(decoded.runs.last { $0.runID == "GD5" })
        XCTAssertEqual(waiting.status, .awaitingMerge)
        XCTAssertEqual(waiting.mergeWait?.number, 362)
    }
}
