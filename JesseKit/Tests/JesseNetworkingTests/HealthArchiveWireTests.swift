import XCTest
@testable import JesseNetworking

/// The health archive's request shape, which the bridge's `parse_ingest` validates exactly:
/// batch sizes, series numbering, metadata conversion, deletions, timestamps and route
/// thinning. The catalogue's match against the app's HealthKit read set is the app's
/// `HealthArchiveCatalogueTests`, because only the app can see that set.
final class HealthArchiveWireTests: XCTestCase {
    private let rome = TimeZone(identifier: "Europe/Rome")!
    private let start = Date(timeIntervalSince1970: 1_791_000_000)

    private func sample(_ i: Int) -> HealthArchiveQuantitySample {
        HealthArchiveQuantitySample(uuid: "u\(i)", start: "s", end: "e", value: Double(i),
                                    unit: "count/min", source: "com.apple.health",
                                    sourceName: "Watch", device: nil)
    }

    private func json(_ data: Data) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    // MARK: Batching

    func testOneSampleIsOneRequest() {
        let b = HealthArchiveBatcher.batches(samples: [sample(0)], deleted: [])
        XCTAssertEqual(b.count, 1)
        XCTAssertEqual(b[0].samples.map(\.uuid), ["u0"])
    }

    func testExactlyTwoThousandSamplesIsOneRequest() {
        let all = (0..<2000).map(sample)
        let b = HealthArchiveBatcher.batches(samples: all, deleted: [])
        XCTAssertEqual(b.count, 1)
        XCTAssertEqual(b[0].samples, all)
    }

    func testTwoThousandAndOneSamplesSplitInOrder() {
        let all = (0..<2001).map(sample)
        let b = HealthArchiveBatcher.batches(samples: all, deleted: [])
        XCTAssertEqual(b.map(\.samples.count), [2000, 1])
        XCTAssertEqual(b.flatMap(\.samples), all, "order is preserved across the split")
    }

    func testNothingToSendIsNoRequest() {
        XCTAssertTrue(HealthArchiveBatcher.batches(samples: [HealthArchiveQuantitySample](),
                                                   deleted: []).isEmpty)
    }

    func testDeletionsTravelWithTheirType() throws {
        let bodies = try HealthArchiveBatcher.requests(
            kind: .quantity, type: "HKQuantityTypeIdentifierHeartRate",
            samples: [sample(0)], deleted: ["d1", "d2"])
        XCTAssertEqual(bodies.count, 1)
        let body = try json(bodies[0])
        XCTAssertEqual(body["schema"] as? Int, 1)
        XCTAssertEqual(body["kind"] as? String, "quantity")
        XCTAssertEqual(body["type"] as? String, "HKQuantityTypeIdentifierHeartRate")
        XCTAssertEqual(body["deleted"] as? [String], ["d1", "d2"])
        XCTAssertEqual((body["samples"] as? [Any])?.count, 1)
    }

    func testDeletionsAloneStillMakeARequestAndSplitLikeSamples() {
        let deleted = (0..<2001).map { "d\($0)" }
        let b = HealthArchiveBatcher.batches(samples: [HealthArchiveCategorySample](),
                                             deleted: deleted)
        XCTAssertEqual(b.map(\.deleted.count), [2000, 1])
        XCTAssertEqual(b.flatMap(\.deleted), deleted)
        XCTAssertTrue(b.allSatisfy { $0.samples.isEmpty })
    }

    // MARK: Series

    func testSeriesExpansionNumbersPointsInOrderUnderOneUUID() {
        let points = (0..<4).map {
            HealthArchiveSeries.Point(start: start.addingTimeInterval(Double($0) * 5),
                                      end: start.addingTimeInterval(Double($0) * 5 + 5),
                                      value: 140 + Double($0))
        }
        let template = HealthArchiveQuantitySample(
            uuid: "SERIES", start: "", end: "", value: 0, unit: "count/min",
            source: "com.apple.health", sourceName: "Watch", device: "Watch8,1")
        let out = HealthArchiveSeries.expand(points, template: template,
                                             time: .init(timeZone: rome))
        XCTAssertEqual(out.map(\.seq), [0, 1, 2, 3])
        XCTAssertEqual(out.map(\.value), [140, 141, 142, 143])
        XCTAssertTrue(out.allSatisfy { $0.uuid == "SERIES" && $0.device == "Watch8,1" })
        XCTAssertEqual(out[1].start, HealthArchiveTime.string(start.addingTimeInterval(5), timeZone: rome))
    }

    // MARK: Metadata

    private struct FakeQuantity { var meters: Double }

    func testMetadataConvertsQuantitiesDatesAndDropsTheRest() {
        let (values, dropped) = HealthArchiveMetadata.convert(
            ["HKElevationAscended": FakeQuantity(meters: 120.5),
             "HKWasUserEntered": NSNumber(value: true),
             "HKHeartRateMotionContext": NSNumber(value: 2),
             "HKTimeZone": "Europe/Rome",
             "When": start,
             "Blob": Data([1, 2, 3])],
            time: .init(timeZone: rome),
            quantity: { ($0 as? FakeQuantity).map { ($0.meters, "m") } })
        XCTAssertEqual(values["HKElevationAscended"],
                       .object(["value": .number(120.5), "unit": .string("m")]))
        XCTAssertEqual(values["HKWasUserEntered"], .bool(true))
        XCTAssertEqual(values["HKHeartRateMotionContext"], .number(2))
        XCTAssertEqual(values["HKTimeZone"], .string("Europe/Rome"))
        XCTAssertEqual(values["When"], .string(HealthArchiveTime.string(start, timeZone: rome)))
        XCTAssertNil(values["Blob"])
        XCTAssertEqual(dropped, 1)
    }

