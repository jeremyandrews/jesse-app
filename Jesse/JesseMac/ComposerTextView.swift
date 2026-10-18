import AppKit
import SwiftUI

// The macOS composer's text input: an `NSTextView` in an `NSScrollView`, wrapped for SwiftUI.
//
// It exists because a SwiftUI `TextField` cannot tell Return from Shift plus Return. The field
// consumes the key itself and reports it through `.onSubmit`, which carries no modifier state,
// so "send on Return, newline on Shift plus Return" is not expressible there at any level of
// cleverness. `NSTextView.keyDown(with:)` gets the whole `NSEvent`, modifiers included, which is
// the lowest layer that still knows whether the composer has focus.
//
// Two tempting shortcuts are wrong and were not used:
//   * `.keyboardShortcut(.return, modifiers: …)` cannot express "Return with any modifier", and a
//     shortcut competes with the focused text view for the key rather than cooperating with it.
//   * `NSEvent.addLocalMonitorForEvents` sees every key in the process regardless of focus, so it
//     would hijack Return in the sidebar, the search field, and every sheet in the app.
//
// Everything AppKit already gives a text view (paste, copy, cut, select all, undo and redo, spell
// check, autocorrect, dictation, the emoji palette, the Services menu, the context menu) keeps
// working because the only key this class intercepts is Return, and only when
// `composerKeyAction` says so.

