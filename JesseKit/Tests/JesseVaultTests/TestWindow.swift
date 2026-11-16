#if canImport(AppKit)
import AppKit

// The ONLY way a JesseKit test builds a window. scripts/swift-test-guards.sh fails any other
// test file that constructs one (`NSWindow(`, `NSPanel(`, `.init`, or a subclass's
// `(contentRect:`), on Linux CI and in the local gate. Fenced because JesseKit's tests also
// compile for iOS.
//
// Why: a programmatically created window RELEASES ITSELF ON CLOSE, which under ARC is an
// over-release, because the test still holds it. It does not fail where it happens. It
// corrupts the process and blows up later, in `objc_autoreleasePoolPop` at the end of some
// unrelated test, which `swift test` reported as a signal 11 in `VaultBrowserModelTests` or
// `StrandMenuTests` depending on the run. Every window built here is owned by its test.
//
// The Mac app's tests carry the same helper (Jesse/JesseMacTests/MacTestWindow.swift): the
// two test targets share no support files.

/// A window of class `W` (a subclass with its own overrides), with
/// `isReleasedWhenClosed = false` already set.
@MainActor
func makeTestWindow<W: NSWindow>(_ type: W.Type,
                                 contentRect: NSRect,
                                 styleMask: NSWindow.StyleMask,
                                 backing: NSWindow.BackingStoreType = .buffered,
                                 defer flag: Bool = false) -> W {
    let window = W(contentRect: contentRect, styleMask: styleMask, backing: backing, defer: flag)
    window.isReleasedWhenClosed = false
    return window
}

/// A plain `NSWindow`, with `isReleasedWhenClosed = false` already set.
@MainActor
func makeTestWindow(contentRect: NSRect,
                    styleMask: NSWindow.StyleMask,
                    backing: NSWindow.BackingStoreType = .buffered,
                    defer flag: Bool = false) -> NSWindow {
    makeTestWindow(NSWindow.self, contentRect: contentRect, styleMask: styleMask,
                   backing: backing, defer: flag)
}
#endif
