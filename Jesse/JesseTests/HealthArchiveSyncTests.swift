import CoreLocation
import HealthKit
import XCTest
import JesseNetworking
@testable import Jesse

/// The catalogue is the read set: the archive copies exactly what the app may already read.
/// A type added to `HealthContextProvider.readTypes` without a catalogue entry (or the other
/// way round) fails here, and so does a unit HealthKit would refuse for its type, which on
/// the phone is an uncatchable exception rather than a test failure.
final class HealthArchiveCatalogueTests: XCTestCase {
    func testTheCatalogueCoversTheReadSetExactly() {
        let read = Set(HealthContextProvider.readTypes.map(\.identifier))
            .subtracting([HKObjectType.workoutType().identifier,
                          HKSeriesType.workoutRoute().identifier])
        let catalogue = Set(HealthArchiveCatalogue.quantities.map(\.identifier))
            .union(HealthArchiveCatalogue.categories)
        XCTAssertEqual(catalogue.subtracting(read), [], "in the catalogue but not readable")
        XCTAssertEqual(read.subtracting(catalogue), [], "readable but not archived")
        XCTAssertEqual(HealthArchiveCatalogue.quantities.count + HealthArchiveCatalogue.categories.count,
                       catalogue.count, "no type listed twice")
    }

    func testWorkoutsAndRoutesTravelUnderTheirHealthKitIdentifiers() {
        XCTAssertEqual(HealthArchiveCatalogue.workoutType, HKObjectType.workoutType().identifier)
        XCTAssertEqual(HealthArchiveCatalogue.routeType, HKSeriesType.workoutRoute().identifier)
        XCTAssertEqual(HealthArchiveStream.all.count,
                       HealthArchiveCatalogue.quantities.count + HealthArchiveCatalogue.categories.count + 2)
    }

    func testEveryCatalogueUnitIsCompatibleWithItsType() {
        for q in HealthArchiveCatalogue.quantities {
            let type = HKQuantityType(HKQuantityTypeIdentifier(rawValue: q.identifier))
            XCTAssertTrue(type.is(compatibleWith: HKUnit(from: q.unit)), "\(q.identifier) in \(q.unit)")
        }
    }

    func testMetadataQuantitiesBecomeANumberInAStatedUnit() {
        let (values, dropped) = HealthArchiveHealthKitSource.metadata(
            [HKMetadataKeyElevationAscended: HKQuantity(unit: .meterUnit(with: .centi), doubleValue: 12_050),
             HKMetadataKeyWeatherTemperature: HKQuantity(unit: .degreeFahrenheit(), doubleValue: 212),
             "odd": HKQuantity(unit: .millimeterOfMercury(), doubleValue: 120)],
            time: .init())
        XCTAssertEqual(values[HKMetadataKeyElevationAscended],
                       .object(["value": .number(120.5), "unit": .string("m")]))
        guard case .object(let t)? = values[HKMetadataKeyWeatherTemperature],
              case .number(let c)? = t["value"] else { return XCTFail("temperature not converted") }
        XCTAssertEqual(c, 100, accuracy: 1e-9)
        XCTAssertEqual(t["unit"], .string("degC"))
        XCTAssertNil(values["odd"], "a quantity in no listed unit is dropped, not guessed")
        XCTAssertEqual(dropped, 1)
    }

    func testWorkoutEventsMapOntoTheSixWireWords() {
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.lap), .lap)
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.segment), .segment)
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.motionPaused), .pause)
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.motionResumed), .resume)
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.marker), .marker)
        XCTAssertEqual(HealthArchiveHealthKitSource.eventKind(.pauseOrResumeRequest), .other)
    }

    /// Body mass and workouts keep the immediate delivery the chat triggers ask for; the
    /// archive must not slow them to hourly.
    func testTheChatTriggerTypesKeepImmediateDelivery() {
        XCTAssertEqual(HealthArchiveObserver.frequency(for: HKQuantityType(.bodyMass)), .immediate)
        XCTAssertEqual(HealthArchiveObserver.frequency(for: HKObjectType.workoutType()), .immediate)
        XCTAssertEqual(HealthArchiveObserver.frequency(for: HKQuantityType(.heartRate)), .hourly)
        XCTAssertEqual(HealthArchiveObserver.frequency(for: HKSeriesType.workoutRoute()), .hourly)
    }
}

// MARK: - The engine