/// The composer's text input. Grows with its content from `minLines` up to `maxLines`, then
/// scrolls. Return sends, Return with a modifier inserts a newline (see `composerKeyAction`).
struct ComposerTextView: NSViewRepresentable {
    @Binding var text: String
    /// Drawn in place of the text when the composer is empty.
    var placeholder: String = ""
    /// Height floor and ceiling, in lines of the composer's font.
    var minLines: Int = 1
    var maxLines: Int = 8
    /// Whether to take focus when the composer first appears.
    var focusOnAppear: Bool = true
    /// Invoked for a plain Return. The gate on whether a send is ALLOWED is not here: this
    /// closure is the same `send()` the send button calls, and that function's guard (which the
    /// button's `disabled` state mirrors) stays the single source of truth.
    var onSend: () -> Void
    /// Invoked when a Paste, a drop onto the text, or Continuity Camera brings an image or a
    /// PDF: the items read off the pasteboard, for the composer to stage as attachments. When
    /// nil the text view keeps AppKit's own behaviour for all three. Text is never routed here.
    var onMedia: (([MacMediaItem]) -> Void)? = nil

    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }

    func makeNSView(context: Context) -> NSScrollView {
        let textView = ComposerNSTextView()
        textView.delegate = context.coordinator
        textView.onSend = onSend
        textView.onMedia = onMedia
        textView.placeholder = placeholder
        textView.string = text

        textView.isEditable = true
        textView.isSelectable = true
        textView.allowsUndo = true
        textView.isRichText = false
        textView.importsGraphics = false
        textView.font = ComposerNSTextView.composerFont
        textView.textColor = .labelColor
        textView.drawsBackground = false
        textView.textContainerInset = NSSize(width: 0, height: 0)
        textView.textContainer?.lineFragmentPadding = 0
        textView.textContainer?.widthTracksTextView = true
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        // Room to grow taller than the scroll view. Unset, AppKit kept the maximum at the
        // frame the clip view gave the text view, so past `maxLines` the text view stopped at
        // the scroll view's height, there was nothing to scroll, and the caret's line sat
        // below the visible rect: a composer that grew but hid the line being typed.
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                                  height: CGFloat.greatestFiniteMagnitude)

        // Match a SwiftUI TextField: spell check and autocorrect on. Smart quote and smart dash
        // substitution stay OFF: a composer whose text goes to a coding agent must deliver what
        // was typed, and a curly quote or a substituted dash in a path or a code fence is a bug,
        // not a nicety.
        textView.isContinuousSpellCheckingEnabled = true
        textView.isAutomaticSpellingCorrectionEnabled = true
        textView.isAutomaticTextReplacementEnabled = true
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false

        // Accept image and PDF drags as well as the text and file drags a text view already
        // takes, so a Photos item or a screenshot thumbnail reaches `performDragOperation`.
        textView.registerForDraggedTypes(
            Array(Set(textView.registeredDraggedTypes + MacPasteboardMedia.pasteboardTypes + [.fileURL])))
        // After the font, which `font =` sets across the whole storage.
        textView.markdownStyler.restyle()

        let scrollView = NSScrollView()
        scrollView.documentView = textView
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.hasHorizontalScroller = false
        scrollView.verticalScrollElasticity = .none

        if focusOnAppear {
            // Once, on appearance, the way a chat window behaves. Deferred because the view has
            // no window yet during `makeNSView`.
            DispatchQueue.main.async { [weak textView] in
                guard let textView, let window = textView.window else { return }
                // Never yank focus off another text input the user is already typing in.
                if window.firstResponder is NSText || window.firstResponder is NSTextView { return }
                window.makeFirstResponder(textView)
            }
        }
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let textView = scrollView.documentView as? ComposerNSTextView else { return }
        // Refreshed every update, not captured once: the closure reads the view's current state,
        // so a stale one would send a stale draft.
        textView.onSend = onSend
        textView.onMedia = onMedia
        textView.placeholder = placeholder
        context.coordinator.apply(text, to: textView)
    }

    /// Report the composer's height: the laid-out text, clamped to the line floor and ceiling.
    /// Width is passed straight through so the field stays horizontally flexible in its HStack.
    ///
    /// The height is read from the text view's OWN layout, and only as far as the ceiling: past
    /// `maxLines` the answer is the ceiling whatever the rest of the text holds, so nothing below
    /// it is laid out to find out. This used to measure the whole string with `boundingRect` on
    /// every call, and SwiftUI asks several times per layout pass: a single measurement of a
    /// 1 MB paste took 66 s, so the app never came back.
    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        guard let textView = nsView.documentView as? ComposerNSTextView else { return nil }
        textView.sizeQueries += 1
        let width: CGFloat
        if let proposed = proposal.width, proposed > 0, proposed < .infinity {
            width = proposed
        } else {
            width = Self.idealWidth
        }
        let lineHeight = ComposerNSTextView.lineHeight
        let ceiling = lineHeight * CGFloat(max(maxLines, max(minLines, 1)))
        let height = ComposerHeight.clamped(
            textHeight: textView.measuredTextHeight(forWidth: width, upTo: ceiling,
                                                    scroller: nsView),
            lineHeight: lineHeight,
            minLines: minLines,
            maxLines: maxLines,
            verticalInset: textView.textContainerInset.height * 2)
        return CGSize(width: width, height: height)
    }

    /// Only used to answer an unspecified or infinite width proposal; the call site's
    /// `maxWidth: .infinity` is what actually makes the field fill its row.
    private static let idealWidth: CGFloat = 240

    /// Bridges the text view's edits into the SwiftUI binding and back, without the caret
    /// jumping. The classic `NSViewRepresentable` defect is `updateNSView` writing the binding
    /// into the text view on every keystroke: assigning `string` collapses the selection, so the
    /// caret lands at the end and the user cannot type in the middle of their own message. The
    /// fix is `apply`'s equality guard, which makes the round trip of the view's OWN edit a
    /// no-op.
    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private let text: Binding<String>

        /// The composer's OWN undo manager, deliberately not the window's.
        ///
        /// Two reasons, and the second one is a crash. First, the composer's edits are its own
        /// business and should not interleave with anything else in the window. Second, `apply`
        /// replaces the text wholesale (a completed send clears the draft) and such a replacement
        /// is NOT an undoable edit, so every undo action recorded before it now describes ranges
        /// in text that no longer exists. Undoing across that boundary throws `NSRangeException`
        /// ("Range {0, 5} out of bounds; string length 0") and takes the app down. Owning the
        /// stack is what makes it safe to clear at that exact moment.
        let composerUndoManager = UndoManager()

        init(text: Binding<String>) {
            self.text = text
        }

        func undoManager(for view: NSTextView) -> UndoManager? { composerUndoManager }

        /// The value this coordinator last read off the text view and wrote into the binding.
        ///
        /// It is what makes the echo check cheap. Comparing the binding with `textView.string`
        /// compares a Swift string with a bridged `NSString` character by character, with
        /// Unicode normalization, and that ran twice per keystroke: after a 1 MB paste each
        /// key cost about 140 ms. The value that comes back through SwiftUI is this same
        /// string, and two strings sharing storage compare equal without reading it.
        private var echoed: String?

        func textDidChange(_ notification: Notification) {
            guard let textView = notification.object as? NSTextView else { return }
            // Native and contiguous once, here, rather than a lazily bridged copy of the text
            // storage that every later read of the draft would pay for again.
            var current = textView.string
            current.makeContiguousUTF8()
            echoed = current
            text.wrappedValue = current
        }

        /// Push a SwiftUI-side value into the text view. Does nothing when the text view already
        /// holds it (the echo of its own edit), which is what keeps the selection intact.
        func apply(_ newValue: String, to textView: NSTextView) {
            if let echoed, newValue == echoed { return }
            guard textView.string != newValue else { return }
            let previous = textView.selectedRange()
            textView.string = newValue
            echoed = newValue
            // An external change (a send clearing the draft, a restored value): keep the caret
            // where it was if that is still inside the new text, else put it at the end.
            let length = (newValue as NSString).length
            let location = min(previous.location, length)
            textView.setSelectedRange(NSRange(location: location, length: 0))
            // This replacement is not undoable, so nothing recorded before it can be undone
            // safely (see `composerUndoManager`). Start the composer's history fresh.
            composerUndoManager.removeAllActions()
            (textView as? ComposerNSTextView)?.markdownStyler.restyle()
            textView.needsDisplay = true
        }
    }
}

