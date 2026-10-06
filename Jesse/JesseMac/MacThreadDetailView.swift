import SwiftUI
import AppKit
import SwiftData
import JesseCore
import JesseNetworking
import JesseConversations
import JesseSpeech
import JesseVault
import PhotosUI
import UniformTypeIdentifiers

// One conversation: the transcript (hydrated from the bridge on open, cache-first) plus
// the live streaming reply and the composer. Resume is implicit — the thread carries a
// `session_id`, and sending continues that same Claude Code session on the Studio.

struct MacThreadDetailView: View {
    @Environment(\.modelContext) private var context
    @Environment(MacCoordinator.self) private var coordinator

    @Bindable var thread: JesseThread

    // Which folds (Prompt and Thinking rows) are open, by turn id. View state on purpose: it
    // lasts while this conversation is open and is gone when it is reopened. Mirrors iOS.
    @State private var openFolds: Set<UUID> = []
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var draft: String = ""
    @State private var mode: JesseMode = .ask

    /// Attaching a RECORDING, which means transcribing it: audio is never a file attachment
    /// on either platform (a turn attachment can reach a hosted model, and audio must not),
    /// so what lands in the draft is text.
    ///
    /// The same model, the same Studio-first transcriber and the same views as the iPhone;
    /// only the way a file is chosen differs. On the Studio itself the bridge is reached over
    /// loopback, anywhere else over the tailnet, exactly as every turn is; this Mac's own
    /// engine reads the recording only when the Studio cannot be reached, and says so. The
    /// pairing is read from the Keychain at each recording rather than captured here, so a
    /// re-pairing takes effect at the next one.
    @State private var recording = RecordingAttachment(
        transcriber: StudioFirstTranscriber(studio: URLSessionStudioTransport(endpoint: {
            let config = KeychainConfigStore(service: MacConfigStore.keychainService).load()
            return StudioEndpoint(baseURL: config.endpoint("/"), token: config.token)
        })))
    /// The one file panel this composer presents, and what it is choosing. One presenter
    /// rather than three: SwiftUI honours only one `fileImporter` per view chain, and the
    /// kind is kept after the panel closes so its completion knows what was picked.
    @State private var showImporter = false
    @State private var importKind: MacFileImportKind = .images

    // ── Staged FILES: images and PDFs, through the same `AttachmentStaging` as the phone.
    /// The composer's staged files, shown as chips and sent with the next turn.
    @State private var attachments: [JesseAttachment] = []
    /// Why the last file was refused, in the composer's error line. Nil almost always.
    @State private var attachError: String?
    @State private var showPhotosPicker = false
    @State private var photoItems: [PhotosPickerItem] = []

    // ── The DURABLE half of the composer ────────────────────────────────────────────
    //
    // `draft` above is view state, and this view carries `.id(thread.id)` in the split
    // view's detail column — so selecting another conversation destroys it, and quitting
    // takes it regardless.
    //
    // NOTHING REACTS TO TYPING here either: while this composer is on screen `draft` IS
    // the draft, and it is handed over at DEPARTURES through the same shared
    // `ComposerDrafts.capture` the phone uses. The only per-shell code is which hooks
    // count as a departure.

    /// Whether this composer offers a capture into the vault's `Inbox/`.
    ///
    /// State rather than computed in `body`: the decision resolves a security-scoped bookmark
    /// once reachability says it matters, and `body` re-runs as the draft changes.
    @State private var captureOffer: InboxCaptureOffer = .hidden
    /// True while the capture's coordinated append is in flight.
    @State private var capturing = false
    /// Guards the restore so it happens once per composer; a second one would overwrite
    /// live typing with a stale value.
    @State private var didRestoreDraft = false
    /// What a restored draft lost, if anything (a recording mid-transcription, or the
    /// screen context the conversation was opened about). Nil almost always.
    @State private var draftNotice: String?

    @Environment(\.scenePhase) private var scenePhase

    /// Whether this transcript is actually on screen. The detail column carries
    /// `.id(thread.id)`, so selecting another conversation destroys this view and builds a
    /// new one — which makes appear/disappear exactly "this is the selected conversation",
    /// with no need to read the selection binding from here.
    @State private var isOnScreen = false
    /// Whether this view's window is KEY. A Mac can have this conversation showing in a
    /// background window while the person works in another one, and that is not reading it.
    /// `controlActiveState` is `.key` only for the frontmost window of the active app, so
    /// it carries both halves of the Mac's gate.
    @Environment(\.controlActiveState) private var controlActiveState

    private var running: Bool { coordinator.isRunning(thread.id) }

