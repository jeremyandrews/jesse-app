import Foundation

/// The fixed message the iPhone sends, on a fresh Tell thread, when new workouts land in
/// Apple Health. The workouts peer of `HealthNewDay`, shared here in JesseCore beside it so
/// both platforms can reach it — though only iOS fires it, because HealthKit is iOS-only.
///
/// The wording is load-bearing and must keep four properties if ever reworded:
///   1. It names its own scope ("do only this") so the Studio-side routine logs the
///      exercise alone and does NOT fall into start-of-day or the new-day refresh.
///   2. It contains "log", "exercise", "workout" and "health", all on
///      `HealthKeywordClassifier.words`, so the iOS classifier attaches the health block
///      that carries the workouts. Drop those words and the turn arrives with no workouts
///      in it and silently does nothing. A test pins the classification.
///   3. It tells the routine to DIFF against `exercise-log.csv` rather than trusting the app
///      to say what is new. That diff is what makes the feature self-healing: a workout the
///      app never observed still gets picked up by the next fire, a workout already logged
///      by hand is not logged twice, and a turn replayed late from the offline queue is
///      harmless.
///   4. It must not forbid the agent's standing inferences. The earlier wording said never to
///      estimate an activity the watch did not record, and the agent obeyed that over its
///      standing office treadmill rule, so those walks were logged as "Other" with no
///      distance and corrected by hand. The inference rules (thresholds included) live on
///      the agent side; this text only says they exist and names the treadmill walk.
///
/// Rewording it also means adding the old text to `previousPrompts`, so a turn staged by an
/// older build and replayed after the upgrade is still recognized.
public enum HealthWorkoutLog {
    public nonisolated static let prompt = "Log my new exercise. This is the automatic workout log and only that. My attached health data lists my recent workouts: compare them against diet-logs/exercise-log.csv and log every workout that is not already there, then update the calorie add-back and the dashboard the way the diet logging skill requires. Use the figures the device reported, and never invent a distance, a pace or an activity from nothing. The one exception is a standing inference the diet logging rules already define, such as my office treadmill walk: apply it without asking. Leave every other unmeasured field blank. Say in each row's notes that it was logged automatically from the watch, that I have not described the session, and which fields, if any, were inferred and from what. Do only this. Do not run start of day, the new-day health refresh, the inbox or message scanners, currency, cheatsheets, or any other routine."

    /// Earlier wordings of `prompt`, still recognized as the workout log when a turn staged
    /// by an older build is replayed after an upgrade. An entry may be dropped once no queued
    /// turn from a build before its replacement can still exist.
    public nonisolated static let previousPrompts: [String] = [
        "Log my new exercise. This is the automatic workout log and only that. My attached health data lists my recent workouts: compare them against diet-logs/exercise-log.csv and log every workout that is not already there, then update the calorie add-back and the dashboard the way the diet logging skill requires. Use only the figures the device reported. Never estimate a distance, a pace or an activity the watch did not record, and leave unmeasured fields blank. Say in each row's notes that it was logged automatically from the watch and that I have not described the session. Do only this. Do not run start of day, the new-day health refresh, the inbox or message scanners, currency, cheatsheets, or any other routine.",
    ]
}
