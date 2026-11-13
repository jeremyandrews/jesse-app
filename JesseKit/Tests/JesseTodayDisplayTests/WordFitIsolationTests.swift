import XCTest
@testable import JesseTodayDisplay

/// The word-fitting string helpers are callable OFF the main actor.
///
/// `WordFitLine` is a SwiftUI `View`, and the conformance makes the type and every member not
/// marked otherwise MainActor. `WorkSemantics.rowTitle` is nonisolated and calls two of its
/// helpers. PR 333 shipped that call with the helpers still MainActor: the Studio's Swift 6.4
/// demoted the diagnostic to a warning, and Xcode 26.6 on the nightly runner rejected it as an
/// error. This class is deliberately NOT `@MainActor`, and every call below is made from a
/// detached task, so a helper that slides back onto the main actor stops this file compiling
/// on the runner's toolchain, and fails `scripts/local-ci-macos.sh` (which rejects any
/// surviving Swift warning) on a newer one.
final class WordFitIsolationTests: XCTestCase {

    func testTheStringHelpersRunOffTheMainActor() async {
        let out = await Task.detached {
            (
                dropped: WordFitLine.dropDangling(["plan", "the", "move", "of", "the"]),
                trimmed: WordFitLine.trimTail("ask about the keys, "),
                variants: WordFitLine.variants("Answer the question about the router", minWords: 4),
                dangling: WordFitLine.dangling.contains("the")
            )
        }.value
        XCTAssertEqual(out.dropped, ["plan", "the", "move"])
        XCTAssertEqual(out.trimmed, "ask about the keys")
        XCTAssertEqual(out.variants.first, "Answer the question about the router")
        XCTAssertTrue(out.variants.dropFirst().allSatisfy { $0.hasSuffix("\u{2026}") }, "\(out.variants)")
        XCTAssertTrue(out.dangling)
    }

    func testARowTitleCutShortIsTidiedOffTheMainActor() async {
        let title = await Task.detached {
            WorkSemantics.rowTitle("Rebuild the kiln shelf and the\u{2026}")
        }.value
        XCTAssertEqual(title, "Rebuild the kiln shelf\u{2026}")
    }
}
