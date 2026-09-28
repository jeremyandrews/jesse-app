import SwiftUI

// The two pieces of UI the recording flow needs, and they live in the package rather
// than in either app for the reason the diet and day dashboards do: the iPhone composer
// and the Mac composer show the SAME language picker and the SAME progress row, and a
// copy in each target is two wordings that drift.
//
// Both are thin. Every decision they draw — which languages, in what order, what the
// progress means, what a failure says — belongs to `RecordingAttachment` and is tested
// there. These render it.

/// The language choice, raised once a recording has been read and before a word of it is
/// transcribed.
///
/// It is a sheet with an explicit confirm rather than an inline picker that starts on
/// selection, because starting is expensive: the wrong language on an hour of audio is
/// several minutes and a page of nonsense. The device's own languages are at the top of
/// the list (see `TranscriptionLocalePolicy.menu`), and it opens on the last one used.
public struct RecordingLanguageSheet: View {
    @Bindable var model: RecordingAttachment
    @Environment(\.dismiss) private var dismiss

    public init(model: RecordingAttachment) { self.model = model }

    public var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(model.languages, id: \.identifier) { locale in
                        Button {
                            model.selectedLanguage = locale
                        } label: {
                            HStack {
                                Text(TranscriptionLocalePolicy.displayName(locale))
                                    .foregroundStyle(.primary)
                                Spacer()
                                if model.selectedLanguage?.identifier == locale.identifier {
                                    Image(systemName: "checkmark")
                                        .foregroundStyle(.tint)
                                }
                            }
                        }
                        .accessibilityAddTraits(
                            model.selectedLanguage?.identifier == locale.identifier
                                ? [.isSelected] : [])
                    }
                } header: {
                    Text("Language spoken in “\(model.sourceName)”")
                } footer: {
                    Text("Transcribed on the Studio by your Jesse bridge, or on this device when the Studio can’t be reached. The recording goes to the bridge, never to the cloud assistant, and it is deleted in both places as soon as the text exists.")
                }
                if let menu = model.engineMenu, menu.offersAChoice {
                    RecordingEngineSection(menu: menu, selection: $model.selectedEngine)
                }
            }
            .navigationTitle("Transcribe Recording")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Transcribe") { model.confirmLanguage() }
                        .disabled(model.selectedLanguage == nil)
                }
            }
        }
    }
}

/// The running transcription: which phase, how far, who is doing it, and a Cancel that
/// means it.
///
/// It names the phase because the waits are not interchangeable — a first recording can
/// wait on a model download before anything is recognized — and it names the ENGINE
/// because a Studio run on a large model is minutes of work, and a bar that does not say
/// who is working reads as broken rather than busy.
public struct RecordingProgressBar: View {
    let update: TranscriptionUpdate
    let sourceName: String
    let onCancel: () -> Void

    public init(update: TranscriptionUpdate, sourceName: String, onCancel: @escaping () -> Void) {
        self.update = update
        self.sourceName = sourceName
        self.onCancel = onCancel
    }

    /// Pure, so a phase can never be added without a sentence for it — and `nonisolated`,
    /// so a test can hold every phase to one without standing up a view.
    public nonisolated static func label(for update: TranscriptionUpdate, sourceName: String) -> String {
        let percent = Int(update.fraction * 100)
        switch update.phase {
        case .preparing:
            return "Preparing “\(sourceName)”…"
        case .uploading:
            return "Sending “\(sourceName)” to the Studio… \(percent)%"
        case .queued:
            return "Waiting for the Studio to finish another recording…"
        case .downloadingModel:
            return "Downloading the speech model… \(percent)%"
        case .conditioning:
            return "Cleaning up the audio…"
        case .transcribing:
            return "Transcribing “\(sourceName)”… \(percent)%"
        case .secondReading:
            return "Cross-checking with a second engine… \(percent)%"
        case .reconciling:
            return "Comparing the two readings…"
        }
    }

    private var label: String { Self.label(for: update, sourceName: sourceName) }

    public var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                ProgressView(value: update.fraction)
                    .progressViewStyle(.linear)
                if let engine = update.engine {
                    Text("on \(engine)")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
            Button("Cancel", role: .cancel, action: onCancel)
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(update.engine.map { "\(label), on \($0)" } ?? label)
    }
}

/// The one-line notice that a recording was transcribed somewhere other than the Studio,
/// with a way to put it away. Shown by both composers under the text field.
public struct RecordingNoticeRow: View {
    let notice: String
    let onDismiss: () -> Void

    public init(notice: String, onDismiss: @escaping () -> Void) {
        self.notice = notice
        self.onDismiss = onDismiss
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "exclamationmark.bubble")
            Text(notice)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onDismiss) {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dismiss")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}

/// Which engine reads the recording. The Studio's own default is first and preselected; every
/// hosted engine says, in its row, that choosing it sends the recording on from the Studio
/// and where to. Nothing hosted is ever chosen for the owner.
public struct RecordingEngineSection: View {
    let menu: SpeechEngineMenu
    @Binding var selection: String?

    public init(menu: SpeechEngineMenu, selection: Binding<String?>) {
        self.menu = menu
        self._selection = selection
    }

    public var body: some View {
        Section {
            row(id: nil, label: menu.defaultLabel)
            ForEach(menu.engines) { engine in
                row(id: engine.id, label: engine.menuLabel)
            }
        } header: {
            Text("Transcribe with")
        } footer: {
            Text("A hosted engine receives the recording from the Studio to transcribe it, and nothing else does. The transcript says which engine read it.")
        }
    }

    private func row(id: String?, label: String) -> some View {
        Button {
            selection = id
        } label: {
            HStack {
                Text(label).foregroundStyle(.primary)
                Spacer()
                if selection == id {
                    Image(systemName: "checkmark").foregroundStyle(.tint)
                }
            }
        }
        .accessibilityAddTraits(selection == id ? [.isSelected] : [])
    }
}

/// After a failure: the recording is still here, so offer it to another engine, or let it go.
/// Shown by both composers under the error line, for as long as the recording is kept.
public struct RecordingRetryRow: View {
    let offer: RecordingAttachment.RetryOffer
    let onRetry: (String?) -> Void
    let onDiscard: () -> Void

    public init(offer: RecordingAttachment.RetryOffer, onRetry: @escaping (String?) -> Void,
                onDiscard: @escaping () -> Void) {
        self.offer = offer
        self.onRetry = onRetry
        self.onDiscard = onDiscard
    }

    public var body: some View {
        HStack(spacing: 8) {
            if offer.engines.count > 1 {
                Menu("Try again with…") {
                    ForEach(offer.engines) { engine in
                        Button(engine.menuLabel) { onRetry(engine.id) }
                    }
                }
                .accessibilityLabel("Try “\(offer.sourceName)” again with another engine")
            } else {
                Button("Try again") { onRetry(nil) }
                    .accessibilityLabel("Try transcribing “\(offer.sourceName)” again")
            }
            Button("Discard", role: .destructive, action: onDiscard)
                .accessibilityLabel("Discard “\(offer.sourceName)”")
            Spacer()
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .font(.caption)
    }
}