/// The height clamp, kept apart from the view so it can be tested directly. `nonisolated` for the
/// same reason as `composerKeyAction`: it is arithmetic, not UI state.
nonisolated enum ComposerHeight {
    /// The composer's height for text that lays out to `textHeight`: never shorter than
    /// `minLines`, never taller than `maxLines` (past which the text view scrolls).
    static func clamped(textHeight: CGFloat, lineHeight: CGFloat,
                        minLines: Int, maxLines: Int, verticalInset: CGFloat) -> CGFloat {
        let floor = lineHeight * CGFloat(max(minLines, 1))
        let ceiling = lineHeight * CGFloat(max(maxLines, max(minLines, 1)))
        return min(max(textHeight, floor), ceiling) + verticalInset
    }
}

/// The composer's `NSTextView`. Its job is the Return key, and routing media (a pasted, dropped
/// or Continuity Camera image or PDF) to the composer's attachments instead of into the text.
/// Everything else is inherited.
final class ComposerNSTextView: NSTextView {
    /// Invoked when a key press means "send".
    var onSend: (() -> Void)?
    /// Invoked with the media read off a pasteboard (see `ComposerTextView.onMedia`).
    var onMedia: (([MacMediaItem]) -> Void)?

    // MARK: Layout: TextKit 1, non contiguous

    /// Instrumentation: how many times SwiftUI has asked this composer for its size. A test
    /// reads it to prove a paste settles in a bounded number of layout passes.
    var sizeQueries = 0
    /// Instrumentation: how many characters the last height measurement had laid out. A test
    /// reads it to prove a measurement stops at the ceiling instead of typesetting the paste.
    var lastMeasuredCharacters = 0

    /// The text storage at the root of this view's TextKit 1 stack. AppKit's text storage owns
    /// the layout manager and the container, and nothing in that stack owns the storage, so
    /// the view keeps it.
    private var ownedStorage: NSTextStorage?

    /// Styles the Markdown being typed, display only (see `ComposerMarkdown`).
    let markdownStyler = ComposerMarkdownStyler()

