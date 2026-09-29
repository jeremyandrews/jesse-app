import Foundation
import JesseNetworking

// The pure half of the Patterns screen: which rows go where, what the Health tab's nav row
// says, and the geometry of each row's two-point chart. Every number and every sentence
// arrives from the bridge's engine (`patterns` on the diet snapshot); nothing here computes
// a statistic, so nothing here can word one as a cause.

public enum Patterns {
    /// The Patterns row shows when the bridge sent a report with questions in it. An older
    /// bridge sends none, and a past day never carries one.
    public static func isAvailable(_ report: PatternsReport?) -> Bool {
        report.map { !$0.questions.isEmpty } ?? false
    }

    /// The three sections, in reading order, each keeping the bridge's catalogue order. An
    /// empty verdict is left out.
    public static func sections(_ report: PatternsReport)
        -> [(verdict: PatternVerdict, rows: [PatternResult])] {
        [PatternVerdict.finding, .ruledOut, .watching].compactMap { v in
            let rows = report.questions.filter { $0.verdict == v }
            return rows.isEmpty ? nil : (v, rows)
        }
    }

    /// A section's heading.
    public static func heading(_ v: PatternVerdict) -> String {
        switch v {
        case .finding: return "Findings"
        case .ruledOut: return "Ruled out"
        case .watching: return "Watching"
        }
    }

    /// What each section means, one line under its rows.
    public static func explainer(_ v: PatternVerdict) -> String {
        switch v {
        case .finding:
            return "The interval excludes zero, it survives the check for asking many "
                + "questions at once, and both halves of the history agree."
        case .ruledOut:
            return "Measured, and the whole interval sits inside the smallest difference "
                + "that would matter. A null measured this well is a result."
        case .watching:
            return "Not settled either way yet. Each row says how many days it has and "
                + "roughly how many more it needs."
        }
    }

    /// "2 findings, 4 ruled out, 9 watching", singular where it should be.
    public static func countsLine(_ c: PatternCounts) -> String {
        let finding = c.findings == 1 ? "1 finding" : "\(c.findings) findings"
        return "\(finding), \(c.ruledOut) ruled out, \(c.watching) watching"
    }

    /// The finding that matters most: the largest effect measured against its own
    /// meaningful size, so a 0.7 lb weight shift and a 322 kcal intake shift compare fairly.
    public static func topFinding(_ report: PatternsReport) -> PatternResult? {
        report.questions
            .filter { $0.verdict == .finding && $0.effect != nil }
            .max { strength($0) < strength($1) }
    }

    private static func strength(_ q: PatternResult) -> Double {
        guard let e = q.effect, q.meaningful > 0 else { return 0 }
        return abs(e) / q.meaningful
    }

    /// The nav row's subtitle: the top finding in units when there is one, otherwise the
    /// three counts. It says "not enough days yet" only when not one question has a single
    /// paired day. Nil when there is no report to show.
    public static func subtitle(_ report: PatternsReport?) -> String? {
        guard let report, isAvailable(report) else { return nil }
        if report.questions.allSatisfy({ $0.days == 0 }) { return "not enough days yet" }
        if let top = topFinding(report), let short = top.short, !short.isEmpty {
            let others = report.counts.findings - 1
            return others > 0 ? "\(short) · \(others) more" : short
        }
        return countsLine(report.counts)
    }

    /// A row's two points and interval: the LOW arm's mean, the HIGH arm's mean, and the
    /// HIGH arm's position if the difference sat at either end of its interval. Nil when
    /// the question has no effect yet (below the per-arm minimum).
    public struct ChartGeometry: Equatable, Sendable {
        public var low: Double
        public var high: Double
        public var intervalLow: Double
        public var intervalHigh: Double
    }

    public static func chart(_ q: PatternResult) -> ChartGeometry? {
        guard let base = q.low.mean, let effect = q.effect,
              let lo = q.ciLow, let hi = q.ciHigh else { return nil }
        return ChartGeometry(low: base, high: base + effect,
                             intervalLow: base + lo, intervalHigh: base + hi)
    }

    /// A value in a question's unit, for chart labels and accessibility.
    public static func format(_ v: Double, _ q: PatternResult) -> String {
        let s = v.formatted(.number.precision(.fractionLength(q.decimals)).grouping(.automatic))
        return "\(s) \(q.unit)"
    }
}
