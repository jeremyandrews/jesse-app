import SwiftUI
import Charts
import JesseNetworking
import JesseAsk

// The Patterns screen: the energy audit, then every catalogue question under its verdict
// (Findings, Ruled out, Watching). Reached from a nav row on the Health tab, in the same
// drill-down style as Consistency and Sources, on iOS and macOS alike.
//
// Every number and every sentence comes from the bridge's engine; this view only draws.
// That split is load-bearing: the wording that keeps an association from being read as a
// cause is fixed in one place, so no layout change can turn "after days with alcohol, HRV
// was lower" into "alcohol lowers your HRV". Nothing is hidden: a question that has not
// settled is listed with its days and what it still needs, and a measured null is shown as
// the result it is.

struct PatternsDetail: View {
    let report: PatternsReport
    /// The last day the report covers, which dates a Patterns ask.
    let anchor: String

    var body: some View {
        List {
            if let audit = report.energyAudit {
                Section {
                    EnergyAuditCard(audit: audit)
                        .askable(HealthAsk.energyAudit(audit, anchor: anchor))
                } header: {
                    Text("Energy audit")
                }
            }
            ForEach(Patterns.sections(report), id: \.verdict) { section in
                Section {
                    ForEach(section.rows) { q in
                        PatternRow(result: q)
                            .askable(HealthAsk.patternResult(q, anchor: anchor))
                    }
                    CaveatRow(text: Patterns.explainer(section.verdict))
                } header: {
                    Text("\(Patterns.heading(section.verdict)) (\(section.rows.count))")
                }
            }
            if !report.caveat.isEmpty {
                Section { CaveatRow(text: report.caveat) }
            }
        }
        .navigationTitle("Patterns")
        .dietNavTitle(.inline)
        .askPageToolbar(HealthAsk.patterns(report, anchor: anchor, scope: .page))
    }
}

/// The audit's sentence, the implied maintenance as the number to read, and what it cannot
/// see. A withheld audit says why instead.
struct EnergyAuditCard: View {
    let audit: EnergyAudit

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let withheld = audit.withheld {
                Text("Withheld: \(withheld).")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                if let m = audit.maintenanceKcal {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("Maintenance about \(kcal(m))")
                            .font(.subheadline.weight(.semibold))
                        if let lo = audit.maintenanceLow, let hi = audit.maintenanceHigh {
                            Text("\(kcal(lo, unit: false)) to \(kcal(hi))")
                                .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        }
                    }
                }
                if let sentence = audit.sentence {
                    Text(sentence)
                        .font(.caption).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if let note = audit.note {
                Text(note)
                    .font(.caption2).foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func kcal(_ v: Double, unit: Bool = true) -> String {
        let n = v.formatted(.number.precision(.fractionLength(0)).grouping(.automatic))
        return unit ? "\(n) kcal" : n
    }
}

/// One question: its title, a two-point chart of the two arms with the interval on the
/// second, and the bridge's sentence. Colour is deliberately neutral: green or red would
/// read as good or bad, and a difference between two kinds of day is neither.
struct PatternRow: View {
    let result: PatternResult

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(result.title)
                .font(.subheadline.weight(.semibold))
                .fixedSize(horizontal: false, vertical: true)
            if let g = Patterns.chart(result) {
                PatternChart(result: result, geometry: g)
                    .frame(height: 64)
            }
            Text(result.sentence)
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(result.title). \(result.sentence)")
    }
}

/// Two points on one horizontal value axis: the low arm's mean, and the high arm's mean
/// with its 95% interval as a bar. The axis spans both points and the interval, so the
/// interval's width is read against the gap it is about.
struct PatternChart: View {
    let result: PatternResult
    let geometry: Patterns.ChartGeometry

    var body: some View {
        Chart {
            RuleMark(xStart: .value("Low", geometry.intervalLow),
                     xEnd: .value("High", geometry.intervalHigh),
                     y: .value("Arm", result.high.label))
                .lineStyle(StrokeStyle(lineWidth: 6, lineCap: .round))
                .foregroundStyle(.tint.opacity(0.25))
            PointMark(x: .value("Mean", geometry.high), y: .value("Arm", result.high.label))
                .foregroundStyle(.tint)
                .annotation(position: .trailing) {
                    Text("\(result.high.days)").font(.caption2).foregroundStyle(.secondary)
                }
            PointMark(x: .value("Mean", geometry.low), y: .value("Arm", result.low.label))
                .foregroundStyle(.secondary)
                .annotation(position: .trailing) {
                    Text("\(result.low.days)").font(.caption2).foregroundStyle(.secondary)
                }
        }
        .chartXScale(domain: domain)
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 3)) { value in
                AxisGridLine()
                AxisValueLabel {
                    if let v = value.as(Double.self) {
                        Text(Patterns.format(v, result)).font(.caption2)
                    }
                }
            }
        }
        .chartYAxis {
            AxisMarks { value in
                AxisValueLabel {
                    if let s = value.as(String.self) { Text(s).font(.caption2) }
                }
            }
        }
        .accessibilityHidden(true)
    }

    private var domain: ClosedRange<Double> {
        let values = [geometry.low, geometry.high, geometry.intervalLow, geometry.intervalHigh]
        let lo = values.min() ?? 0, hi = values.max() ?? 1
        let pad = max((hi - lo) * 0.15, result.meaningful * 0.1, 0.1)
        return (lo - pad)...(hi + pad)
    }
}
