import XCTest
@testable import Jesse

/// The route elevation cache's policy, with the HealthKit read replaced by closures:
/// a late read lands for the next gather, a hit reads nothing, overlapping misses
/// share one read, "no routes" is retried while "too few readings" is not, and the
/// file survives a reload and is pruned to the window.
final class RouteElevationCacheTests: XCTestCase {

    private let profile = RouteElevation(ascentM: 412, descentM: 405,
                                         minAltitudeM: 261, maxAltitudeM: 348)
    private let workoutEnd = Date(timeIntervalSince1970: 1_790_000_000)
    private var fileURL: URL!

    override func setUp() {
        super.setUp()
        fileURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("route-cache-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent("route-elevation.json")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent())
        super.tearDown()
    }

    /// Counts reads, and can be slowed with a delay that IGNORES cancellation, the
    /// way a HealthKit query behind a continuation does.
    private final class Reader: @unchecked Sendable {
        private let lock = NSLock()
        private var _count = 0
        var count: Int { lock.withLock { _count } }
        let result: RouteReadResult
        let delay: TimeInterval
        init(_ result: RouteReadResult, delay: TimeInterval = 0) {
            self.result = result
            self.delay = delay
        }
        func read() async -> RouteReadResult {
            lock.withLock { _count += 1 }
            guard delay > 0 else { return result }
            return await withCheckedContinuation { cont in
                DispatchQueue.global().asyncAfter(deadline: .now() + delay) {
                    cont.resume(returning: self.result)
                }
            }
        }
    }

    private func cache(now: Date? = nil,
                       report: @escaping @Sendable (RouteReadReport) -> Void = { _ in })
        -> RouteElevationCache {
        let fixed = now ?? workoutEnd.addingTimeInterval(3600)
        return RouteElevationCache(fileURL: fileURL, now: { fixed }, report: report)
    }

    func testLateResultIsAbsentFromTheFirstGatherAndPresentFromTheSecond() async {
        let c = cache()
        let id = UUID()
        let slow = Reader(.reduced(profile, seriesCount: 1, pointCount: 12_600), delay: 0.3)
        let first = await c.elevation(for: id, workoutEnd: workoutEnd,
                                      within: .milliseconds(50)) { await slow.read() }
        XCTAssertNil(first, "the bound fires before the read finishes")
        try? await Task.sleep(for: .milliseconds(600))
        let never = Reader(.failed)
        let second = await c.elevation(for: id, workoutEnd: workoutEnd,
                                       within: .milliseconds(50)) { await never.read() }
        XCTAssertEqual(second, profile, "the late result landed in the cache")
        XCTAssertEqual(slow.count, 1)
        XCTAssertEqual(never.count, 0)
    }

    func testACacheHitPerformsNoRead() async {
        let c = cache()
        let id = UUID()
        let fast = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        let first = await c.elevation(for: id, workoutEnd: workoutEnd,
                                      within: .seconds(2)) { await fast.read() }
        XCTAssertEqual(first, profile)
        let again = Reader(.reduced(nil, seriesCount: 1, pointCount: 3))
        for _ in 0..<3 {
            let hit = await c.elevation(for: id, workoutEnd: workoutEnd,
                                        within: .seconds(2)) { await again.read() }
            XCTAssertEqual(hit, profile)
        }
        XCTAssertEqual(fast.count, 1)
        XCTAssertEqual(again.count, 0)
    }

    func testOverlappingGathersOnAMissStartExactlyOneRead() async {
        let c = cache()
        let id = UUID()
        let slow = Reader(.reduced(profile, seriesCount: 1, pointCount: 12_600), delay: 0.3)
        let end = workoutEnd
        async let a = c.elevation(for: id, workoutEnd: end,
                                  within: .milliseconds(50)) { await slow.read() }
        async let b = c.elevation(for: id, workoutEnd: end,
                                  within: .seconds(2)) { await slow.read() }
        let (ra, rb) = await (a, b)
        XCTAssertNil(ra, "the short wait gives up")
        XCTAssertEqual(rb, profile, "the long wait joins the same read and gets its value")
        XCTAssertEqual(slow.count, 1, "one read for both gathers")
    }

    func testNoRouteSeriesIsNotCachedAndIsRetried() async {
        let c = cache()
        let id = UUID()
        let none = Reader(.noRoutes)
        let first = await c.elevation(for: id, workoutEnd: workoutEnd,
                                      within: .seconds(2)) { await none.read() }
        XCTAssertNil(first)
        let saved = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        let second = await c.elevation(for: id, workoutEnd: workoutEnd,
                                       within: .seconds(2)) { await saved.read() }
        XCTAssertEqual(second, profile, "a route saved after the workout is found later")
        XCTAssertEqual(saved.count, 1)
    }

    func testAFailedReadIsNotCached() async {
        let c = cache()
        let id = UUID()
        let broken = Reader(.failed)
        _ = await c.elevation(for: id, workoutEnd: workoutEnd,
                              within: .seconds(2)) { await broken.read() }
        let retry = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        let second = await c.elevation(for: id, workoutEnd: workoutEnd,
                                       within: .seconds(2)) { await retry.read() }
        XCTAssertEqual(second, profile)
    }

    func testRoutesWithTooFewReadingsAreCachedAsNegative() async {
        let c = cache()
        let id = UUID()
        let sparse = Reader(.reduced(nil, seriesCount: 1, pointCount: 4))
        let first = await c.elevation(for: id, workoutEnd: workoutEnd,
                                      within: .seconds(2)) { await sparse.read() }
        XCTAssertNil(first)
        let again = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        let second = await c.elevation(for: id, workoutEnd: workoutEnd,
                                       within: .seconds(2)) { await again.read() }
        XCTAssertNil(second, "the negative is cached")
        XCTAssertEqual(again.count, 0, "and not re-read")
    }

    func testTheCacheSurvivesAReloadFromDisk() async {
        let id = UUID(), sparseID = UUID()
        do {
            let c = cache()
            let fast = Reader(.reduced(profile, seriesCount: 2, pointCount: 12_600))
            _ = await c.elevation(for: id, workoutEnd: workoutEnd,
                                  within: .seconds(2)) { await fast.read() }
            let sparse = Reader(.reduced(nil, seriesCount: 1, pointCount: 4))
            _ = await c.elevation(for: sparseID, workoutEnd: workoutEnd,
                                  within: .seconds(2)) { await sparse.read() }
        }
        let reloaded = cache()
        let never = Reader(.failed)
        let hit = await reloaded.elevation(for: id, workoutEnd: workoutEnd,
                                           within: .seconds(2)) { await never.read() }
        let negative = await reloaded.elevation(for: sparseID, workoutEnd: workoutEnd,
                                                within: .seconds(2)) { await never.read() }
        XCTAssertEqual(hit, profile)
        XCTAssertNil(negative)
        XCTAssertEqual(never.count, 0, "both came from the file")
    }

    /// The file holds the four numbers and dates, and nothing shaped like a route.
    func testTheFileHoldsOnlyTheFourNumbers() async throws {
        let c = cache()
        let fast = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        _ = await c.elevation(for: UUID(), workoutEnd: workoutEnd,
                              within: .seconds(2)) { await fast.read() }
        let object = try JSONSerialization.jsonObject(with: Data(contentsOf: fileURL))
        let entry = try XCTUnwrap((object as? [String: [String: Any]])?.values.first)
        XCTAssertEqual(Set(entry.keys), ["elevation", "computedAt", "workoutEnd"])
        let elevation = try XCTUnwrap(entry["elevation"] as? [String: Any])
        XCTAssertEqual(Set(elevation.keys), ["ascentM", "descentM", "minAltitudeM", "maxAltitudeM"])
    }

    func testPruningDropsWorkoutsOutsideTheWindow() async {
        let old = UUID(), recent = UUID()
        let recentEnd = workoutEnd.addingTimeInterval(RouteElevationCache.retention)
        do {
            let c = cache(now: recentEnd)
            let fast = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
            _ = await c.elevation(for: old, workoutEnd: workoutEnd,
                                  within: .seconds(2)) { await fast.read() }
            _ = await c.elevation(for: recent, workoutEnd: recentEnd,
                                  within: .seconds(2)) { await fast.read() }
        }
        // An hour later the first workout ended more than the retention ago.
        let later = cache(now: recentEnd.addingTimeInterval(3600))
        let reread = Reader(.reduced(nil, seriesCount: 1, pointCount: 4))
        let pruned = await later.elevation(for: old, workoutEnd: workoutEnd,
                                           within: .seconds(2)) { await reread.read() }
        XCTAssertNil(pruned)
        XCTAssertEqual(reread.count, 1, "the old entry was dropped, so it is read again")
        let never = Reader(.failed)
        let kept = await later.elevation(for: recent, workoutEnd: recentEnd,
                                         within: .seconds(2)) { await never.read() }
        XCTAssertEqual(kept, profile)
        XCTAssertEqual(never.count, 0)
    }

    /// Every finished read is reported with its sizes and whether it beat the bound.
    func testEachReadIsReportedWithSizesAndTiming() async {
        let box = ReportBox()
        let c = cache(report: { box.append($0) })
        let slow = Reader(.reduced(profile, seriesCount: 2, pointCount: 12_600), delay: 0.2)
        _ = await c.elevation(for: UUID(), workoutEnd: workoutEnd,
                              within: .milliseconds(20)) { await slow.read() }
        let fast = Reader(.reduced(profile, seriesCount: 1, pointCount: 1800))
        _ = await c.elevation(for: UUID(), workoutEnd: workoutEnd,
                              within: .seconds(2)) { await fast.read() }
        try? await Task.sleep(for: .milliseconds(500))
        let reports = box.all.sorted { $0.pointCount < $1.pointCount }
        XCTAssertEqual(reports.count, 2)
        XCTAssertEqual(reports.map(\.pointCount), [1800, 12_600])
        XCTAssertEqual(reports.map(\.seriesCount), [1, 2])
        XCTAssertEqual(reports.map(\.withinBound), [true, false])
        XCTAssertEqual(reports.map(\.cached), [true, true])
        XCTAssertGreaterThanOrEqual(reports[1].milliseconds, 150)
    }

    private final class ReportBox: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [RouteReadReport] = []
        func append(_ r: RouteReadReport) { lock.withLock { items.append(r) } }
        var all: [RouteReadReport] { lock.withLock { items } }
    }
}