/// A scripted HealthKit: per stream, the page to return after each anchor. An anchor not in
/// the script is the end of the stream. Records every read.
private final class FakeSource: HealthArchiveSource, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [String: [Data?: HealthArchivePage]] = [:]
    private var failures: [String: Error] = [:]
    private(set) var reads: [(type: String, anchor: Data?)] = []

    func on(_ stream: HealthArchiveStream, after anchor: Data?, _ page: HealthArchivePage) {
        lock.withLock { script[stream.typeIdentifier, default: [:]][anchor] = page }
    }

    func fail(_ stream: HealthArchiveStream, with error: Error) {
        lock.withLock { failures[stream.typeIdentifier] = error }
    }

    func page(_ stream: HealthArchiveStream, after anchor: Data?) async throws -> HealthArchivePage {
        let (page, error): (HealthArchivePage?, Error?) = lock.withLock {
            reads.append((stream.typeIdentifier, anchor))
            return (script[stream.typeIdentifier]?[anchor], failures[stream.typeIdentifier])
        }
        if let error { throw error }
        return page ?? .end
    }

    var readTypes: [String] { lock.withLock { reads.map(\.type) } }
}

/// A scripted bridge: answers each body in turn from a queue (200 once the queue is empty),
/// optionally waiting on a gate first. Records every body.
private final class FakeTransport: @unchecked Sendable {
    private let lock = NSLock()
    private var answers: [Result<HealthIngestOutcome, Error>] = []
    private(set) var bodies: [Data] = []
    var gate: (@Sendable () async -> Void)?

    func answer(_ a: Result<HealthIngestOutcome, Error>...) { lock.withLock { answers += a } }

    var post: HealthArchiveTransport {
        { [self] _, body in
            if let gate { await gate() }
            let next: Result<HealthIngestOutcome, Error> = lock.withLock {
                bodies.append(body)
                return answers.isEmpty ? .success(.committed) : answers.removeFirst()
            }
            return try next.get()
        }
    }

    var count: Int { lock.withLock { bodies.count } }
}

private struct Boom: Error {}

