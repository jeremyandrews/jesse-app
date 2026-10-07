import AppKit
import SwiftUI
import JesseCore
import JesseNetworking

/// A compact row of a turn's persisted attachment previews (1..N), the Mac twin of the
/// phone's `TurnAttachmentsView`: the same 78 pt rounded thumbnails, the same PDF corner
/// badge, the same typed placeholder when a thumbnail does not decode, and the same
/// accessibility label. Each is the small JPEG in `TurnAttachment.thumbnail`; the
/// full-resolution file is never on this Mac's disk. The caller skips turns with none.
struct MacTurnAttachmentsView: View {
    let attachments: [TurnAttachment]

    static let side: CGFloat = 78

    var body: some View {
        HStack(spacing: 8) {
            ForEach(attachments) { att in
                thumbnail(att)
            }
        }
    }

    @ViewBuilder
    private func thumbnail(_ att: TurnAttachment) -> some View {
        ZStack(alignment: .bottomTrailing) {
            if let image = NSImage(data: att.thumbnail) {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(width: Self.side, height: Self.side)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
            } else {
                RoundedRectangle(cornerRadius: 10)
                    .fill(Color.secondary.opacity(0.15))
                    .frame(width: Self.side, height: Self.side)
                    .overlay(
                        Image(systemName: att.isPDF ? "doc.text" : "photo")
                            .foregroundStyle(.secondary))
            }
            if att.isPDF {
                Image(systemName: "doc.text.fill")
                    .font(.caption2)
                    .foregroundStyle(.white)
                    .padding(4)
                    .background(.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 5))
                    .padding(4)
            }
        }
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(Color.secondary.opacity(0.25), lineWidth: 0.5))
        .help(att.filename)
        .accessibilityElement()
        .accessibilityLabel(att.isPDF ? "PDF attachment: \(att.filename)"
                                      : "Image attachment: \(att.filename)")
    }
}

/// The composer's staged files as removable chips (icon, filename, remove button), the
/// same chip the phone draws above its composer.
struct MacAttachmentChips: View {
    let attachments: [JesseAttachment]
    let onRemove: (JesseAttachment) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(attachments) { att in
                    HStack(spacing: 6) {
                        Image(systemName: att.isImage ? "photo" : "doc.text")
                        Text(att.filename)
                            .font(.caption)
                            .lineLimit(1)
                        Button {
                            onRemove(att)
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Remove \(att.filename)")
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(Color.secondary.opacity(0.15))
                    .clipShape(Capsule())
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
