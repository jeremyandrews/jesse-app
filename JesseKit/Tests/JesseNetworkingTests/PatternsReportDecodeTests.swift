import XCTest
@testable import JesseNetworking

/// The bridge's `patterns` field, decoded the way the Health tab reads it. The fixture is the
/// bridge engine's own output shape over invented numbers, never the owner's logs.
final class PatternsReportDecodeTests: XCTestCase {
    static let json = #"""
    {
      "window": {"from": "2025-07-01", "to": "2026-06-30"},
      "questions": [
        {"id": "weekend-calories", "title": "Weekends and calories", "verdict": "finding",
         "sentence": "On weekend days, calorie intake was 250 kcal higher than on weekdays (40 to 460 higher), 44 vs 120 days.",
         "short": "calorie intake 250 kcal higher on weekend days",
         "unit": "kcal", "decimals": 0, "meaningful": 200, "lagDays": 0,
         "expected": "higher", "asExpected": true,
         "high": {"label": "weekend days", "days": 44, "mean": 2300.0},
         "low": {"label": "weekdays", "days": 120, "mean": 2050.0},
         "effect": 250.0, "ciLow": 40.0, "ciHigh": 460.0, "p": 0.012, "fdrPass": true,
         "halvesAgree": true, "rho": 0.2, "threshold": 0, "daysNeeded": null,
         "watchingReason": null, "from": "2026-01-10", "to": "2026-06-30"},
        {"id": "alcohol-hrv", "title": "Alcohol and next-morning HRV", "verdict": "watching",
         "sentence": "Not enough days yet: 0 vs 0 mornings so far, 8 needed in each.",
         "short": null, "unit": "ms", "decimals": 0, "meaningful": 5,
         "high": {"label": "days with alcohol", "days": 0, "mean": null},
         "low": {"label": "days without", "days": 0, "mean": null},
         "effect": null, "ciLow": null, "ciHigh": null, "p": null, "fdrPass": false,
         "verdictFromTheFuture": "ignored"}
      ],
      "counts": {"findings": 1, "ruledOut": 0, "watching": 1},
      "energyAudit": {"window": {"from": "2026-06-03", "to": "2026-06-30"},
        "netIntakeKcal": 1900, "maintenanceKcal": 2500, "maintenanceLow": 2100,
        "maintenanceHigh": 2900, "sentence": "Last 28 days ...", "note": "A four-week scale trend still carries water."},
      "caveat": "These compare days in your own logs, and they are associations, not causes."
    }
    """#

    func testTheReportDecodesWithUnitsIntervalsAndVerdicts() throws {
        let r = try JSONDecoder().decode(PatternsReport.self, from: Data(Self.json.utf8))
        XCTAssertEqual(r.questions.count, 2)
        let q = r.questions[0]
        XCTAssertEqual(q.verdict, .finding)
        XCTAssertEqual(q.effect, 250)
        XCTAssertEqual(q.ciLow, 40)
        XCTAssertEqual(q.high.days, 44)
        XCTAssertEqual(q.days, 164)
        XCTAssertNil(r.questions[1].effect, "no number below the per-arm minimum")
        XCTAssertEqual(r.counts, PatternCounts(findings: 1, ruledOut: 0, watching: 1))
        XCTAssertEqual(r.energyAudit?.maintenanceKcal, 2500)
        XCTAssertTrue(r.caveat.contains("not causes"))
    }

    func testAnUnknownVerdictReadsAsWatching() throws {
        let body = #"{"questions":[{"id":"x","verdict":"somethingNew","high":{"label":"a","days":1},"low":{"label":"b","days":1}}],"counts":{"findings":0,"ruledOut":0,"watching":1},"caveat":""}"#
        let r = try JSONDecoder().decode(PatternsReport.self, from: Data(body.utf8))
        XCTAssertEqual(r.questions.first?.verdict, .watching)
    }

    func testAWithheldAuditDecodes() throws {
        let body = #"{"questions":[],"counts":{"findings":0,"ruledOut":0,"watching":0},"energyAudit":{"withheld":"12 of the last 28 days have calories logged; the audit needs 21"},"caveat":""}"#
        let r = try JSONDecoder().decode(PatternsReport.self, from: Data(body.utf8))
        XCTAssertEqual(r.energyAudit?.withheld, "12 of the last 28 days have calories logged; the audit needs 21")
        XCTAssertNil(r.energyAudit?.maintenanceKcal)
    }
}