@MainActor
final class HealthArchiveSyncTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suite: String!
    private let hr = HealthArchiveStream.quantity(identifier: "HKQuantityTypeIdentifierHeartRate",
                                                  unit: "count/min")
    private let sleep = HealthArchiveStream.category(identifier: "HKCategoryTypeIdentifierSleepAnalysis")
    private var source: FakeSource!
    private var transport: FakeTransport!
    private var unlocked = true

    override func setUp() {
        super.setUp()
        suite = "HealthArchiveSyncTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        source = FakeSource()
        transport = FakeTransport()
        unlocked = true
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    private func engine(_ streams: [HealthArchiveStream]? = nil) -> HealthArchiveSync {
        HealthArchiveSync(defaults: defaults,
                          configProvider: { JesseConfig(host: "studio", port: 8765, token: "tok") },
                          source: source, transport: transport.post,
                          protectedDataAvailable: { [unowned self] in self.unlocked },
                          isEnabled: { true },
                          streams: streams ?? [hr, sleep])
    }

    private func anchor(_ s: String) -> Data { Data(s.utf8) }

    private func page(_ bodies: Int, anchor a: String, samples: Int = 1) -> HealthArchivePage {
        HealthArchivePage(bodies: (0..<bodies).map { Data("body\($0)".utf8) },
                          sampleCount: samples, deletedCount: 0, newAnchor: anchor(a), isEnd: false)
    }

    private func stored(_ s: HealthArchiveStream) -> Data? { defaults.data(forKey: s.anchorKey) }

    func testTheAnchorAdvancesOnlyAfterEveryBatchOfThePageIsCommitted() async {
        source.on(hr, after: nil, page(3, anchor: "A1"))
        transport.answer(.success(.committed), .success(.committed), .success(.unavailable(503)))
        await engine().sync()
        XCTAssertEqual(transport.count, 3)
        XCTAssertNil(stored(hr), "two of three batches is not the page")

        await engine().sync()
        XCTAssertEqual(stored(hr), anchor("A1"))
        XCTAssertEqual(defaults.bool(forKey: hr.caughtUpKey), true)
    }

    func testAn503LeavesTheAnchorAndMovesOnToTheNextType() async {
        defaults.set(anchor("A0"), forKey: hr.anchorKey)
        source.on(hr, after: anchor("A0"), page(1, anchor: "A1"))
        source.on(sleep, after: nil, page(1, anchor: "S1"))
        transport.answer(.success(.unavailable(503)))
        let sync = engine()
        await sync.sync()
        XCTAssertEqual(stored(hr), anchor("A0"))
        XCTAssertEqual(stored(sleep), anchor("S1"), "one type's failure does not stop the others")
        XCTAssertEqual(sync.lastError, "unavailable")
        XCTAssertNil(sync.lastSync, "a run with an error is not a sync")
    }

    func testAThrownTransportErrorLeavesTheAnchor() async {
        source.on(hr, after: nil, page(1, anchor: "A1"))
        transport.answer(.failure(Boom()))
        let sync = engine([hr])
        await sync.sync()
        XCTAssertNil(stored(hr))
        XCTAssertEqual(sync.lastError, "transport")
    }

    func testAFailedReadLeavesTheAnchor() async {
        defaults.set(anchor("A0"), forKey: hr.anchorKey)
        source.fail(hr, with: Boom())
        await engine([hr]).sync()
        XCTAssertEqual(stored(hr), anchor("A0"))
        XCTAssertEqual(transport.count, 0)
    }

    func testALockedDatabaseEndsTheRunTouchingNoAnchor() async {
        defaults.set(anchor("A0"), forKey: hr.anchorKey)
        source.fail(hr, with: HKError(.errorDatabaseInaccessible))
        source.on(sleep, after: nil, page(1, anchor: "S1"))
        let sync = engine()
        await sync.sync()
        XCTAssertEqual(stored(hr), anchor("A0"))
        XCTAssertNil(stored(sleep), "the run ended; no later type was read")
        XCTAssertEqual(source.readTypes, [hr.typeIdentifier])
        XCTAssertFalse(defaults.bool(forKey: hr.caughtUpKey), "locked is not caught up")
        XCTAssertNil(sync.lastError, "a locked phone is not an error")
    }

    func testALockedPhoneDoesNotStartARun() async {
        unlocked = false
        await engine().sync()
        XCTAssertTrue(source.readTypes.isEmpty)
    }

    func testNotConfiguredStopsTheWholeRun() async {
        source.on(hr, after: nil, page(1, anchor: "A1"))
        source.on(sleep, after: nil, page(1, anchor: "S1"))
        transport.answer(.success(.notConfigured))
        let sync = engine()
        await sync.sync()
        XCTAssertEqual(transport.count, 1)
        XCTAssertEqual(source.readTypes, [hr.typeIdentifier])
        XCTAssertNil(stored(hr))
        XCTAssertEqual(sync.state, .notConfigured)

        // The next run that gets a 200 clears it.
        await sync.sync()
        XCTAssertNotEqual(sync.state, .notConfigured)
    }

    func testTwoConcurrentTriggersMakeOneRun() async {
        source.on(hr, after: nil, page(1, anchor: "A1"))
        transport.gate = { try? await Task.sleep(for: .milliseconds(100)) }
        let sync = engine([hr])
        async let first: Void = sync.sync()
        async let second: Void = sync.sync()
        _ = await (first, second)
        XCTAssertEqual(transport.count, 1)
        // One run: one page read and one end-of-stream read, not two of each.
        XCTAssertEqual(source.readTypes.count, 2)
    }

    func testAnInterruptedBackfillResumesFromTheLastStoredAnchor() async {
        source.on(hr, after: nil, page(1, anchor: "A1"))
        source.on(hr, after: anchor("A1"), page(1, anchor: "A2"))
        source.on(hr, after: anchor("A2"), page(1, anchor: "A3"))
        transport.answer(.success(.committed), .failure(Boom()))
        await engine([hr]).sync()
        XCTAssertEqual(stored(hr), anchor("A1"), "the first page held")

        await engine([hr]).sync()
        XCTAssertEqual(stored(hr), anchor("A3"))
        let resumedAt = source.reads.dropFirst(2).first?.anchor
        XCTAssertEqual(resumedAt, anchor("A1"), "the second run starts where the first stopped")
        XCTAssertFalse(source.reads.dropFirst(2).contains { $0.anchor == nil },
                       "history is not read again from the start")
    }

    func testBackfillProgressCountsTypesDone() async {
        source.on(hr, after: nil, page(1, anchor: "A1", samples: 40))
        source.fail(sleep, with: Boom())
        let sync = engine()
        await sync.sync()
        XCTAssertEqual(sync.state, .backfilling(sent: 40, typesDone: 1, typesTotal: 2))
    }

    // MARK: Routes

    func testARouteAnchorAdvancesOnlyAfterItsRequestIsCommitted() async {
        let routes = HealthArchiveStream.routes
        source.on(routes, after: nil, page(1, anchor: "R1"))
        transport.answer(.success(.unavailable(503)))
        await engine([routes]).sync()
        XCTAssertNil(stored(routes))
        await engine([routes]).sync()
        XCTAssertEqual(stored(routes), anchor("R1"))
        XCTAssertEqual(routes.anchorKey, "healthArchive.anchor.HKWorkoutRouteTypeIdentifier")
    }

    /// The archive's anchors never share a key with the chat triggers' `healthAnchor.*`.
    func testArchiveAnchorsAreSeparateFromTheChatTriggerAnchors() {
        for stream in HealthArchiveStream.all {
            XCTAssertTrue(stream.anchorKey.hasPrefix("healthArchive.anchor."))
            XCTAssertFalse(stream.anchorKey.hasPrefix("healthAnchor."))
        }
    }
}

