import Foundation
import os

// Navigation cost signposts: one interval per piece of work that runs on the way into or
// out of a screen (a tab switch, a push, a pop, a new conversation), in subsystem
// `com.tag1.jesse`, category `nav`.
//
// They exist because "switching around the app has become slow" is a question about
// WHERE main thread time goes as the store grows, and an interval per suspect is what lets
// Instruments, `log show --signpost` and `XCTOSSignpostMetric` answer it by name rather
// than by reading a sampled stack. They stay in the shipped code: an interval costs
// nothing unless something is recording the subsystem.
//
// The app's own wrapper is `Log.nav` (see `Log.swift`), which is THIS signposter, so the
// app target and the packages write one timeline.
public nonisolated enum NavTrace {

    public static let subsystem = "com.tag1.jesse"
    public static let category = "nav"

    public static let signposter = OSSignposter(subsystem: subsystem, category: category)

    /// Run `body` inside a named interval and return its result.
    @inline(__always)
    public static func interval<T>(_ name: StaticString, _ body: () throws -> T) rethrows -> T {
        let state = signposter.beginInterval(name, id: signposter.makeSignpostID())
        defer { signposter.endInterval(name, state) }
        return try body()
    }

    /// Open an interval whose end is not in the same scope (an `async` pass). The state
    /// must be handed to `end` exactly once.
    public static func begin(_ name: StaticString) -> OSSignpostIntervalState {
        signposter.beginInterval(name, id: signposter.makeSignpostID())
    }

    public static func end(_ name: StaticString, _ state: OSSignpostIntervalState) {
        signposter.endInterval(name, state)
    }
}
