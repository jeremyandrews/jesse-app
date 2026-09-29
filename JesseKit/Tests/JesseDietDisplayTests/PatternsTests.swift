import XCTest
@testable import JesseDietDisplay
import JesseNetworking
import JesseAsk

/// The Patterns screen's presentation rules over bridge-shaped reports: the nav row's
/// subtitle, the section order, the chart geometry, and what an ask carries. The statistics
/// themselves are the bridge's and are tested there.
final class PatternsTests: XCTestCase {
    private func q(_ id: String, _ verdict: PatternVerdict, effect: Double? = nil,
                   meaningful: Double = 5, days: (Int, Int) = (20, 40),
                   short: String? = nil) -> PatternResult {
        PatternResult(id: id, title: id, verdict: verdict, sentence: "\(id) sentence",
                      short: short, unit: "ms", decimals: 0, meaningful: meaningful,
                      high: PatternArm(label: "days with alcohol", days: days.0, mean: effect.map { 70 + $0 }),
                      low: PatternArm(label: "days without", days: days.1, mean: 70),
                      effect: effect, ciLow: effect.map { $0 - 3 }, ciHigh: effect.map { $0 + 3 })
    }

    private func report(_ qs: [PatternResult]) -> PatternsReport {
        PatternsReport(
            questions: qs,
            counts: PatternCounts(findings: qs.filter { $0.verdict == .finding }.count,
                                  ruledOut: qs.filter { $0.verdict == .ruledOut }.count,
                                  watching: qs.filter { $0.verdict == .watching }.count),
            caveat: "associations, not causes")
    }

    func testTheSubtitleIsTheStrongestFindingInUnits() {
        let r = report([
            q("small", .finding, effect: 6, meaningful: 5, short: "HRV 6 ms lower"),
            q("big", .finding, effect: 500, meaningful: 200, short: "calorie intake 500 kcal higher"),
            q("null", .ruledOut, effect: 0.5),
        ])
        XCTAssertEqual(Patterns.subtitle(r), "calorie intake 500 kcal higher · 1 more",
                       "2.5 meaningful sizes beats 1.2")
    }

    func testWithoutAFindingTheSubtitleIsTheCountsNeverNothingYet() {
        let r = report([q("a", .ruledOut, effect: 1), q("b", .watching, effect: 2),
                        q("c", .watching, days: (0, 0))])
        XCTAssertEqual(Patterns.subtitle(r), "0 findings, 1 ruled out, 2 watching")
    }

    func testNotEnoughDaysOnlyWhenNoQuestionHasADay() {
        let r = report([q("a", .watching, days: (0, 0)), q("b", .watching, days: (0, 0))])
        XCTAssertEqual(Patterns.subtitle(r), "not enough days yet")
    }

    func testNoReportHidesTheRow() {
        XCTAssertNil(Patterns.subtitle(nil))
        XCTAssertFalse(Patterns.isAvailable(report([])))
    }

    func testSectionsAreFindingsRuledOutWatchingInCatalogueOrder() {
        let r = report([q("w1", .watching), q("f1", .finding, effect: 9), q("r1", .ruledOut, effect: 0),
                        q("w2", .watching), q("f2", .finding, effect: 7)])
        let s = Patterns.sections(r)
        XCTAssertEqual(s.map(\.verdict), [.finding, .ruledOut, .watching])
        XCTAssertEqual(s[0].rows.map(\.id), ["f1", "f2"])
        XCTAssertEqual(s[2].rows.map(\.id), ["w1", "w2"])
    }

    func testTheChartPlacesTheHighArmAndItsIntervalAgainstTheLowArm() {
        let g = Patterns.chart(q("x", .finding, effect: -7))
        XCTAssertEqual(g, Patterns.ChartGeometry(low: 70, high: 63, intervalLow: 60, intervalHigh: 66))
        XCTAssertNil(Patterns.chart(q("y", .watching)), "no effect, no chart")
    }

    func testAnAskCarriesTheVerdictTheEffectInUnitsAndTheInterval() {
        let ctx = HealthAsk.patternResult(q("alcohol-hrv", .finding, effect: -7), anchor: "2026-09-28")
        let lines = ctx.facts.children.flatMap(\.lines)
        XCTAssertTrue(lines.contains("Verdict: finding"), "\(lines)")
        XCTAssertTrue(lines.contains { $0.contains("-7 ms") && $0.contains("95% interval -10 ms to -4 ms") },
                      "\(lines)")
        XCTAssertTrue(lines.contains("days with alcohol: 20 days; days without: 40 days"), "\(lines)")
        XCTAssertNotNil(ctx.facts.note)
    }

    func testThePageAskGroupsByVerdictAndStatesTheCounts() {
        let r = report([q("f", .finding, effect: 9), q("w", .watching, days: (3, 10))])
        let ctx = HealthAsk.patterns(r, anchor: "2026-09-28", scope: .page)
        XCTAssertEqual(ctx.facts.lines, ["1 finding, 0 ruled out, 1 watching"])
        XCTAssertEqual(ctx.facts.children.compactMap(\.heading), ["Findings", "Watching"])
        XCTAssertEqual(ctx.facts.note, "associations, not causes")
    }
}