    /// An explicit TextKit 1 stack with non contiguous layout, so a large paste lays out only
    /// what is on screen and what the height measurement asks for, never the whole text up
    /// front. Explicit rather than the implicit fallback reading `layoutManager` triggers on a
    /// TextKit 2 view, because that fallback is not something to build on deliberately.
    override init(frame frameRect: NSRect) {
        let storage = NSTextStorage()
        let manager = NSLayoutManager()
        manager.allowsNonContiguousLayout = true
        storage.addLayoutManager(manager)
        let container = NSTextContainer(
            size: NSSize(width: frameRect.width, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        manager.addTextContainer(container)
        super.init(frame: frameRect, textContainer: container)
        ownedStorage = storage
        attachStyler()
    }

    override init(frame frameRect: NSRect, textContainer container: NSTextContainer?) {
        super.init(frame: frameRect, textContainer: container)
        attachStyler()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        attachStyler()
    }

    private func attachStyler() {
        markdownStyler.textView = self
        textStorage?.delegate = markdownStyler
    }

    /// Text typed next is always plain body text. AppKit takes the typing attributes from the
    /// character before the caret, which may be a styled one; styling is the styler's to
    /// apply, and a big paste must arrive unstyled so dropping styling past the limit is free.
    override var typingAttributes: [NSAttributedString.Key: Any] {
        get { super.typingAttributes }
        set {
            var plain = newValue
            plain[.font] = Self.composerFont
            plain[.foregroundColor] = NSColor.labelColor
            super.typingAttributes = plain
        }
    }

    // MARK: Media: Paste, drop, Continuity Camera

    /// Hand `pasteboard`'s media to the composer, if it has any and the composer takes it.
    /// False means "not media": the caller falls through to AppKit's text behaviour.
    @discardableResult
    func routeMedia(from pasteboard: NSPasteboard) -> Bool {
        guard let onMedia, let items = MacPasteboardMedia.read(pasteboard) else { return false }
        onMedia(items)
        return true
    }

    /// Paste: an image or a PDF on the clipboard is staged, verbatim; text pastes as text.
    override func paste(_ sender: Any?) {
        if routeMedia(from: .general) { return }
        super.paste(sender)
    }

    /// Paste and Match Style is the same decision; there is no style to match on a photo.
    override func pasteAsPlainText(_ sender: Any?) {
        if routeMedia(from: .general) { return }
        super.pasteAsPlainText(sender)
    }

    override func validateUserInterfaceItem(_ item: any NSValidatedUserInterfaceItem) -> Bool {
        // `isRichText` is false, so AppKit would grey Paste out for a clipboard holding only
        // an image. With media routing on, an image or a PDF is something to paste.
        if item.action == #selector(paste(_:)) || item.action == #selector(pasteAsPlainText(_:)),
           onMedia != nil, MacPasteboardMedia.hasMedia(.general) {
            return isEditable
        }
        return super.validateUserInterfaceItem(item)
    }

    /// A drag carrying an image or PDF (a Finder file, a Photos item, a screenshot thumbnail,
    /// a Preview page) is offered as a copy into the attachments, not as a path in the text.
    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        if onMedia != nil, MacPasteboardMedia.hasMedia(sender.draggingPasteboard) { return .copy }
        return super.draggingEntered(sender)
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        if onMedia != nil, MacPasteboardMedia.hasMedia(sender.draggingPasteboard) { return .copy }
        return super.draggingUpdated(sender)
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        if routeMedia(from: sender.draggingPasteboard) { return true }
        return super.performDragOperation(sender)
    }

    /// CONTINUITY CAMERA. AppKit offers "Take Photo" and "Scan Documents" (the iPhone's
    /// camera, from this text view's context menu) only to a responder that says it can take
    /// an image back. A plain-text view cannot, so this one says yes for image and PDF return
    /// types when nothing is being sent, and `readSelection(from:)` then stages what comes
    /// back as an attachment, never into the text. This is the Mac's Take Photo: no capture
    /// view of its own and no camera permission, because the camera is the phone's.
    override func validRequestor(forSendType sendType: NSPasteboard.PasteboardType?,
                                 returnType: NSPasteboard.PasteboardType?) -> Any? {
        if onMedia != nil, isEditable, (sendType == nil || sendType?.rawValue.isEmpty == true),
           let returnType, MacPasteboardMedia.isMediaType(returnType.rawValue) {
            return self
        }
        return super.validRequestor(forSendType: sendType, returnType: returnType)
    }

    override func readSelection(from pboard: NSPasteboard) -> Bool {
        if routeMedia(from: pboard) { return true }
        return super.readSelection(from: pboard)
    }

    /// Drawn when the composer is empty. `NSTextView` has no placeholder of its own.
    var placeholder: String = "" {
        didSet { if placeholder != oldValue { needsDisplay = true } }
    }

    static let composerFont = NSFont.preferredFont(forTextStyle: .body)
    /// One line of the composer's font, used for the height floor and ceiling.
    static let lineHeight = ceil(NSLayoutManager().defaultLineHeight(for: composerFont))

    override func keyDown(with event: NSEvent) {
        switch composerKeyAction(keyCode: event.keyCode,
                                 modifiers: event.modifierFlags,
                                 hasMarkedText: hasMarkedText()) {
        case .send:
            onSend?()
        case .insertNewline:
            // Through the text input path rather than by rewriting `string`, so the newline is a
            // normal edit: one undo step, current typing attributes, marked text respected.
            insertText("\n", replacementRange: selectedRange())
            scrollRangeToVisible(selectedRange())
        case .passThrough:
            super.keyDown(with: event)
        }
    }

    override func didChangeText() {
        super.didChangeText()
        markdownStyler.restyle()
        // The placeholder appears and disappears with emptiness.
        needsDisplay = true
    }

    /// The fence tints and code chips go down with the background, under the text and the
    /// selection.
    override func drawBackground(in rect: NSRect) {
        super.drawBackground(in: rect)
        markdownStyler.drawBackground(in: rect)
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard string.isEmpty, !placeholder.isEmpty else { return }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font ?? Self.composerFont,
            .foregroundColor: NSColor.placeholderTextColor,
        ]
        let padding = textContainer?.lineFragmentPadding ?? 0
        let origin = NSPoint(x: textContainerInset.width + padding, y: textContainerInset.height)
        (placeholder as NSString).draw(at: origin, withAttributes: attributes)
    }