    var body: some View {
        VStack(spacing: 0) {
            transcript
            Divider()
            composer
        }
        .navigationTitle(displayTitle(for: thread))
        .navigationSubtitle(subtitle)
        .onAppear {
            mode = thread.modeValue
            restoreDraft()
            isOnScreen = true
            // One hop later, for the reason the phone's `onAppear` states: marking read is
            // a save, and a save inside the transaction that swaps the detail column puts a
            // database write into the frames that draw the new transcript. The gate is
            // re-checked when it runs, so a selection that moved on in between marks
            // nothing.
            Task { markReadIfOnScreen() }
        }
        // The three moments a transcript is read, the same three as the phone's: it came on
        // screen (above), its window became key again, and a reply landed while it was
        // being watched.
        .onChange(of: controlActiveState) { _, _ in markReadIfOnScreen() }
        .onChange(of: thread.lastReplyMs) { _, _ in markReadIfOnScreen() }
        // ── THE DEPARTURES ────────────────────────────────────────────────────────────
        // Three of the four (the fourth is `send`): the detail column replacing this view
        // on `.id(thread.id)`, the app losing the foreground, and a quit. Cmd-Q with the
        // window frontmost may not change the scene phase at all, which is why
        // `willTerminate` is here in its own right and not as a belt to a brace.
        // LEAVING: the detail column is replacing this composer, so it stops exempting its
        // conversation from the reapers. See `ComposerDrafts.leave`.
        .onDisappear {
            isOnScreen = false
            guard didRestoreDraft else { return }
            ComposerDrafts.leave(composerState, for: thread, in: context)
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { captureDraft() }
        }
        .onReceive(NotificationCenter.default.publisher(
            for: NSApplication.willTerminateNotification)) { _ in
            captureDraft(terminating: true)
        }
        .task(id: thread.id) {
            await coordinator.hydrate(thread: thread, context: context)
        }
    }

    // MARK: - Unread

    /// Mark this conversation read, if someone is actually looking at it: it is the
    /// selected conversation (`isOnScreen`) AND its window is key in the active app
    /// (`controlActiveState == .key`). Both halves go through the same pure
    /// `jesseShouldMarkRead` the phone uses, so the two shells cannot drift on what
    /// "being read" means.
    ///
    /// Cheap to call from all three moments because `markRead` is a no-op when the
    /// conversation is already read — no save, no push.
    private func markReadIfOnScreen() {
        guard jesseShouldMarkRead(isVisible: isOnScreen,
                                  isActive: controlActiveState == .key) else { return }
        guard thread.markRead(nowMs: JesseThread.unixMillis(.now)) else { return }
        try? context.save()
        // Best-effort mirror so the phone's dot and icon badge clear too; self-healing if
        // it fails (see MacCoordinator.pushReadChange).
        coordinator.pushReadChange(for: thread)
    }

    // MARK: - The durable draft

    /// Put the composer back the way the user left it. One call into the shared
    /// `ComposerDrafts.restore` — the already-sent check, the notice and the one-shot
    /// markers all live there, so this shell cannot grow its own idea of them. Staged files
    /// come back too, from memory only (see `ComposerDraftStore`'s header), as on the phone.
    private func restoreDraft() {
        guard !didRestoreDraft else {
            // Appearing again on a composer whose text is still live: nothing to restore, but
            // the composer is open again and the reaper must know it.
            ComposerDraftStore.shared.composerOpened(thread.id)
            return
        }
        didRestoreDraft = true
        let restored = ComposerDrafts.restore(
            for: thread, newestUserTurn: newestUserTurn,
            contextStillAttached: coordinator.attachedContext(for: thread.id) != nil)
        draft = restored.text
        attachments = restored.files.map {
            JesseAttachment(filename: $0.filename, mime: $0.mime, data: $0.data)
        }
        draftNotice = restored.notice
    }

    /// The visible text and date of this conversation's newest user turn. `visibleText`
    /// because the turn's `text` may carry a screen context the composer never held.
    private var newestUserTurn: (text: String, createdAt: Date)? {
        guard let turn = thread.orderedTurns.last(where: { $0.isUser }) else { return nil }
        return (turn.visibleText, turn.createdAt)
    }

    /// What this composer is holding right now. The two situational markers are read HERE,
    /// at the departure, rather than tracked as they change.
    private var composerState: ComposerDraftCapture {
        ComposerDraftCapture(
            text: draft,
            files: attachments.map {
                ComposerDraftFile(filename: $0.filename, mime: $0.mime, data: $0.data)
            },
            pendingRecording: recording.isInFlight ? recording.sourceName : nil,
            contextLabel: coordinator.attachment(for: thread.id)?.contextLabel)
    }

    /// A DEPARTURE. Hand the composer over; the shared function does the rest, including
    /// putting a never-saved conversation on disk so the draft has somewhere to belong.
    ///
    /// `terminating` is the quit: there the write happens on this thread, because an
    /// asynchronous one may never get a turn before the process is gone.
    private func captureDraft(terminating: Bool = false) {
        guard didRestoreDraft else { return }
        ComposerDrafts.capture(composerState, for: thread, in: context,
                               terminating: terminating)
    }

