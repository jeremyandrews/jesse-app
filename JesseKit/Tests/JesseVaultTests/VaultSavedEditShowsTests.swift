import XCTest
@testable import JesseVault

// A SAVED EDIT SHOWS ON THE READER AT ONCE, NOT AFTER A RELAUNCH.
//
// The defect, reported 2026-09-27: edit a note, Save, and the reader showed the text as it
// was before the save; force quit and relaunch, and the edit was there. The reload after a
// save asked the Studio whether the device's copy was current BEFORE the unawaited outbox
// flush had carried the save to it, so the Studio answered with its old copy and the open
// decision, which knew nothing of this device's own queued writes, showed that copy as
// "the device is behind". Nothing reloaded when the flush then landed.
//
// Each test holds the flush where the defect needs it: a write route that waits behind a
// latch, one that answers `503`, or a copy left in the opener's reuse cache.

/// A door the fake write route waits behind, so a flush can be held in flight.
final class FlushLatch: @unchecked Sendable {
    private let lock = NSLock()
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let pass: Bool = lock.withLock {
                if isOpen { return true }
                waiters.append(continuation)
                return false
            }
            if pass { continuation.resume() }
        }
    }

    func open() {
        let waiting: [CheckedContinuation<Void, Never>] = lock.withLock {
            isOpen = true
            defer { waiters = [] }
            return waiters
        }
        waiting.forEach { $0.resume() }
    }
}

@MainActor
final class VaultSavedEditShowsTests: XCTestCase {

    private var root: URL!
    private var container: URL!
    private var support: URL!
    private var suiteName: String!
    private var defaults: UserDefaults!

    private let path = "Projects/drafts/Draft.md"
    private let before = "# Draft\n\n- [ ] **P1** Kits.\n- [ ] **P2** Sheet.\n"
    private let saved = "# Draft\n\n- [ ] **P1** Kits.\n- [ ] **P2** Sheet.\n\nA new paragraph.\n"
    private let stale = "# Draft\n\n- [ ] **P1** Kits.\n"

    override func setUp() async throws {
        try await super.setUp()
        root = VaultFixture.makeDirectory()
        container = VaultFixture.makeDirectory()
        support = VaultFixture.makeDirectory()
        suiteName = "jesse.saved.edit.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suiteName)
        for url in [root, container, support] { VaultFixture.cleanUp(url!) }
        try await super.tearDown()
    }

    private func makeSource() throws -> VaultIndexSource {
        let folder = VaultFolder(defaults: defaults, key: "saved.edit.bookmark")
        try folder.adopt(url: root)
        return VaultIndexSource(folder: folder, container: container)
    }

    /// The device's own writer, on this test's outbox, log and opener.
    private func localWriter(_ source: VaultIndexSource, _ outbox: VaultWriteOutbox,
                             _ opener: VaultNoteOpener) -> VaultNoteWriter {
        VaultNoteWriter(source: source,
                        log: OfflineWriteLog(directory: support.appendingPathComponent("log")),
                        outbox: outbox, opener: opener)
    }

    private func save(_ text: String, through writer: any VaultNoteWriting) async throws {
        let editor = VaultNoteEditorModel(path: path, writer: writer)
        await editor.load()
        editor.text = text
        await editor.save()
        XCTAssertTrue(editor.didSave, "the save itself must succeed")
    }

    // MARK: - Cause 3: the reload raced the unawaited flush