// MARK: - Coordinates stay out of the turn and the log

final class HealthArchiveCoordinateTests: XCTestCase {
    /// A route walked through the per-turn block's own path (`AltitudeSample(location)`, the
    /// reducer, the formatter) leaves nothing positional in the block: no latitude, no
    /// longitude, and from the route only its four elevation numbers.
    func testThePerTurnBlockCarriesNoCoordinateFromARoute() throws {
        let lat = 51.2345678, lon = -0.8765432
        let start = Date(timeIntervalSince1970: 1_791_000_000)
        let altitudes = (0..<600).map { 300 + 40 * sin(Double($0) / 95) }
        let locations = altitudes.enumerated().map { i, alt in
            CLLocation(coordinate: CLLocationCoordinate2D(latitude: lat + Double(i) * 1e-5,
                                                          longitude: lon - Double(i) * 1e-5),
                       altitude: alt, horizontalAccuracy: 4, verticalAccuracy: 3, course: 90,
                       speed: 3, timestamp: start.addingTimeInterval(Double(i)))
        }
        let elevation = try XCTUnwrap(RouteElevationReducer.reduce(locations.map(AltitudeSample.init)))
        let run = WorkoutSummary(activityName: "Run", start: start, duration: 600, elapsed: 600,
                                 distanceMeters: 2000, activeEnergyKcal: 150,
                                 averageHeartRateBPM: 150, maxHeartRateBPM: 165, source: "Watch",
                                 routeElevation: elevation)
        let block = try XCTUnwrap(HealthContextFormatter.block(daily: .empty, workouts: [run],
                                                                now: start.addingTimeInterval(3600),
                                                                timeZone: TimeZone(identifier: "UTC")!))
        for fragment in ["51.23", "51.2", "0.876", "-0.87", "0.88", "lat", "lon"] {
            XCTAssertFalse(block.contains(fragment), "\(fragment) in:\n\(block)")
        }
        let f = { (m: Double) in String(format: "%.0f", m) }
        XCTAssertTrue(block.contains(
            "ascent \(f(elevation.ascentM)) m (route), descent \(f(elevation.descentM)) m (route), elevation \(f(elevation.minAltitudeM)) to \(f(elevation.maxAltitudeM)) m"),
            block)
    }

    /// No log call in the archive's files interpolates anything positional or any value:
    /// type names, counts, status codes and error kinds only.
    func testNoLogLineInTheArchiveFilesFormatsACoordinateOrAValue(file: StaticString = #filePath) throws {
        let app = URL(fileURLWithPath: "\(file)").deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("Jesse")
        let forbidden = ["coordinate", "latitude", "longitude", "lat", "lon", "loc", "altitude",
                         "course", "speed", "point", "value", "metadata", "body",
                         "localizedDescription", "description", "route.", "workout."]
        var checked = 0
        for name in ["HealthArchiveSync.swift", "HealthArchiveReader.swift"] {
            let source = try String(contentsOf: app.appendingPathComponent(name), encoding: .utf8)
            for line in source.split(separator: "\n") where line.contains("Log.") {
                checked += 1
                for expr in Self.interpolations(in: String(line)) {
                    let lowered = expr.lowercased()
                    for word in forbidden where lowered.contains(word.lowercased()) {
                        XCTFail("\(name): log line interpolates `\(expr)`")
                    }
                }
            }
        }
        XCTAssertGreaterThan(checked, 5, "the scan found the log lines")
    }

    /// Every `\( … )` in a line, nested ones included, by matching parentheses.
    static func interpolations(in line: String) -> [String] {
        let chars = Array(line)
        var out: [String] = []
        var i = 0
        while i < chars.count - 1 {
            if chars[i] == "\\", chars[i + 1] == "(" {
                var depth = 0, j = i + 1
                repeat {
                    if chars[j] == "(" { depth += 1 } else if chars[j] == ")" { depth -= 1 }
                    j += 1
                } while depth > 0 && j < chars.count
                out.append(String(chars[(i + 2)..<(j - 1)]))
            }
            i += 1
        }
        return out
    }
}
