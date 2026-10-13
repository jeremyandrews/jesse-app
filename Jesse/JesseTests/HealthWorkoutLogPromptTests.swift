import XCTest
import JesseCore
@testable import Jesse

/// The automatic workout log only works if its text classifies as health-related: the health
/// block that carries the workouts attaches ONLY then. A future reword that dropped the health
/// words would quietly disable the whole feature — every automatic turn would arrive with no
/// workouts in it and log nothing — so the classification is pinned here, beside the same test
/// for `HealthNewDay.prompt` in `HealthRelevanceClassifierTests`.
final class HealthWorkoutLogPromptTests: XCTestCase {

    @MainActor
    func testWorkoutLogPromptClassifiesAsHealth() {
        XCTAssertTrue(HealthKeywordClassifier.matches(HealthWorkoutLog.prompt))
    }

    /// The prompt keeps its scope and its diff, and leaves room for the agent's standing
    /// inferences: the earlier "Never estimate" wording overrode the office treadmill rule.
    func testWorkoutLogPromptAllowsStandingInferences() {
        let p = HealthWorkoutLog.prompt
        XCTAssertTrue(p.lowercased().contains("do only this"))
        XCTAssertTrue(p.contains("exercise-log.csv"))
        XCTAssertTrue(p.contains("office treadmill"))
        XCTAssertFalse(p.contains("Never estimate"))
        XCTAssertFalse(p.contains("\n"))
    }

    /// The automatic weigh-in sends the button's own text, so it classifies the same way.
    @MainActor
    func testAutomaticWeighInPromptClassifiesAsHealth() {
        XCTAssertTrue(HealthKeywordClassifier.matches(HealthAutoTurn.morningRefresh.prompt))
    }
}