    /// The window subtitle. This used to read "Not yet started" off `sessionId == nil`, which
    /// conflated two different things: a brand-new conversation and one whose first turn the
    /// bridge has already accepted but whose CLI session id has not come back yet. The phase
    /// caption below the transcript now carries the delivery state, so the subtitle is only
    /// about whether the thread has ever run.
    private var subtitle: String {
        thread.registeredAt == nil && (thread.sessionId ?? "").isEmpty ? "Not yet started" : ""
    }

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    // The transcript as it renders, grouped by JesseKit's `TranscriptItem` exactly as
                    // the phone groups it: an untyped prompt folds under a Prompt row, narration
                    // into a Thinking row above its answer.
                    ForEach(TranscriptItem.items(thread.orderedTurns)) { item in
                        transcriptRow(item)
                            .id(item.id)
                    }
                    // Delivery caption under the last user bubble, the Mac's counterpart to the
                    // phone's: "Sending…" is the pre-ACK window, "Received" means the bridge has
                    // the turn and will answer it even if this window closes.
                    if let phase = coordinator.phase(thread.id),
                       thread.orderedTurns.last?.isUser == true {
                        MacDeliveryCaption(phase: phase)
                    }
                    if running {
                        MacStreamingBubble(text: coordinator.streamingText(for: thread.id),
                                           thinking: coordinator.thinkingText(for: thread.id),
                                           activity: coordinator.activity(for: thread.id),
                                           thinkingExpanded: fold(thread.id))
                            .id(Self.streamAnchor)
                    }
                    Color.clear.frame(height: 1).id(Self.bottomAnchor)
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .onChange(of: thread.orderedTurns.count) { scrollToBottom(proxy) }
            .onChange(of: coordinator.streamingText(for: thread.id)) { scrollToBottom(proxy) }
            .onAppear { scrollToBottom(proxy) }
        }
    }

    /// One rendered transcript row.
    @ViewBuilder
    private func transcriptRow(_ item: TranscriptItem) -> some View {
        switch item {
        case .user(let turn):
            MacTurnBubble(turn: turn)
        case .prompt(let turn, let hint):
            MacPromptFold(turn: turn, hint: hint, isExpanded: fold(turn.id))
        case .reply(let answer, let thinking, let id):
            VStack(alignment: .leading, spacing: 6) {
                if let thinking {
                    MacThinkingFold(text: thinking, isLive: false, isExpanded: fold(id))
                }
                if let answer { MacTurnBubble(turn: answer) }
            }
        }
    }

    /// Whether one fold is open, as a binding its row toggles: the system default animation,
    /// none under Reduce Motion.
    private func fold(_ id: UUID) -> Binding<Bool> {
        Binding(
            get: { openFolds.contains(id) },
            set: { open in
                withAnimation(reduceMotion ? nil : .default) {
                    if open { openFolds.insert(id) } else { openFolds.remove(id) }
                }
            })
    }

    private static let bottomAnchor = "bottom"
    private static let streamAnchor = "stream"

    private func scrollToBottom(_ proxy: ScrollViewProxy) {
        withAnimation(.easeOut(duration: 0.15)) {
            proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
        }
    }

    /// The attachment a screen is holding against this conversation — its scope title
    /// and its starters as well as its body. Fully populated by the Health tab's "Ask
    /// about this"; body-only for the Today tab's Discuss, whose two extra affordances
    /// below then simply don't render.
    private var attachment: AttachedContext? { coordinator.attachment(for: thread.id) }

    private var composer: some View {
        VStack(spacing: 8) {
            // What a restored draft LOST, named. Not an error line: the text is right
            // there, and something that was part of the pending message simply is not
            // coming back with it. On a Section footer this would ellipsise (see the
            // Health tab's caveats), so it is a row of its own.
            if let draftNotice {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "exclamationmark.circle")
                    Text(draftNotice)
                    Spacer(minLength: 0)
                    Button {
                        self.draftNotice = nil
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Dismiss")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            // This conversation's own error first, then the app-wide one (a failed sync), which
            // is what `error(for:)` resolves: an error belonging to another conversation's turn
            // never appears here.
            // A refused file first: it answers what the person just did.
            if let error = attachError ?? coordinator.error(for: thread.id) ?? recording.errorMessage {
                Text(error).font(.caption).foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // A failed recording is kept, so it can go to another engine instead of being lost.
            if let offer = recording.retry {
                RecordingRetryRow(offer: offer,
                                  onRetry: { recording.retry(engine: $0) },
                                  onDiscard: { recording.discardRecording() })
            }
            // Read on this Mac because the Studio could not be reached: said out loud, beside
            // the draft it produced.
            if let notice = recording.notice {
                RecordingNoticeRow(notice: notice, onDismiss: { recording.dismissNotice() })
            }
            if case .running(let update) = recording.stage {
                RecordingProgressBar(update: update,
                                     sourceName: recording.sourceName,
                                     onCancel: { recording.cancel() })
            }
            // Says why the composer is empty and why Send works with nothing typed — and,
            // for an ask, NAMES the reading it is about, so "this" is never ambiguous. One
            // small caption, not a banner; the scope is also the window's title.
            if let attachment, thread.orderedTurns.isEmpty {
                Label(attachment.title.map { "Asking about \($0). Send an empty message to have Jesse just read it." }
                        ?? "This item is attached. Ask about it, or send an empty message to have Jesse just read it.",
                      systemImage: "paperclip")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Opening questions, in the EMPTY state only: gone the moment anything is
            // typed, one is clicked, or the conversation has a turn. Clicking one sends it
            // through the same `send` path as anything typed.
            if let starters = attachment?.starters, !starters.isEmpty,
               thread.orderedTurns.isEmpty, draft.isEmpty, !running {
                HStack(spacing: 8) {
                    ForEach(starters, id: \.self) { starter in
                        Button(starter) { draft = starter; send() }
                            .font(.caption)
                            .buttonStyle(.bordered)
                    }
                    Spacer(minLength: 0)
                }
            }
            // WHAT THE SECOND CONTROL IS FOR. The Mac has no send outbox, so this row is
            // the difference between a thought that is on disk and one that is nowhere.
            if captureOffer.isOffered {
                MacCaptureOfferNotice()
            }
            if !attachments.isEmpty {
                MacAttachmentChips(attachments: attachments, onRemove: remove)
            }
            HStack(alignment: .bottom, spacing: 10) {
                Picker("", selection: $mode) {
                    ForEach(JesseMode.allCases) { m in Text(m.label).tag(m) }
                }
                .pickerStyle(.menu)
                .labelsHidden()
                .frame(width: 130)
                .disabled(running)

                // The PER-CONVERSATION model this thread sends its next turn on. Local to this
                // Mac and this thread — never the bridge's global default, so the phone is
                // unaffected. Always present: it shows the model the next turn will use even
                // before (or without) the model list loading.
                MacModelPickerMenu(thread: thread,
                                   store: coordinator.modelList,
                                   config: coordinator.configStore.config)
                    .disabled(running)

                attachMenu

                // An AppKit-backed text view, not a SwiftUI TextField. A `TextField` reports
                // Return through `.onSubmit`, which is handed no modifier state, so "Return
                // sends, Return with a modifier makes a newline" cannot be written there at
                // all. `ComposerTextView` decides in `keyDown(with:)`, where the modifiers
                // still exist. Send remains gated by `send()` below, the same guard the send
                // button's `disabled` state mirrors.
                ComposerTextView(text: $draft, placeholder: "Message Jesse…", onSend: send,
                                 onMedia: stageMedia)
                    .frame(maxWidth: .infinity)
                    .padding(8)
                    .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 8))

                // On the Mac there IS room for the wording, so the button says what it
                // does rather than relying on a glyph and a caption.
                if captureOffer.isOffered {
                    Button {
                        captureToInbox()
                    } label: {
                        if capturing {
                            ProgressView().controlSize(.small)
                        } else {
                            Label("Capture to Inbox", systemImage: "tray.and.arrow.down")
                        }
                    }
                    .buttonStyle(.bordered)
                    .help("Writes this straight into the vault's Inbox on this Mac, without the Studio")
                    .disabled(capturing || running
                              || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill").font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(!canSend)
                // No `.keyboardShortcut(.return, modifiers: .command)` here any more: Command
                // plus Return is one of the newline combinations now, and a button shortcut
                // would win the key before the focused composer ever saw it.
            }
        }
        .padding(12)
        // A drop anywhere on the composer outside the text view. One onto the text view is
        // the text view's own (`ComposerNSTextView.performDragOperation`); both stage through
        // `stageMedia`, so a dropped file never lands as a path in the message.
        .onDrop(of: MacItemProviderMedia.dropTypes, isTargeted: nil) { providers in
            guard !running else { return false }
            Task { stageMedia(await MacItemProviderMedia.read(providers)) }
            return true
        }
        .fileImporter(isPresented: $showImporter,
                      allowedContentTypes: importKind.contentTypes,
                      allowsMultipleSelection: importKind.allowsMultipleSelection) { result in
            switch importKind {
            case .images, .pdfs: handleFileImport(result)
            // One recording at a time: the composer holds one transcript.
            case .audio: handleAudioImport(result)
            }
        }
        .photosPicker(isPresented: $showPhotosPicker, selection: $photoItems,
                      maxSelectionCount: AttachmentLimits.maxCount, matching: .images)
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            Task { await handlePhotoItems(items) }
        }
        .sheet(isPresented: Binding(get: { recording.stage == .choosingLanguage },
                                    set: { if !$0 { recording.abandon() } })) {
            RecordingLanguageSheet(model: recording)
                .frame(minWidth: 380, minHeight: 420)
        }
        .onChange(of: recording.completed) { _, value in
            guard value != nil, let done = recording.takeCompleted() else { return }
            draft = done.messageBody(typed: draft)
        }
        // The offer's ONE input. `initial: true` so a composer opened while the Studio is
        // already unreachable shows it without waiting for a state change.
        .onChange(of: BridgeReachabilityModel.shared.state, initial: true) { _, _ in
            captureOffer = coordinator.captureOffer()
        }
        // AND on the window becoming active, because the OTHER input changes elsewhere: the
        // folder is picked in the Settings scene, which reachability never notices.
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            captureOffer = coordinator.captureOffer()
        }
        // NO `onChange(of: draft)` and none for the recording stage. Typing changes this
        // view's own state and nothing else; what the composer holds is read off that
        // state at the next departure.
    }

    // MARK: - Attachments

    /// The paperclip: three ways to attach a file, and the recording, which is not one (it
    /// becomes TEXT, exactly as before it moved into this menu). Disabled while a turn runs
    /// and at the file cap, as on the phone.
    private var attachMenu: some View {
        Menu {
            Button("Photo or Image…", systemImage: "photo") {
                attachError = nil
                importKind = .images
                showImporter = true
            }
            Button("PDF Document…", systemImage: "doc") {
                attachError = nil
                importKind = .pdfs
                showImporter = true
            }
            Button("From Photos…", systemImage: "photo.on.rectangle") {
                attachError = nil
                showPhotosPicker = true
            }
            Divider()
            Button("Audio Recording…", systemImage: "waveform") {
                attachError = nil
                recording.dismissError()
                importKind = .audio
                showImporter = true
            }
            .disabled(recording.isBusy)
        } label: {
            Image(systemName: "paperclip")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Attach a photo, image or PDF, or transcribe a recording into this message")
        .accessibilityLabel("Add attachment")
        .disabled(running || attachments.count >= AttachmentLimits.maxCount)
    }

    /// The ONE staging call on this Mac, shared with the phone's `addAttachment`: every source
    /// here (the two importers, Photos, paste, drop, Continuity Camera) ends in it.
    private func addAttachment(data: Data, fallbackName: String, suggestedName: String? = nil) {
        // The Mac has no frugal mode: it is never on a metered link it knows about.
        attachError = AttachmentStaging.add(data: data, fallbackName: fallbackName,
                                            suggestedName: suggestedName, to: &attachments,
                                            frugal: .off)
    }

    /// Media read off a pasteboard or a drop. Each item is staged in order, so the caps
    /// refuse exactly the ones past the limit; an unreadable one says so.
    private func stageMedia(_ items: [MacMediaItem]) {
        attachError = nil
        for item in items {
            guard let data = item.data else {
                attachError = item.suggestedName.map { "Couldn’t attach “\($0)” (images or PDF only)." }
                    ?? "Couldn’t paste that item (images or PDF only)."
                continue
            }
            addAttachment(data: data, fallbackName: "Pasted", suggestedName: item.suggestedName)
        }
    }

    private func remove(_ att: JesseAttachment) {
        attachments.removeAll { $0.id == att.id }
        attachError = nil
    }

    private func handleFileImport(_ result: Result<[URL], Error>) {
        switch result {
        case .success(let urls):
            for url in urls {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                guard let data = try? Data(contentsOf: url) else {
                    attachError = "Couldn’t read “\(url.lastPathComponent)”."
                    continue
                }
                addAttachment(data: data, fallbackName: "Document",
                              suggestedName: url.lastPathComponent)
            }
        case .failure(let error):
            attachError = error.localizedDescription
        }
    }

    private func handlePhotoItems(_ items: [PhotosPickerItem]) async {
        for item in items {
            let loaded = try? await item.loadTransferable(type: Data.self)
            guard let data = loaded ?? nil else {
                attachError = "Couldn’t load that image."
                continue
            }
            addAttachment(data: data, fallbackName: "Photo")
        }
        photoItems = []
    }

    /// A picked recording. Transcribed on the Studio (on this Mac only when the Studio can't
    /// be reached), sent to the paired bridge and nowhere else, and the working copy is
    /// deleted however the run ends — the model owns all three.
    private func handleAudioImport(_ result: Result<[URL], Error>) {
        guard case .success(let urls) = result, let url = urls.first else { return }
        let scoped = url.startAccessingSecurityScopedResource()
        Task {
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            await recording.begin(pickedFileAt: url)
        }
    }

    /// An empty composer is normally not a turn — except on a thread a screen OPENED
    /// with context attached (the Today tab's Discuss). There, sending nothing is the
    /// explicit "just look at it", and the attached item is what the turn carries. The
    /// coordinator composes and re-checks either way; this only decides whether the
    /// button is live.
    private var canSend: Bool {
        MacSendGate.refusal(typed: draft,
                            hasAttachment: coordinator.attachedContext(for: thread.id) != nil
                                || !attachments.isEmpty,
                            isConfigured: coordinator.configStore.isConfigured,
                            isRunningInThisConversation: running) == nil
    }

    /// THE COMPOSER IS CLEARED ONLY ON A DURABLE STAGE. `stageAndSend` persists the user
    /// turn synchronously and returns whether that succeeded. A refused send and a staging
    /// save that threw both return false, leave the draft in place, and leave the text on
    /// screen — the release below is ORDERED AFTER the save and simply never runs.
    /// Write the composer's text into the vault, then clear the composer — and only then.
    ///
    /// The same ownership rule `send` follows: the draft is released after the write has
    /// landed, so a refusal or a failed write leaves the text exactly where it was.
    private func captureToInbox() {
        let text = draft
        capturing = true
        Task {
            let landed = await coordinator.captureToInbox(text: text, thread: thread,
                                                          context: context)
            capturing = false
            guard landed else {
                captureDraft()
                return
            }
            ComposerDrafts.release(for: thread)
            draft = ""
            draftNotice = nil
        }
    }

    private func send() {
        guard canSend else { return }
        guard coordinator.stageAndSend(text: draft, mode: mode, thread: thread,
                                       context: context, files: attachments) else {
            // Nothing was released, so the composer is still the truth. A refused send is
            // a departure like any other: capture it.
            captureDraft()
            return
        }
        // Durably staged: the user turn is on disk. Only now is the draft released.
        ComposerDrafts.release(for: thread)
        draft = ""
        attachments = []
        attachError = nil
        draftNotice = nil
    }
}

/// What the composer's one file panel is choosing.
enum MacFileImportKind: Equatable {
    case images, pdfs, audio

    var contentTypes: [UTType] {
        switch self {
        case .images: return [.image]
        case .pdfs: return [.pdf]
        case .audio: return AudioRecordingTypes.contentTypes
        }
    }

    /// Files go several at a time, up to the cap; a recording goes alone.
    var allowsMultipleSelection: Bool { self != .audio }
}

/// The PER-CONVERSATION model picker for the Mac composer. The selection is LOCAL — stored on
/// the thread (`selectedModelID`) and per device — so it never mutates the bridge's global
/// default and never affects another conversation or the phone. On a pick it writes the thread's
/// selection and updates this Mac's last-used default.
///
/// The control is ALWAYS present. The button shows the model the next turn will run on (the
/// thread's own choice, else this Mac's default, else the ambient `opus`) drawn from the shared
/// `MacModelListStore` — even before the list loads, and even if it never does (an older bridge
/// with no `/jesse/models` route, or a persistent failure): the button then simply shows the
/// resolved model and is not expandable, rather than the whole control vanishing. The list is
/// loaded once into the shared store and retried on failure.
private struct MacModelPickerMenu: View {
    @Environment(\.modelContext) private var context
    @Bindable var thread: JesseThread
    let store: MacModelListStore
    let config: JesseConfig

    var body: some View {
        Group {
            if let modelState = store.state {
                // The same one menu the iPhone renders, from the same `ModelMenuLayout`.
                Menu {
                    ForEach(layout.sections) { section in
                        if let header = section.header {
                            Section(header) { rows(section, in: modelState) }
                        } else {
                            rows(section, in: modelState)
                        }
                    }
                    if let control = layout.effort,
                       let resolved = modelState.resolvedModel(
                        threadModelID: thread.selectedModelID,
                        deviceDefaultID: LastUsedModelStore.id) {
                        Section("Effort") { effortControl(control, on: resolved) }
                    }
                } label: {
                    buttonLabel
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            } else {
                // The list has not loaded yet (slow / older bridge / transient failure). Show the
                // resolved model, non-expandable, so the control is present and truthful about
                // the next turn's model — never invisible.
                buttonLabel
                    .foregroundStyle(.secondary)
                    .fixedSize()
                    .help("The model this conversation will use. The full list is still loading.")
            }
        }
        .task { await loadWithRetry() }
    }

    private var buttonLabel: some View { Label(layout.buttonLabel, systemImage: "cpu") }

    /// Everything the menu renders — shared with the iPhone's picker.
    private var layout: ModelMenuLayout {
        ModelMenuLayout(state: store.state, threadModelID: thread.selectedModelID,
                        deviceDefaultID: LastUsedModelStore.id, threadEffort: thread.selectedEffort,
                        usage: UsageStore.shared.state)
    }

    /// One family's rows: the checkmark and the harness/version detail on the resolved model, a
    /// disabled row with its reason for one that cannot be picked right now.
    @ViewBuilder
    private func rows(_ section: ModelMenuSection, in state: ModelSwitchState) -> some View {
        ForEach(section.rows) { row in
            Button {
                if let model = state.offered.first(where: { $0.id == row.id }) { select(model) }
            } label: {
                // One line per row on the Mac, title and subtitle joined by ` · `: the form this
                // menu has always rendered, now carried by every row whose model bills an
                // account rather than by the resolved row alone.
                if row.isSelected, let subtitle = row.subtitle {
                    Label("\(row.title) · \(subtitle)", systemImage: "checkmark")
                } else if row.isSelected {
                    Label(row.title, systemImage: "checkmark")
                } else if let subtitle = row.subtitle {
                    Text("\(row.title) · \(subtitle)")
                } else {
                    Text(row.title)
                }
            }
            .disabled(!row.isEnabled)
        }
    }

    /// The resolved model's declared effort control: one row per value, or a single switch.
    ///
    /// Rows rather than an inline `Picker`, matching the iPhone: an inline picker in a menu
    /// replaces the enclosing `Section("Effort")` with its own, so the values render with no
    /// header saying what they are. Proven on iOS by `ModelPickerMenuUITests`; applied here
    /// because this menu is built from the same construct. **Not observed rendering on
    /// macOS** — there is no macOS UI-test target, so the Mac side of this rests on the
    /// shared construct and on `JesseMacTests` staying green, not on a screenshot.
    @ViewBuilder
    private func effortControl(_ control: ModelEffortControl, on model: ModelInfo) -> some View {
        switch control {
        case .picker(let values, let selected):
            ForEach(values, id: \.self) { value in
                Button {
                    selectEffort(value, on: model)
                } label: {
                    if value == selected {
                        Label(value, systemImage: "checkmark")
                    } else {
                        Text(value)
                    }
                }
            }
        case .toggle(let off, let on, let isOn):
            Toggle("Thinking", isOn: Binding(get: { isOn },
                                             set: { selectEffort($0 ? on : off, on: model) }))
        }
    }

    /// Populate the shared list with ONE bounded, backed-off burst (`loadModelList`, the same
    /// policy the iPhone uses), so a slow or briefly-unreachable bridge still fills in without
    /// user action but a bridge that cannot answer no longer leaves a standing 3-second poll
    /// running for as long as the conversation is open. The button already shows the resolved
    /// model meanwhile; a persistent failure just leaves it non-expandable.
    private func loadWithRetry() async {
        _ = await loadModelList(
            isConfigured: config.isConfigured,
            fetch: {
                await store.loadIfNeeded(config: config)
                return store.state
            },
            sleep: { try? await Task.sleep(for: .seconds($0)) })
        // Drop a stored effort the resolved model no longer declares, exactly as the iPhone does.
        if let state = store.state {
            let kept = ModelMenuAction.sanitizedEffort(
                state: state, threadModelID: thread.selectedModelID,
                deviceDefaultID: LastUsedModelStore.id, threadEffort: thread.selectedEffort)
            if kept != thread.selectedEffort {
                thread.selectedEffort = kept
                try? context.save()
            }
        }
        // The one shot usage load, once the list is here, exactly as the iPhone makes it.
        if store.state != nil,
           let usage = await loadUsage(
            isConfigured: config.isConfigured,
            fetch: { try? await JesseBridgeClient(config: config).fetchUsage() },
            sleep: { try? await Task.sleep(for: .seconds($0)) }) {
            UsageStore.shared.replace(usage)
        }
    }

    /// Pick a model for THIS conversation: store it on the thread and make it this Mac's
    /// default for the next new conversation. No bridge write — the phone is unaffected. A
    /// different model clears the thread's effort.
    private func select(_ model: ModelInfo) {
        guard model.available, model.id != thread.selectedModelID else { return }
        let next = ModelMenuAction.pick(model, currentModelID: thread.selectedModelID,
                                        currentEffort: thread.selectedEffort)
        thread.selectedModelID = next.modelID
        thread.selectedEffort = next.effort
        LastUsedModelStore.id = model.id
        try? context.save()
    }

    /// Pick an effort for the resolved model; it pins that model to the thread.
    private func selectEffort(_ value: String, on model: ModelInfo) {
        let next = ModelMenuAction.pickEffort(value, on: model)
        thread.selectedModelID = next.modelID
        thread.selectedEffort = next.effort
        LastUsedModelStore.id = next.modelID
        try? context.save()
    }
}

/// A persisted turn — a user message (right, tinted) or a Jesse reply (left, rendered
/// Markdown).
struct MacTurnBubble: View {
    let turn: Turn

    var body: some View {
        if turn.isUser {
            HStack {
                Spacer(minLength: 60)
                VStack(alignment: .trailing, spacing: 2) {
                    // What a screen attached to this turn, when it attached something.
                    // One caption naming the scope — never the snapshot itself, which
                    // `turn.text` still carries for the model. Mirrors iOS.
                    if turn.hasAttachedContext, let label = turn.contextLabel {
                        Label(label, systemImage: "paperclip")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    // The files sent with this turn, as their stored previews. Mirrors iOS.
                    if !turn.attachments.isEmpty {
                        MacTurnAttachmentsView(attachments: turn.orderedAttachments)
                    }
                    // An ask sent on an empty composer has no typed half to draw — the
                    // caption above is the whole turn.
                    if !turn.visibleText.isEmpty {
                        Text(turn.visibleText)
                            .textSelection(.enabled)
                            .padding(10)
                            .background(.tint.opacity(0.85), in: .rect(cornerRadius: 12))
                            .foregroundStyle(.white)
                    }
                }
            }
        } else {
            HStack(alignment: .top, spacing: 10) {
                jesseGlyph
                VStack(alignment: .leading, spacing: 4) {
                    MacMarkdownView(text: turn.text)
                    // Files JESSE returned on this turn. Nothing renders for the
                    // overwhelming majority of turns. Mirrors iOS.
                    if !turn.artifacts.isEmpty {
                        MacTurnArtifactsView(artifacts: turn.orderedArtifacts)
                    }
                    // Native provenance chip under a Jesse reply that carried structured
                    // provenance (the badge text is already stripped from `turn.text` when
                    // the reply was ingested). Absent for older / badges-off replies —
                    // nothing renders there and the text shows verbatim. Mirrors iOS.
                    if let provenance = JesseProvenance.from(json: turn.provenanceJSON) {
                        ProvenanceChip(provenance: provenance)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Spacer(minLength: 40)
            }
        }
    }

    private var jesseGlyph: some View {
        Image(systemName: "sparkle")
            .font(.callout)
            .foregroundStyle(.tint)
            .padding(.top, 2)
    }
}

/// A user turn the owner did not type, folded: one compact trailing row where the bubble
/// would be (document symbol, "Prompt", what sent it, chevron); expanded, the full prompt as a
/// selectable bubble. Mirrors the iOS `PromptFoldView`.
struct MacPromptFold: View {
    let turn: Turn
    let hint: String
    @Binding var isExpanded: Bool

    var body: some View {
        HStack {
            Spacer(minLength: 60)
            VStack(alignment: .trailing, spacing: 6) {
                Button { isExpanded.toggle() } label: {
                    HStack(spacing: 6) {
                        Image(systemName: FoldCopy.promptSymbol)
                        Text(FoldCopy.promptTitle).fontWeight(.semibold)
                        Text(hint).foregroundStyle(.secondary).lineLimit(1)
                        Image(systemName: FoldCopy.chevron(expanded: isExpanded))
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(.tint.opacity(0.15), in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(FoldCopy.promptAccessibilityLabel(hint: hint))
                .accessibilityValue(FoldCopy.accessibilityValue(expanded: isExpanded))
                .accessibilityHint(FoldCopy.promptAccessibilityHint(expanded: isExpanded))
                .accessibilityAddTraits(.isButton)
                if isExpanded {
                    // The WHOLE prompt, context included.
                    Text(turn.text)
                        .textSelection(.enabled)
                        .padding(10)
                        .background(.tint.opacity(0.85), in: .rect(cornerRadius: 12))
                        .foregroundStyle(.white)
                        .transition(.opacity)
                }
            }
        }
    }
}

/// The model's working narration, folded above its answer: a small, dim, leading line with its
/// own symbol and no bubble, so it reads as metadata rather than content; expanded, secondary
/// text set off by a thin leading rule and capped in height. Mirrors the iOS `ThinkingFold`.
struct MacThinkingFold: View {
    let text: String
    let isLive: Bool
    @Binding var isExpanded: Bool
    /// How far in the row sits: under a reply's text, past the Jesse glyph, in the transcript;
    /// zero inside the streaming bubble, which already sits past it.
    var indent: CGFloat = 26

    static let expandedMaxHeight: CGFloat = 260

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { isExpanded.toggle() } label: {
                HStack(spacing: 5) {
                    Image(systemName: FoldCopy.thinkingSymbol)
                    Text(isLive ? FoldCopy.thinkingLiveTitle : FoldCopy.thinkingTitle)
                    Image(systemName: FoldCopy.chevron(expanded: isExpanded))
                        .font(.caption2.weight(.semibold))
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(FoldCopy.thinkingAccessibilityLabel)
            .accessibilityValue(FoldCopy.accessibilityValue(expanded: isExpanded))
            .accessibilityHint(FoldCopy.thinkingAccessibilityHint(expanded: isExpanded))
            .accessibilityAddTraits(.isButton)
            if isExpanded {
                ViewThatFits(in: .vertical) {
                    narration
                    ScrollView { narration }
                }
                .frame(maxHeight: Self.expandedMaxHeight)
                .padding(.leading, 10)
                .overlay(alignment: .leading) {
                    Rectangle().fill(.tertiary).frame(width: 2)
                }
                .padding(.bottom, 4)
                .transition(.opacity)
            }
        }
        .padding(.leading, indent)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var narration: some View {
        Text((try? AttributedString(markdown: text, options: .init(
            interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(text))
            .font(.callout)
            .foregroundStyle(.secondary)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// The in-flight assistant reply while a turn streams: the live narration folded into a
/// Thinking row in its "Thinking…" state, then the answer as it arrives. Never the narration
/// as if it were the answer.
struct MacStreamingBubble: View {
    let text: String
    /// The live narration (`LiveReply`), empty until the turn has said something on its way to
    /// a tool call.
    var thinking: String = ""
    /// Already a human line with its own ellipsis (`ToolActivity.displayLabel`), so
    /// nothing here appends punctuation to it.
    let activity: String
    var thinkingExpanded: Binding<Bool> = .constant(false)

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "sparkle").font(.callout).foregroundStyle(.tint).padding(.top, 2)
            VStack(alignment: .leading, spacing: 6) {
                if !thinking.isEmpty {
                    MacThinkingFold(text: thinking, isLive: true, isExpanded: thinkingExpanded,
                                    indent: 0)
                }
                if text.isEmpty {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        // With a live Thinking row above, this line must not say it a second time.
                        Text(activity.isEmpty ? (thinking.isEmpty ? "Thinking…" : "Working…") : activity)
                            .font(.caption).foregroundStyle(.secondary)
                    }
                } else {
                    MacMarkdownView(text: text)
                    if !activity.isEmpty {
                        Text(activity).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Spacer(minLength: 40)
        }
    }
}

/// A subtle capsule rendered under a Jesse message when structured provenance is present.
/// Distinct tint for local vs hosted vs emergency, and a warning state for unverified
/// citations. This is the macOS-native sibling of the iOS `ProvenanceChip`: both are pure
/// renderings of the SAME shared `JesseProvenance` presentation helpers (chipTitle /
/// costLabel / iconName / routeKind / accessibilityText live in JesseNetworking), so the
/// two chips carry byte-identical content and can never drift on what they show — only the
/// ~30 lines of SwiftUI live per platform, because there is no shared SwiftUI module the
/// two app targets both compile (JesseNetworking is view-free by design).
struct ProvenanceChip: View {
    let provenance: JesseProvenance

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: provenance.iconName)
                .font(.caption2)
            // The account this turn billed is near its limit: the one quota mark on the chat
            // surface. Mirrors iOS.
            if provenance.isUsageWarning {
                Image(systemName: "exclamationmark.triangle")
                    .font(.caption2)
            }
            Text(provenance.chipTitle)
                .font(.caption2.weight(.medium))
            if let cost = provenance.costLabel {
                Text(cost)
                    .font(.caption2)
                    .foregroundStyle(tint.opacity(0.75))
            }
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(tint.opacity(0.14)))
        .overlay(Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 0.5))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(provenance.accessibilityText)
    }

    private var tint: Color {
        switch provenance.routeKind {
        case .hosted: return .secondary
        case .local: return .teal
        case .emergency: return .orange
        case .warning: return .red
        }
    }
}

/// The trailing delivery caption under the last user bubble. Standard macOS treatment: a
/// `.caption`/`.secondary` line, trailing aligned, no new symbol and no tint. The
/// accessibility label carries the meaning the two words cannot.
private struct MacDeliveryCaption: View {
    let phase: TurnPhase

    private var text: String {
        switch phase {
        case .sending: return "Sending…"
        case .accepted: return "Received"
        }
    }

    private var label: String {
        switch phase {
        case .sending:
            return "Sending"
        case .accepted:
            return "Received by Jesse. Your message is saved and will be answered even if you close this window."
        }
    }

    var body: some View {
        HStack {
            Spacer(minLength: 0)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityLabel(label)
        }
        .padding(.trailing, 4)
        .padding(.top, 2)
        .animation(.default, value: phase)
    }
}
