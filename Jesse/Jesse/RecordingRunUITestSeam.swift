import Foundation
import SwiftData
import JesseCore
import JesseSpeech

/// The simulator seam for a recording run, and nothing else.
///
/// What this feature changes only exists in a running app: a run that keeps going while the
/// app is in the background, is suspended, or is terminated and relaunched. Starting one
/// normally takes a file picker and a language sheet, which is UI automation; this seam
/// starts one from the launch environment instead, so the lifecycle can be driven with
/// `simctl` alone (launch, background, terminate, relaunch) and no UI automation at all.
///
/// `JESSE_UITEST_TRANSCRIBE=<absolute path to a recording>|<locale>` arms it, for example
/// `/…/data/tmp/fixture.m4a|en-US`. It opens a new conversation, lands on it, and confirms
/// the language on the owner's behalf; from there the run is the real one, through the real
/// owner, store, upload session and bridge. ABSENT, which is every ordinary launch, it does
/// nothing. Compiled out of Release, on the same terms as `JESSE_UITEST_BRIDGE`.
@MainActor
enum RecordingRunUITestSeam {

    /// The recording and the language, or nil in every ordinary launch. Read ONCE.
    static let fixture: (url: URL, locale: String)? = {
        #if DEBUG
        guard let raw = ProcessInfo.processInfo.environment["JESSE_UITEST_TRANSCRIBE"] else { return nil }
        let parts = raw.split(separator: "|", maxSplits: 1).map(String.init)
        guard parts.count == 2, !parts[0].isEmpty else { return nil }
        return (URL(fileURLWithPath: parts[0]), parts[1])
        #else
        return nil
        #endif
    }()

    /// The language the armed launch offers, since the simulator has no on-device speech
    /// locales for the picker to list. Nil in every ordinary launch.
    static var supportedLocale: Locale? { fixture.map { Locale(identifier: $0.locale) } }

    private static var fired = false

    /// Start the armed run, once per process.
    static func startIfArmed(context: ModelContext, land: (JesseThread) -> Void) {
        guard let fixture, !fired else { return }
        fired = true
        let thread = JesseThread(mode: .ask)
        context.insert(thread)
        try? context.save()
        land(thread)
        let model = RecordingRunService.shared.runs.model(for: thread.id)
        Task {
            await model.begin(pickedFileAt: fixture.url)
            model.selectedLanguage = Locale(identifier: fixture.locale)
            model.confirmLanguage()
        }
    }
}
