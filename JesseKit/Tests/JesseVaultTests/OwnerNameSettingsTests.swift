import XCTest
@testable import JesseVault

// THE OWNER NAME, WRITABLE — the 2026-10-21 incident, asserted.
//
// Offline, on the iPhone, a first-person question ended with "No owner name is set in
// Settings", and there was no field in Settings to set it in: the key was read by the
// retriever and by `PromptStore`, and nothing on either platform wrote it. These tests pin
// the one setter that now writes it, the shared field that commits through that setter,
// and a notice that names where the field is.
//
// Every test runs against its own throwaway `UserDefaults` suite.

final class OwnerNameSettingsTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!
    private var settings: OfflineLookupSettings!

    override func setUpWithError() throws {
        try super.setUpWithError()
        suiteName = "jesse.owner.name.tests.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        settings = OfflineLookupSettings(defaults: defaults)
    }

    override func tearDown() {
        defaults?.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    // MARK: - The setter

    func testASingleNameIsStoredTrimmedAndReadBack() {
        settings.ownerName = "  Jeremy \n"
        XCTAssertEqual(settings.ownerName, "Jeremy")
        XCTAssertEqual(defaults.string(forKey: OfflineLookupSettings.ownerNameKey), "Jeremy")
        XCTAssertEqual(LookupQuery.ownerForms(settings.ownerName), ["Jeremy"])
    }

    func testSeveralFormsAreStoredAsOneListAndSplitInOrder() {
        settings.ownerName = " Jeremy, Jeremiah, Andrews "
        XCTAssertEqual(settings.ownerName, "Jeremy, Jeremiah, Andrews")
        XCTAssertEqual(LookupQuery.ownerForms(settings.ownerName),
                       ["Jeremy", "Jeremiah", "Andrews"])
    }

    func testBlankOrWhitespaceRemovesTheKey() {
        for blank in ["", "   ", " \n\t ", nil] as [String?] {
            settings.ownerName = "Jeremy"
            XCTAssertNotNil(defaults.object(forKey: OfflineLookupSettings.ownerNameKey))
            settings.ownerName = blank
            XCTAssertNil(defaults.object(forKey: OfflineLookupSettings.ownerNameKey),
                         "\(String(describing: blank)) left the key behind")
            XCTAssertNil(settings.ownerName)
        }
    }

    // MARK: - The shared field's commit

    func testTheFieldCommitsThroughTheSameSetter() {
        let shown = OwnerNameSettingsRows.commit(" Jeremy, Jeremiah, Andrews ", to: settings)
        XCTAssertEqual(OfflineLookupSettings(defaults: defaults).ownerName,
                       "Jeremy, Jeremiah, Andrews")
        XCTAssertEqual(shown, "Jeremy, Jeremiah, Andrews", "the field shows what was stored")

        let cleared = OwnerNameSettingsRows.commit("   ", to: settings)
        XCTAssertNil(OfflineLookupSettings(defaults: defaults).ownerName)
        XCTAssertNil(defaults.object(forKey: OfflineLookupSettings.ownerNameKey))
        XCTAssertEqual(cleared, "")
    }

    func testTheFieldSaysWhatItIsAndWhatItTakes() {
        XCTAssertEqual(OwnerNameSettingsRows.label, "Owner name")
        XCTAssertEqual(OwnerNameSettingsRows.prompt, "Jeremy, Jeremiah, Andrews")
        XCTAssertEqual(OwnerNameSettingsRows.caption,
                       "How your notes name you. Separate several forms with commas, the one "
                       + "you go by first. On-device answers use it for questions about "
                       + "\"me\" and \"my\".")
    }

    // MARK: - The notice

    func testTheNoticeNamesTheField() {
        XCTAssertEqual(OfflineLookupReply.ownerNameMissing,
                       "No owner name is set (Settings › Vault folder › Owner name), so "
                       + "\"my\" and \"I\" could not be matched to your notes.")
    }

    func testTheNoticeComesOnlyForAFirstPersonQuestionWithNoName() {
        XCTAssertEqual(OfflineLookupReply.ownerNameNotice(question: "What is my birthday?",
                                                          ownerName: settings.ownerName),
                       OfflineLookupReply.ownerNameMissing)
        XCTAssertNil(OfflineLookupReply.ownerNameNotice(question: "When is Lucia's birthday?",
                                                        ownerName: settings.ownerName))
        _ = OwnerNameSettingsRows.commit("Jeremy, Jeremiah, Andrews", to: settings)
        XCTAssertNil(OfflineLookupReply.ownerNameNotice(question: "What is my birthday?",
                                                        ownerName: settings.ownerName))
    }
}
