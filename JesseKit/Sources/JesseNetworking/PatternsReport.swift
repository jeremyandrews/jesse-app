import Foundation

/// The bridge's Patterns report (`patterns` on the live diet snapshot, bridge 0.162.0 and
/// later): every preregistered question with its verdict, effect in units and interval, plus
/// the energy audit. The bridge computes all of it, sentences included, so the wording that
/// keeps an association from reading as a cause has one home. The app only draws.
///
/// An older bridge sends no field and `DietSnapshot.patterns` is nil, which hides the
/// Health tab's Patterns row, exactly as before.
public struct PatternsReport: Decodable, Equatable, Sendable {
    public var questions: [PatternResult]
    public var counts: PatternCounts
    public var energyAudit: EnergyAudit?
    public var caveat: String

    enum CodingKeys: String, CodingKey { case questions, counts, energyAudit, caveat }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        questions = try c.decodeIfPresent([PatternResult].self, forKey: .questions) ?? []
        counts = try c.decodeIfPresent(PatternCounts.self, forKey: .counts) ?? PatternCounts()
        energyAudit = try? c.decodeIfPresent(EnergyAudit.self, forKey: .energyAudit)
        caveat = try c.decodeIfPresent(String.self, forKey: .caveat) ?? ""
    }

    public init(questions: [PatternResult], counts: PatternCounts,
                energyAudit: EnergyAudit? = nil, caveat: String = "") {
        self.questions = questions; self.counts = counts
        self.energyAudit = energyAudit; self.caveat = caveat
    }
}

public struct PatternCounts: Decodable, Equatable, Sendable {
    public var findings: Int
    public var ruledOut: Int
    public var watching: Int
    public init(findings: Int = 0, ruledOut: Int = 0, watching: Int = 0) {
        self.findings = findings; self.ruledOut = ruledOut; self.watching = watching
    }
}

/// A question's verdict. An unknown value from a newer bridge reads as `watching`, the one
/// verdict that claims nothing.
public enum PatternVerdict: String, Decodable, Equatable, Sendable {
    case finding, ruledOut, watching

    public init(from decoder: Decoder) throws {
        self = PatternVerdict(rawValue: try decoder.singleValueContainer().decode(String.self))
            ?? .watching
    }
}

/// One arm of a question: which kind of day, how many of them, and the outcome's mean.
public struct PatternArm: Decodable, Equatable, Sendable {
    public var label: String
    public var days: Int
    public var mean: Double?
    public init(label: String, days: Int, mean: Double? = nil) {
        self.label = label; self.days = days; self.mean = mean
    }
}

/// One catalogue question, as the bridge judged it.
public struct PatternResult: Decodable, Equatable, Sendable, Identifiable {
    public var id: String
    public var title: String
    public var verdict: PatternVerdict
    /// The row's sentence, built by the bridge: associational wording, in units.
    public var sentence: String
    /// The finding in a few words for the nav row ("calorie intake 322 kcal higher on
    /// weekend days"); nil when there is no effect yet.
    public var short: String?
    public var unit: String
    public var decimals: Int
    /// The smallest difference the question treats as worth caring about, in `unit`.
    public var meaningful: Double
    public var high: PatternArm
    public var low: PatternArm
    /// HIGH-arm mean minus LOW-arm mean, and its 95% interval.
    public var effect: Double?
    public var ciLow: Double?
    public var ciHigh: Double?
    public var p: Double?
    public var rho: Double?
    public var expected: String?
    public var asExpected: Bool?
    public var halvesAgree: Bool?
    public var fdrPass: Bool?
    public var daysNeeded: Int?
    public var watchingReason: String?

    enum CodingKeys: String, CodingKey {
        case id, title, verdict, sentence, short, unit, decimals, meaningful, high, low, effect
        case ciLow, ciHigh, p, rho, expected, asExpected, halvesAgree, fdrPass, daysNeeded
        case watchingReason
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? id
        verdict = try c.decodeIfPresent(PatternVerdict.self, forKey: .verdict) ?? .watching
        sentence = try c.decodeIfPresent(String.self, forKey: .sentence) ?? ""
        short = try c.decodeIfPresent(String.self, forKey: .short)
        unit = try c.decodeIfPresent(String.self, forKey: .unit) ?? ""
        decimals = try c.decodeIfPresent(Int.self, forKey: .decimals) ?? 0
        meaningful = try c.decodeIfPresent(Double.self, forKey: .meaningful) ?? 0
        high = try c.decodeIfPresent(PatternArm.self, forKey: .high) ?? PatternArm(label: "", days: 0)
        low = try c.decodeIfPresent(PatternArm.self, forKey: .low) ?? PatternArm(label: "", days: 0)
        effect = try c.decodeIfPresent(Double.self, forKey: .effect)
        ciLow = try c.decodeIfPresent(Double.self, forKey: .ciLow)
        ciHigh = try c.decodeIfPresent(Double.self, forKey: .ciHigh)
        p = try c.decodeIfPresent(Double.self, forKey: .p)
        rho = try c.decodeIfPresent(Double.self, forKey: .rho)
        expected = try c.decodeIfPresent(String.self, forKey: .expected)
        asExpected = try c.decodeIfPresent(Bool.self, forKey: .asExpected)
        halvesAgree = try c.decodeIfPresent(Bool.self, forKey: .halvesAgree)
        fdrPass = try c.decodeIfPresent(Bool.self, forKey: .fdrPass)
        daysNeeded = try c.decodeIfPresent(Int.self, forKey: .daysNeeded)
        watchingReason = try c.decodeIfPresent(String.self, forKey: .watchingReason)
    }

    public init(id: String, title: String, verdict: PatternVerdict, sentence: String,
                short: String? = nil, unit: String, decimals: Int = 0, meaningful: Double,
                high: PatternArm, low: PatternArm, effect: Double? = nil,
                ciLow: Double? = nil, ciHigh: Double? = nil, daysNeeded: Int? = nil) {
        self.id = id; self.title = title; self.verdict = verdict; self.sentence = sentence
        self.short = short; self.unit = unit; self.decimals = decimals
        self.meaningful = meaningful; self.high = high; self.low = low; self.effect = effect
        self.ciLow = ciLow; self.ciHigh = ciHigh; self.daysNeeded = daysNeeded
    }

    /// Paired days behind the question, both arms together.
    public var days: Int { high.days + low.days }
}

/// The energy audit: logged intake net of logged exercise over the last four weeks, against
/// what the scale trend implies. `withheld` carries the reason instead of numbers when too
/// few days were logged.
public struct EnergyAudit: Decodable, Equatable, Sendable {
    public var withheld: String?
    public var sentence: String?
    public var note: String?
    public var netIntakeKcal: Double?
    public var trendLbsPerWeek: Double?
    public var scaleDeficitKcal: Double?
    public var scaleDeficitLow: Double?
    public var scaleDeficitHigh: Double?
    public var maintenanceKcal: Double?
    public var maintenanceLow: Double?
    public var maintenanceHigh: Double?

    public init(withheld: String? = nil, sentence: String? = nil, note: String? = nil,
                netIntakeKcal: Double? = nil, maintenanceKcal: Double? = nil,
                maintenanceLow: Double? = nil, maintenanceHigh: Double? = nil) {
        self.withheld = withheld; self.sentence = sentence; self.note = note
        self.netIntakeKcal = netIntakeKcal; self.maintenanceKcal = maintenanceKcal
        self.maintenanceLow = maintenanceLow; self.maintenanceHigh = maintenanceHigh
    }
}
