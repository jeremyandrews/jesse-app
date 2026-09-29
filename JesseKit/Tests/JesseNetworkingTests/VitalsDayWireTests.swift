import XCTest
@testable import JesseNetworking

/// The vitals upload's wire shape: an unknown metric is ABSENT from the JSON (the bridge
/// writes an empty cell for it), never a 0 and never a null the bridge would have to guess
/// about, and a day with nothing known is never part of an upload.
final class VitalsDayWireTests: XCTestCase {
    func testUnknownMetricsAreOmittedNotZero() throws {
        let day = VitalsDay(date: "2026-09-27", sleepMin: 431, restingHr: 55)
        let data = try JesseBridgeClient.encodeBody(VitalsUpload(days: [day]))
        let json = String(decoding: data, as: UTF8.self)
        XCTAssertEqual(json, #"{"days":[{"date":"2026-09-27","restingHr":55,"sleepMin":431}]}"#)
    }

    func testADayWithNothingKnownIsEmpty() {
        XCTAssertTrue(VitalsDay(date: "2026-09-27").isEmpty)
        XCTAssertFalse(VitalsDay(date: "2026-09-27", steps: 0).isEmpty,
                       "a measured 0 is a value, not a gap")
    }
}
