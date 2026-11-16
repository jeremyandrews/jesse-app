import Foundation

// MARK: - A tap on the Today tab bar item

/// **What a tap on a tab bar item does to the Today tab's segment.**
///
/// The iPhone and iPad shell (`RootTabView`) asks this on every tab bar tap; the Today tab
/// then acts on the answer. It lives here, apart from the shell, so the rule is tested
/// without a tab bar:
///
/// 1. **A tap that SELECTS the Today tab changes nothing.** Coming back from Chats lands on
///    whichever segment was last showing, as it always has: the remembered segment is a
///    per-device preference, and a first tap is not a request to forget it.
/// 2. **A tap on the Today tab while it is already selected, with Strands showing,
///    returns to the day.** That is the gesture every iOS tab bar teaches ("take me to this
///    tab's top"), and the day is the Today tab's top. So from another tab with Strands
///    remembered, two taps land on the day.
/// 3. **A re-tap with the day already showing changes no segment.** Whatever the system
///    does on its own (popping a pushed screen, scrolling to the top) is left to it.
///
/// Taps on any other tab never touch this tab's segment. Generic over the shell's tab type
/// so the package does not have to know the shell's tab list.
public enum TodayTabRetap {

    /// The segment the tap should leave showing, or `nil` when it changes no segment.
    ///
    /// - Parameters:
    ///   - selected: the tab that was up when the tap arrived.
    ///   - tapped: the tab bar item that was tapped.
    ///   - todayTab: the shell's Today tab.
    ///   - showing: the Today tab's segment at the moment of the tap.
    public static func segment<Tab: Equatable>(selected: Tab, tapped: Tab, todayTab: Tab,
                                               showing: TodaySegment) -> TodaySegment? {
        guard tapped == todayTab, selected == todayTab else { return nil }
        return showing == .strands ? .today : nil
    }
}
