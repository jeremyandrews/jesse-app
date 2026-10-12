import Foundation

/// Every HealthKit quantity and category type the health archive copies to the bridge, by
/// raw identifier, each quantity type with the ONE unit its values travel in.
///
/// The list is exactly the app's HealthKit read set (`HealthContextProvider.readTypes`) minus
/// the two types that are not plain samples, workouts and workout routes, which the archive
/// carries as kinds of their own. The archive copies what the app may already read and
/// nothing more: a type added to the read set without an entry here, or the other way round,
/// fails the app's `HealthArchiveCatalogueTests`.
///
/// A unit is a HealthKit unit string, parsed on the phone with `HKUnit(from:)` and sent on
/// the wire as written here, so the archive's `unit` column says what the number means.
/// Percentages are HealthKit's `%`, a 0…1 fraction.
public enum HealthArchiveCatalogue {
    public struct Quantity: Equatable, Sendable {
        public let identifier: String
        public let unit: String
    }

    public static let quantities: [Quantity] = [
        q("HeartRate", "count/min"),
        q("ActiveEnergyBurned", "kcal"),
        q("DistanceSwimming", "m"),
        q("DistanceWalkingRunning", "m"),
        q("DistanceCycling", "m"),
        q("RestingHeartRate", "count/min"),
        q("HeartRateVariabilitySDNN", "ms"),
        q("VO2Max", "mL/(kg*min)"),
        q("RespiratoryRate", "count/min"),
        q("OxygenSaturation", "%"),
        q("AppleSleepingWristTemperature", "degC"),
        q("HeartRateRecoveryOneMinute", "count/min"),
        q("StepCount", "count"),
        q("BodyMass", "kg"),
        q("BodyFatPercentage", "%"),
        q("LeanBodyMass", "kg"),
        q("RunningPower", "W"),
        q("RunningGroundContactTime", "ms"),
        q("RunningVerticalOscillation", "cm"),
        q("RunningStrideLength", "m"),
        q("WalkingAsymmetryPercentage", "%"),
        q("AppleWalkingSteadiness", "%"),
        q("SwimmingStrokeCount", "count"),
        q("WaterTemperature", "degC"),
        q("WorkoutEffortScore", "appleEffortScore"),
        q("EstimatedWorkoutEffortScore", "appleEffortScore"),
        q("AppleSleepingBreathingDisturbances", "count"),
    ]

    public static let categories: [String] = [
        "HKCategoryTypeIdentifierSleepAnalysis",
        "HKCategoryTypeIdentifierLowHeartRateEvent",
        "HKCategoryTypeIdentifierHighHeartRateEvent",
        "HKCategoryTypeIdentifierIrregularHeartRhythmEvent",
        "HKCategoryTypeIdentifierSleepApneaEvent",
        "HKCategoryTypeIdentifierHypertensionEvent",
    ]

    /// The two non-sample kinds' `type` values, as the bridge requires them.
    public static let workoutType = "HKWorkoutTypeIdentifier"
    public static let routeType = "HKWorkoutRouteTypeIdentifier"

    private static func q(_ suffix: String, _ unit: String) -> Quantity {
        Quantity(identifier: "HKQuantityTypeIdentifier" + suffix, unit: unit)
    }
}