    func testASavedEditShowsOnTheReloadBeforeItsFlushLands() async throws {
        VaultFixture.write(before, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        let latch = FlushLatch()
        bridge.onSend = { await latch.wait() }
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let source = try makeSource()
        let reader = VaultNoteReaderModel(source: source,
                                          writer: localWriter(source, outbox, opener),
                                          opener: opener, outbox: outbox)
        await reader.load(path: path)
        XCTAssertEqual(reader.origin, .local)

        try await save(saved, through: localWriter(source, outbox, opener))
        // The Studio still has the old note: the flush is held behind the latch.
        await reader.reload()

        XCTAssertEqual(reader.text, saved, "the reader shows what was just saved")
        XCTAssertEqual(reader.origin, .local,
                       "never the device is behind caption because of the owner's own save")
        XCTAssertNotNil(reader.fileURL)

        bridge.setNote(path, saved)
        latch.open()
        await outbox.flush()
    }

    // MARK: - Cause 5: the Studio copy path, a busy bridge

    func testASaveToTheStudiosCopyThatIsOnlyQueuedShowsWhatWasSaved() async throws {
        VaultFixture.write(stale, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        bridge.busy = true
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let reader = VaultNoteReaderModel(source: try makeSource(), opener: opener,
                                          outbox: outbox)
        await reader.load(path: path)
        guard case .bridge = reader.origin else { return XCTFail("\(reader.origin)") }

        try await save(saved, through: try XCTUnwrap(reader.bridgeWriter))
        await reader.reload()

        XCTAssertEqual(reader.text, saved, "the reader shows what was just saved")
        XCTAssertEqual(reader.pending.filter { !$0.isConflicted }.count, 1,
                       "with one notice that it has not reached the Studio yet")
        XCTAssertEqual(try VaultFile(root: root).read(relativePath: path), stale,
                       "the Obsidian folder is never written from the Studio's copy")

        // A second save is made against what the owner is looking at.
        let base = try await XCTUnwrap(reader.bridgeWriter).readStamped(path: path)
        XCTAssertEqual(base.text, saved)
        XCTAssertEqual(base.stamp, VaultFileStamp(text: saved))
    }

    // MARK: - Cause 6: the reuse window

    func testTheReuseCacheCannotServeAPreSaveCopyToTheReloadAfterASave() async throws {
        VaultFixture.write(before, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        let studioTakes = saved
        let notePath = path
        bridge.onSend = { [weak bridge] in bridge?.setNote(notePath, studioTakes) }
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let source = try makeSource()
        let reader = VaultNoteReaderModel(source: source,
                                          writer: localWriter(source, outbox, opener),
                                          opener: opener, outbox: outbox)
        await reader.load(path: path)
        XCTAssertEqual(reader.origin, .local)
        // Another screen resolves a link to this note: the Studio's copy, before the save,
        // is now in the opener's reuse cache.
        _ = await opener.resolve(target: "Projects/drafts/Draft", localPath: path)

        try await save(saved, through: localWriter(source, outbox, opener))
        await outbox.flush()
        let waiting = await outbox.entries(forPath: path)
        XCTAssertTrue(waiting.isEmpty, "the Studio took the save")
        await reader.reload()

        XCTAssertEqual(reader.text, saved)
        XCTAssertEqual(reader.origin, .local)
    }

    // MARK: - Ticks take the same path

    func testATickShowsOnAReloadBeforeItsFlushLands() async throws {
        VaultFixture.write(before, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        let latch = FlushLatch()
        bridge.onSend = { await latch.wait() }
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let source = try makeSource()
        let reader = VaultNoteReaderModel(source: source,
                                          writer: localWriter(source, outbox, opener),
                                          opener: opener, outbox: outbox)
        await reader.load(path: path)
        let block = try XCTUnwrap(reader.document?.blocks.first { block in
            if case .checkbox = block.kind { return block.line == 3 }
            return false
        })
        await reader.tick(block: block, to: true)
        await reader.reload()

        XCTAssertTrue(reader.text.contains("- [x] **P1** Kits."), reader.text)
        XCTAssertEqual(reader.origin, .local)

        latch.open()
        await outbox.flush()
    }

    // MARK: - The answer reloads the reader

    func testTheFlushLandingReloadsTheReaderOntoTheStudiosMatchingCopy() async throws {
        VaultFixture.write(before, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        let latch = FlushLatch()
        bridge.onSend = { await latch.wait() }
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let source = try makeSource()
        let reader = VaultNoteReaderModel(source: source,
                                          writer: localWriter(source, outbox, opener),
                                          opener: opener, outbox: outbox)
        await reader.load(path: path)
        try await save(saved, through: localWriter(source, outbox, opener))
        await reader.reload()
        XCTAssertEqual(reader.pending.count, 1)

        // With the editor up, an answer changes the notice and nothing else.
        bridge.setNote(path, saved)
        latch.open()
        await outbox.flush()
        let fetches = bridge.fetches.count
        await reader.outboxDidChange(editing: true)
        XCTAssertEqual(bridge.fetches.count, fetches, "never a reload under the editor")
        XCTAssertTrue(reader.pending.isEmpty)

        // The same answer with the editor closed reloads onto the Studio's copy.
        let reader2 = VaultNoteReaderModel(source: source,
                                           writer: localWriter(source, outbox, opener),
                                           opener: opener, outbox: outbox)
        let latch2 = FlushLatch()
        bridge.onSend = { await latch2.wait() }
        await reader2.load(path: path)
        let final = saved + "One more line.\n"
        try await save(final, through: localWriter(source, outbox, opener))
        await reader2.reload()
        XCTAssertEqual(reader2.pending.count, 1)
        bridge.setNote(path, final)
        latch2.open()
        await outbox.flush()
        let fetchesBefore = bridge.fetches.count
        await reader2.outboxDidChange(editing: false)

        XCTAssertGreaterThan(bridge.fetches.count, fetchesBefore, "the answer reloaded the reader")
        XCTAssertEqual(bridge.fetches.last?.ifNoneMatch, VaultFileStamp(text: final).digest)
        XCTAssertEqual(reader2.text, final)
        XCTAssertEqual(reader2.stamp, VaultFileStamp(text: final))
        XCTAssertEqual(reader2.origin, .local)
        XCTAssertTrue(reader2.pending.isEmpty)
    }

    func testARefusedWriteReloadsTheReaderOntoWhatTheStudioHas() async throws {
        VaultFixture.write(stale, to: path, in: root)
        let bridge = FakeVaultBridge()
        bridge.setNote(path, before)
        bridge.busy = true
        let outbox = scratchOutbox(support)
        await outbox.configure { bridge }
        let opener = VaultNoteOpener(client: bridge)
        let reader = VaultNoteReaderModel(source: try makeSource(), opener: opener,
                                          outbox: outbox)
        await reader.load(path: path)
        try await save(saved, through: try XCTUnwrap(reader.bridgeWriter))
        await reader.reload()
        XCTAssertEqual(reader.text, saved)

        bridge.busy = false
        bridge.answer { ["id": $0["id"] ?? "", "status": "refused", "reason": "no"] }
        await outbox.flush()
        await reader.outboxDidChange(editing: false)

        XCTAssertEqual(reader.text, before, "a refused save no longer shows as if it landed")
        XCTAssertTrue(reader.pending.isEmpty)
    }
}

// MARK: - The decision, with this device's own queued writes

final class VaultNoteOpeningQueuedTests: XCTestCase {

    private let studio = "# Draft\n"
    private let mine = "# Draft\n\nMine.\n"

    private var studioNote: VaultBridgeNote {
        VaultBridgeNote(path: "Draft.md", markdown: studio, modified: nil,
                        sha256: VaultFileStamp(text: studio).digest, truncated: false)
    }

    private func entry(_ new: String, state: VaultWriteState = .queued) -> VaultOutboxEntry {
        VaultOutboxEntry(record: .replacing(localPath: "Draft.md", base: studio,
                                            baseStamp: VaultFileStamp(text: studio),
                                            new: new, kind: .edit),
                         state: state, strandTickSent: false)
    }

    func testAQueuedWriteMatchingTheLocalCopyOpensItLocal() {
        let opening = VaultNoteOpening.decide(localStamp: VaultFileStamp(text: mine),
                                              fetch: .note(studioNote),
                                              queued: [entry(mine)])
        XCTAssertEqual(opening, .local)
    }

    func testOnlyTheNewestQueuedWriteCounts() {
        let opening = VaultNoteOpening.decide(localStamp: VaultFileStamp(text: mine),
                                              fetch: .note(studioNote),
                                              queued: [entry(mine), entry("# Draft\n\nLater.\n")])
        XCTAssertEqual(opening, .bridge(studioNote))
    }

    func testAQueuedWriteNotMatchingTheLocalCopyChangesNothing() {
        let opening = VaultNoteOpening.decide(localStamp: VaultFileStamp(text: "# Other\n"),
                                              fetch: .note(studioNote),
                                              queued: [entry(mine)])
        XCTAssertEqual(opening, .bridge(studioNote))
    }

    func testAConflictedWriteChangesNothing() {
        let conflicted = entry(mine, state: .conflicted(currentText: studio,
                                                        currentSHA256: studioNote.sha256))
        let opening = VaultNoteOpening.decide(localStamp: VaultFileStamp(text: mine),
                                              fetch: .note(studioNote),
                                              queued: [conflicted])
        XCTAssertEqual(opening, .bridge(studioNote))
    }

    func testTheOfflineAnswerIsUnchanged() {
        let opening = VaultNoteOpening.decide(localStamp: VaultFileStamp(text: mine),
                                              fetch: .failed("offline"),
                                              queued: [entry(mine)])
        XCTAssertEqual(opening, .offline)
        XCTAssertEqual(VaultNoteOpening.decide(localStamp: VaultFileStamp(text: mine),
                                               fetch: .routeMissing, queued: [entry(mine)]),
                       .offline)
    }
}
