import XCTest

/// THE BUG, DRIVEN END TO END: a returned file opened on the phone with no way to close it.
///
/// A chip used to open a `QLPreviewController` embedded as a SwiftUI sheet's child. Embedded
/// that way QuickLook draws no navigation bar, so there was no Done; and the sheet ignored
/// the safe area, so its grabber sat under a scrolling document that took every downward
/// drag. The user's only way back to the conversation was a force quit.
///
/// The app's own seam (`ArtifactViewerUITestSeam`, armed by `JESSE_UITEST_ARTIFACTS`) seeds
/// one conversation whose reply returned a PDF, an HTML page, a PNG and an SVG, with the
/// bytes already in the artifact cache, and opens it the way a tapped notification does.
/// Every test here opens one of those files and requires a way back: the Done button, and
/// separately a swipe down, and then the conversation on screen again.
final class ArtifactViewerUITests: XCTestCase {

    // Duplicated from `ArtifactViewerUITestSeam`: a UI test cannot import the app.
    private static let threadTitle = "Artifact Probe"
    private static let pdf = "probe-report.pdf"
    private static let html = "probe-page.html"
    private static let png = "probe-chart.png"
    private static let svg = "probe-diagram.svg"

    private var app: XCUIApplication!

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchEnvironment["JESSE_UITEST_ARTIFACTS"] = "1"
        app.launch()
        XCTAssertTrue(app.navigationBars[Self.threadTitle].waitForExistence(timeout: 60),
                      "the seeded conversation opens")
    }

    // MARK: - Done

    func testAPDFClosesWithDone() { openThenCloseWithDone(Self.pdf) }
    func testAnHTMLFileClosesWithDone() { openThenCloseWithDone(Self.html) }
    func testAPictureClosesWithDone() { openThenCloseWithDone(Self.png) }
    func testAnSVGClosesWithDone() { openThenCloseWithDone(Self.svg) }

    // MARK: - Swipe down

    func testAPDFClosesWithASwipeDown() { openThenCloseWithSwipe(Self.pdf) }
    func testAnHTMLFileClosesWithASwipeDown() { openThenCloseWithSwipe(Self.html) }
    func testAPictureClosesWithASwipeDownAtMinimumZoom() { openThenCloseWithSwipe(Self.png) }
    func testAnSVGClosesWithASwipeDownAtMinimumZoom() { openThenCloseWithSwipe(Self.svg) }

    // MARK: - Reopening

    /// The chip's presentation state is cleared on dismissal, so the same chip opens again.
    /// A state left set would make the second tap a no-op: SwiftUI sees no change.
    func testTheSameChipOpensAgainAfterClosing() {
        openThenCloseWithDone(Self.pdf)
        openThenCloseWithSwipe(Self.pdf)
        openThenCloseWithDone(Self.pdf)
    }

    func testTheSamePictureOpensAgainAfterClosing() {
        openThenCloseWithSwipe(Self.png)
        openThenCloseWithDone(Self.png)
    }

    // MARK: - Steps

    private func openThenCloseWithDone(_ name: String) {
        open(name)
        let done = doneButton()
        XCTAssertTrue(done.isHittable, "\(name): and Done can be tapped")
        done.tap()
        assertBackInConversation(name)
    }

    /// Where a user's swipe down starts, which differs by viewer.
    private enum SwipeFrom {
        /// The sheet's own bar, level with Done. QuickLook draws its document out of process
        /// and keeps a drag that starts on it (verified: from the page, the sheet does not
        /// move), so the swipe that puts the sheet away starts on its header.
        case bar
        /// The picture itself, which is where the full screen viewer's swipe lives.
        case content
    }

    private func openThenCloseWithSwipe(_ name: String) {
        open(name)
        let done = doneButton()
        let from: SwipeFrom = (name == Self.pdf || name == Self.html) ? .bar : .content
        let origin = app.coordinate(withNormalizedOffset: .zero)
        let width = app.windows.firstMatch.frame.width
        let height = app.windows.firstMatch.frame.height
        let startY = from == .bar ? done.frame.midY : height * 0.3
        let start = origin.withOffset(CGVector(dx: width / 2, dy: startY))
        let end = origin.withOffset(CGVector(dx: width / 2, dy: height * 0.98))
        // Slow enough to read as a drag, fast enough to commit the dismissal.
        start.press(forDuration: 0.05, thenDragTo: end, withVelocity: 1500,
                    thenHoldForDuration: 0)
        assertBackInConversation(name)
    }

    /// Tap the chip or picture labelled `name`, scrolling the transcript to it first.
    private func open(_ name: String) {
        let item = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", name)).firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 30), "\(name) is in the transcript")
        // Hittable is not enough: a picture whose top shows above the composer is hittable,
        // and a tap at its centre lands on the composer. Scroll the transcript until the whole
        // item sits above the composer.
        let composer = app.textViews.firstMatch
        let limit = composer.exists ? composer.frame.minY : app.windows.firstMatch.frame.height * 0.7
        var tries = 0
        while !item.isHittable || item.frame.maxY > limit, tries < 6 {
            app.scrollViews.firstMatch.swipeUp()
            tries += 1
        }
        XCTAssertTrue(item.isHittable && item.frame.maxY <= limit,
                      "\(name) is fully on screen (frame \(item.frame), limit \(limit))")
        item.tap()
        XCTAssertTrue(doneButton().waitForExistence(timeout: 20), "\(name): the viewer shows Done")
        settle(doneButton())
        // A document must actually render in the viewer, not just its chrome: wait for text
        // only the file contains. Pictures carry no text to wait for.
        if let text = Self.contentText[name] {
            XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label BEGINSWITH %@", text))
                            .firstMatch.waitForExistence(timeout: 20),
                          "\(name): the document renders")
        }
        // Kept whether or not the test passes: the screenshot is the evidence when the
        // chrome is what is in question.
        attachScreenshot("\(name) open")
    }

    /// Text each document's own content starts a line with, to know it has rendered.
    private static let contentText = [pdf: "Probe report, page 1", html: "Row 1 of a page"]

    /// The conversation is what is on screen: its navigation bar exists and can be touched,
    /// and no viewer's Done is left above it.
    private func assertBackInConversation(_ name: String) {
        let bar = app.navigationBars[Self.threadTitle]
        let back = waitFor(timeout: 20) { bar.exists && bar.isHittable && !self.doneButton().exists }
        if !back { attachScreenshot("\(name) after closing") }
        XCTAssertTrue(back, "\(name): closing the viewer returns to the conversation")
    }

    /// The viewer's Done. Every viewer carries the app's own, QuickLook included: QuickLook's
    /// xmark ignores a tap while an HTML preview is still loading.
    private func doneButton() -> XCUIElement { app.buttons["Done"].firstMatch }

    /// Wait until `element` stops moving. UIKit drops touches while a presentation is still
    /// animating, so a tap on a sheet's close control as it slides up is lost (seen on a
    /// gate run: tapped at the half-way point, nothing happened). A person cannot tap that
    /// fast; the test can.
    private func settle(_ element: XCUIElement) {
        var last = element.frame
        for _ in 0..<40 {
            _ = app.navigationBars.firstMatch.waitForExistence(timeout: 0.25)
            let now = element.frame
            if now == last { return }
            last = now
        }
    }

    private func attachScreenshot(_ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    private func waitFor(timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            _ = app.navigationBars.firstMatch.waitForExistence(timeout: 0.25)
        }
        return condition()
    }
}