    /// How tall the current text lays out at `width`, before clamping, read from this view's
    /// own layout manager and laid out no further than `ceiling`: once the text is taller than
    /// that, the answer is "at least the ceiling" and the rest is never typeset for it.
    ///
    /// `scroller`, when given, is the enclosing scroll view. A text taller than the ceiling
    /// shows a vertical scroller, and a legacy (not overlay) scroller takes its width from the
    /// text, so the width is decided once, here, from which side of the ceiling the text is on.
    /// Narrower text is never shorter, so the decision cannot flip back and forth between
    /// layout passes.
    func measuredTextHeight(forWidth width: CGFloat, upTo ceiling: CGFloat = .greatestFiniteMagnitude,
                            scroller: NSScrollView? = nil) -> CGFloat {
        guard !string.isEmpty, let manager = layoutManager, let container = textContainer else {
            return Self.lineHeight
        }
        let usable = max(width - textContainerInset.width * 2, 1)
        var height = laidOutHeight(manager, container, width: usable, ceiling: ceiling)
        if height >= ceiling, let scroller, scroller.scrollerStyle == .legacy {
            let scrollerWidth = NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
            height = laidOutHeight(manager, container, width: max(usable - scrollerWidth, 1),
                                   ceiling: ceiling)
        }
        // The container tracks this view's width only when the frame CHANGES, so a probe at
        // another width (SwiftUI asks at zero, at infinity and with no proposal at all) would
        // otherwise leave the text wrapped for a width it is not drawn at.
        let drawn = frame.width - textContainerInset.width * 2
        if drawn > 0, container.size.width != drawn {
            container.size = NSSize(width: drawn, height: CGFloat.greatestFiniteMagnitude)
        }
        return height
    }

    /// The used height of the text at `width`, laid out only down to `ceiling`. The extra line
    /// fragment a trailing newline opens is part of the used rect, so the composer grows on the
    /// newline rather than on the next character.
    private func laidOutHeight(_ manager: NSLayoutManager, _ container: NSTextContainer,
                               width: CGFloat, ceiling: CGFloat) -> CGFloat {
        if container.size.width != width {
            container.size = NSSize(width: width, height: CGFloat.greatestFiniteMagnitude)
        }
        let bounded = ceiling.isFinite ? ceiling + Self.lineHeight : CGFloat.greatestFiniteMagnitude
        manager.ensureLayout(forBoundingRect: NSRect(x: 0, y: 0, width: width, height: bounded),
                             in: container)
        let firstUnlaid = manager.firstUnlaidCharacterIndex()
        lastMeasuredCharacters = firstUnlaid
        let laidOut = firstUnlaid >= (string as NSString).length
        let used = ceil(manager.usedRect(for: container).height)
        // Text that is not all laid out yet extends below the rect just laid out, which is
        // already past the ceiling.
        return laidOut ? used : max(used, ceiling)
    }
}