    func testMetadataSurvivesTheEncoderAsJSONObjects() throws {
        var s = sample(0)
        s.metadata = ["q": .object(["value": .number(1.5), "unit": .string("m")]), "b": .bool(false)]
        let body = try json(HealthArchiveRequest(kind: .quantity, type: "t", samples: [s],
                                                 deleted: []).encoded())
        let meta = try XCTUnwrap(((body["samples"] as? [[String: Any]])?.first?["metadata"]) as? [String: Any])
        XCTAssertEqual((meta["q"] as? [String: Any])?["unit"] as? String, "m")
        XCTAssertEqual(meta["b"] as? Bool, false)
    }

    // MARK: Time

    func testTimestampsCarryANumericOffsetEvenAtUTC() {
        let utc = HealthArchiveTime.string(start, timeZone: TimeZone(identifier: "UTC")!)
        XCTAssertTrue(utc.hasSuffix("+00:00"), utc)
        let summer = HealthArchiveTime.string(Date(timeIntervalSince1970: 1_791_540_000),
                                              timeZone: rome)
        XCTAssertTrue(summer.range(of: #"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}\+02:00$"#,
                                   options: .regularExpression) != nil, summer)
        let parsed = ISO8601DateFormatter()
        parsed.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        XCTAssertEqual(parsed.date(from: summer), Date(timeIntervalSince1970: 1_791_540_000))
    }

    // MARK: Routes

    private func point(_ i: Int, hAcc: Double = 5, vAcc: Double = 3, speed: Double = 2.5,
                       course: Double = 90) -> HealthArchiveRoutePoint? {
        HealthArchiveRoutePoint(t: "t\(i)", lat: 45 + Double(i) * 1e-6, lon: 9, altitude: 120,
                                horizontalAccuracy: hAcc, verticalAccuracy: vAcc,
                                speed: speed, course: course)
    }

    func testALongRouteIsThinnedKeepingFirstAndLast() throws {
        let points = (0..<250_000).compactMap { point($0) }
        let route = HealthArchiveRoute(uuid: "R", workoutUUID: "W", source: "s",
                                       sourceName: "n", points: points)
        XCTAssertEqual(route.points.count, 100_000)
        XCTAssertTrue(route.downsampled)
        XCTAssertEqual(route.points.first, points.first)
        XCTAssertEqual(route.points.last, points.last)
        let order = route.points.map(\.t).map { Int($0.dropFirst())! }
        XCTAssertEqual(order, order.sorted(), "thinning keeps time order")
        XCTAssertEqual(Set(order).count, order.count, "no point is picked twice")
    }

    func testAShortRouteIsSentWhole() {
        let points = (0..<10).compactMap { point($0) }
        let route = HealthArchiveRoute(uuid: "R", workoutUUID: "W", source: "s",
                                       sourceName: "n", points: points)
        XCTAssertEqual(route.points, points)
        XCTAssertFalse(route.downsampled)
    }

    func testInvalidAccuracySpeedAndCourseBecomeNull() throws {
        let p = try XCTUnwrap(point(0, hAcc: -1, vAcc: -1, speed: -1, course: -1))
        XCTAssertNil(p.hAcc); XCTAssertNil(p.vAcc); XCTAssertNil(p.speed); XCTAssertNil(p.course)
        XCTAssertNil(p.alt, "an altitude with invalid vertical accuracy is no altitude")
        let encoded = String(decoding: try JSONEncoder().encode(p), as: UTF8.self)
        for key in ["h_acc", "v_acc", "speed", "course", "alt"] {
            XCTAssertTrue(encoded.contains("\"\(key)\":null"), "\(key) is an explicit null: \(encoded)")
        }
    }

    func testAnImpossibleCoordinateIsDroppedNotSent() {
        XCTAssertNil(HealthArchiveRoutePoint(t: "t", lat: 91, lon: 0, altitude: 0,
                                             horizontalAccuracy: 1, verticalAccuracy: 1,
                                             speed: 0, course: 0))
        XCTAssertNil(HealthArchiveRoutePoint(t: "t", lat: .nan, lon: 0, altitude: 0,
                                             horizontalAccuracy: 1, verticalAccuracy: 1,
                                             speed: 0, course: 0))
    }

    func testARouteRequestHasTheContractShape() throws {
        let route = HealthArchiveRoute(uuid: "R", workoutUUID: "W", source: "s", sourceName: "n",
                                       points: [point(0)!])
        let body = try json(HealthArchiveRequest(kind: .route, type: HealthArchiveCatalogue.routeType,
                                                 samples: [route], deleted: []).encoded())
        let s = try XCTUnwrap((body["samples"] as? [[String: Any]])?.first)
        XCTAssertEqual(s["workout_uuid"] as? String, "W")
        XCTAssertEqual(s["downsampled"] as? Bool, false)
        XCTAssertEqual((s["points"] as? [[String: Any]])?.first?["h_acc"] as? Double, 5)
    }

    // MARK: Responses

    func testOutcomesFollowTheContract() {
        XCTAssertEqual(HealthIngestOutcome.from(status: 200, body: Data(#"{"upserted":1,"deleted":0}"#.utf8)), .committed)
        XCTAssertEqual(HealthIngestOutcome.from(status: 503, body: Data(#"{"error":"not_configured"}"#.utf8)), .notConfigured)
        XCTAssertEqual(HealthIngestOutcome.from(status: 503, body: Data(#"{"error":"unavailable"}"#.utf8)), .unavailable(503))
        XCTAssertEqual(HealthIngestOutcome.from(status: 400, body: Data()), .rejected(400))
        XCTAssertEqual(HealthIngestOutcome.from(status: 401, body: Data()), .unauthorized)
    }
}
