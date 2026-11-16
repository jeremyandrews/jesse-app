import AppKit

// The ONLY way a Mac test builds a window. scripts/swift-test-guards.sh fails any other
// test file that constructs one (`NSWindow(`, `NSPanel(`, `.init`, or a subclass's
// `(contentRect:`), on Linux CI and in the local gate.
//
// Why: a programmatically created window RELEASES ITSELF ON CLOSE, which under ARC is an
// over-release, because the test still holds it. It does not fail where it happens. It
// corrupts the process and blows up later, in `objc_autoreleasePoolPop` at the end of some
// unrelated test (ComposerTextViewTests met it first; VaultEditorStartTests in JesseKit did
// it again in September 2026). Every window built here is owned by its test instead.
//
// JesseKit's tests carry the same helper (JesseKit/Tests/JesseVaultTests/TestWindow.swift):
// the two test targets share no support files.

/// A window of class `W` (a subclass with its own overrides, such as an off screen window
/// that refuses key), with `isReleasedWhenClosed = false` already set.
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
