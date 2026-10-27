import SwiftUI
import UIKit
import JesseCore

/// The UI-test seam that lets a test ASSERT body counts, and nothing else.
///
/// `RenderProbe` logs one line per body evaluation, which a script can count after a run
/// but a UI test cannot: the test runs inside the simulator and cannot read the log. This
/// is a one-point, invisible accessibility element whose VALUE is `RenderProbe`'s live
/// in-memory counts, formatted `View=count;View=count`, read at the moment XCUITest asks
/// for it.
///
/// It is read through UIKit's accessibility getter, never through SwiftUI state, so
/// reading it causes no render and the number it reports is not disturbed by being looked
/// at. Present only in a DEBUG build launched with `JESSE_RENDER_PROBE=1`; absent from
/// every ordinary launch and compiled out of Release.
struct RenderProbeReadout: UIViewRepresentable {

    static let identifier = "render-probe-readout"

    static var isArmed: Bool {
        #if DEBUG
        return ProcessInfo.processInfo.environment["JESSE_RENDER_PROBE"] == "1"
        #else
        return false
        #endif
    }

    func makeUIView(context: Context) -> UIView { ReadoutView() }
    func updateUIView(_ uiView: UIView, context: Context) {}

    private final class ReadoutView: UIView {
        override init(frame: CGRect) {
            super.init(frame: frame)
            isAccessibilityElement = true
            accessibilityIdentifier = RenderProbeReadout.identifier
            accessibilityLabel = "Render probe"
        }

        required init?(coder: NSCoder) { nil }

        override var accessibilityValue: String? {
            get {
                RenderProbe.bodyCounts()
                    .sorted { $0.key < $1.key }
                    .map { "\($0.key)=\($0.value)" }
                    .joined(separator: ";")
            }
            set {}
        }
    }
}
