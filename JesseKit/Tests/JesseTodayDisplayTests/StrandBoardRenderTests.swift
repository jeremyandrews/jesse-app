import XCTest
import SwiftUI
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
@testable import JesseTodayDisplay
import JesseNetworking

// **The board, drawn.** Every other assertion about a strand's colour is a number; this
// one produces the picture those numbers are supposed to add up to, so a human can look
// at the thing Jeremy looked at when he called the old palette a mess.
//
// It renders the `Tree` lens — the eight roots and the children the vault has under them
// — in both appearances, through the REAL `StrandRow` and the real `StrandTone`, with the
// same indent and chevron columns `StrandsListView.treeRow` lays out, and writes a PNG
// per appearance. `ImageRenderer` in process: no simulator, no UI automation, no
// permission prompt, nothing on a device.
//
// The rows are stacked in a plain `VStack` rather than a `List`, because `ImageRenderer`
// renders a `ScrollView` or a `LazyVStack` as blank. Everything inside one row is the
// shipping view, which is the half that can be wrong about a colour.
//
// The paths are printed. `STRAND_BOARD_PNG_DIR` overrides where they land, which is how
// the two attached to the pull request were collected.
//
// The image handling is CoreGraphics and ImageIO only, on purpose: `renderer.cgImage`, an
// ImageIO PNG encode, and a pixel read from an RGBA buffer. JesseKit declares iOS and
// macOS, so this file compiles for both, and an AppKit type here (the first version used
// `nsImage` and `NSBitmapImageRep`) breaks the package's iOS test build. No `#if`, because
// a fence would drop the iOS half of the render without anything reporting the loss.

@MainActor
final class StrandBoardRenderTests: XCTestCase {

    /// One row of the board as the `Tree` lens lays it out: depth, and whether it has
    /// children of its own.
    private struct Row {
        var slug: String
        var title: String
        var group: TodayProject
        var parent: String?
        var now: String
        var hasChildren: Bool
    }

    /// The live board, 2026-09-26: eight roots, and what sits under them.
    private let board: [Row] = [
        Row(slug: "Tag1", title: "Tag1", group: .tag1, parent: nil,
            now: "Financials and the DrupalCon slate are the two live fronts.", hasChildren: true),
        Row(slug: "Jesse", title: "Jesse", group: .tag1, parent: "Tag1",
            now: "Strand colours on the board are being rebuilt.", hasChildren: true),
        Row(slug: "Strands-System", title: "Strands System", group: .tag1, parent: "Jesse",
            now: "The audit resolves links against the filesystem now.", hasChildren: false),
        Row(slug: "Vault-Links", title: "Vault Links", group: .tag1, parent: "Jesse",
            now: "Default on since the retrieval audit.", hasChildren: false),
        Row(slug: "Scolta", title: "Scolta", group: .tag1, parent: "Tag1",
            now: "Composer advisory rows are policy, not a puzzle.", hasChildren: false),
        Row(slug: "DrupalCon-Rotterdam", title: "DrupalCon Rotterdam", group: .tag1, parent: "Tag1",
            now: "Talk slate submitted.", hasChildren: true),
        Row(slug: "Scolta-DrupalCon-Talk", title: "Scolta DrupalCon Talk", group: .tag1,
            parent: "DrupalCon-Rotterdam", now: "Outline only.", hasChildren: false),
        Row(slug: "Homelab", title: "Homelab", group: .network, parent: nil,
            now: "epyc stays the storage head for months.", hasChildren: true),
        Row(slug: "K3s-Cluster", title: "K3s Cluster", group: .network, parent: "Homelab",
            now: "A2 paused on three answers, S4 waiting on a push.", hasChildren: false),
        Row(slug: "Perseido", title: "Perseido", group: .perseido, parent: nil,
            now: "The rebrand site is still on Gandi.", hasChildren: false),
        Row(slug: "Via-Con-Me", title: "Via Con Me", group: .viaConMe, parent: nil,
            now: "Nothing running.", hasChildren: false),
        Row(slug: "Family", title: "Family", group: .personal, parent: nil,
            now: "Term dates and the winter trip.", hasChildren: true),
        Row(slug: "Greta", title: "Greta", group: .personal, parent: "Family",
            now: "Nothing running.", hasChildren: false),
        Row(slug: "Trovato", title: "Trovato", group: .personal, parent: nil,
            now: "The independent security review is the long pole for 1.0.", hasChildren: true),
        Row(slug: "Trovato-Core", title: "Trovato Core", group: .personal, parent: "Trovato",
            now: "B1 test isolation runs next.", hasChildren: false),
        Row(slug: "Netgrasp", title: "Netgrasp", group: .personal, parent: "Trovato",
            now: "Nothing running.", hasChildren: false),
        Row(slug: "Argus", title: "Argus", group: .personal, parent: "Trovato",
            now: "Nothing running.", hasChildren: false),
        Row(slug: "Health", title: "Health", group: .personal, parent: nil,
            now: "Weigh in logged, the strength reminder stands.", hasChildren: false),
        Row(slug: "Tangent", title: "Tangent", group: .personal, parent: nil,
            now: "Philosophy study and the currency report.", hasChildren: false),
    ]

    private var strands: [Strand] {
        board.map { Strand(slug: $0.slug, title: $0.title, group: $0.group,
                           updated: "2026-09-26", now: $0.now, parent: $0.parent) }
    }

    private func depth(of row: Row) -> Int {
        var depth = 0
        var current = row
        while let parentSlug = current.parent,
              let parent = board.first(where: { $0.slug == parentSlug }), depth < 8 {
            depth += 1
            current = parent
        }
        return depth
    }

    /// One row, ready to draw: the wire value, its indent depth and whether it has a
    /// chevron. Prepared before the view is built so the `ForEach` closure captures a
    /// value array and not the test case.
    private struct Prepared: Identifiable {
        var id: String { strand.slug }
        var strand: Strand
        var depth: Int
        var hasChildren: Bool
        var tone: TodayProjectColor
    }

    private func prepared(_ scheme: ColorScheme) -> [Prepared] {
        let family = StrandFamily(strands: strands, scheme: scheme)
        return board.map { row in
            Prepared(strand: Strand(slug: row.slug, title: row.title, group: row.group,
                                    updated: "2026-09-26", now: row.now, parent: row.parent),
                     depth: depth(of: row), hasChildren: row.hasChildren,
                     tone: family.tone(row.slug))
        }
    }

    /// The `Tree` lens, laid out as `StrandsListView.treeRow` lays it out: blank indent,
    /// chevron column, then the row and its single accent bar.
    private func lens(_ scheme: ColorScheme) -> some View {
        let rows = prepared(scheme)
        let background = scheme == .dark ? StrandTone.darkBackground : StrandTone.lightBackground
        return VStack(alignment: .leading, spacing: 0) {
            Text("Strands · Tree · \(scheme == .dark ? "dark" : "light")")
                .font(.headline)
                .padding(.bottom, 8)
            ForEach(rows) { row in
                HStack(alignment: .top, spacing: 2) {
                    if row.depth > 0 {
                        Color.clear
                            .frame(width: CGFloat(row.depth) * StrandsListView.treeIndent)
                    }
                    Group {
                        if row.hasChildren {
                            Image(systemName: "chevron.down")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .frame(width: 20, height: 24)
                        } else {
                            Color.clear.frame(width: 20, height: 24)
                        }
                    }
                    .padding(.vertical, StrandsListView.treeRowPadding)
                    StrandRow(strand: row.strand,
                              tone: row.tone,
                              referenceDay: "2026-09-26",
                              isOpening: false,
                              onOpen: {},
                              onShowFindings: {})
                        .padding(.vertical, StrandsListView.treeRowPadding)
                    Spacer(minLength: 0)
                }
                .fixedSize(horizontal: false, vertical: true)
                Divider().opacity(0.35)
            }
        }
        .padding(16)
        .frame(width: 430, alignment: .leading)
        .background(background.color)
        .environment(\.colorScheme, scheme)
    }

    /// **Draws the board in both appearances and writes a PNG for each.** The assertions
    /// are the ones a render test can honestly make — the file exists, it is a real PNG of
    /// the expected size, and it is not one flat colour — and the point of it is the file.
    func testTheTreeLensRendersInBothAppearances() throws {
        let directory = ProcessInfo.processInfo.environment["STRAND_BOARD_PNG_DIR"]
            .map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.temporaryDirectory
                .appendingPathComponent("strand-board-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        for scheme in [ColorScheme.light, .dark] {
            let name = scheme == .dark ? "strands-tree-dark.png" : "strands-tree-light.png"
            let renderer = ImageRenderer(content: lens(scheme))
            renderer.scale = 2
            let image = try XCTUnwrap(renderer.cgImage, "\(scheme) rendered nothing")
            // Pixels at scale 2: the old 400 point floor.
            XCTAssertGreaterThan(image.width, 800, "\(scheme)")
            XCTAssertGreaterThan(image.height, 800, "\(scheme)")
            let png = NSMutableData()
            let destination = try XCTUnwrap(
                CGImageDestinationCreateWithData(png, UTType.png.identifier as CFString, 1, nil),
                "\(scheme) has no PNG destination")
            CGImageDestinationAddImage(destination, image, nil)
            XCTAssertTrue(CGImageDestinationFinalize(destination), "\(scheme) would not encode")
            let url = directory.appendingPathComponent(name)
            try (png as Data).write(to: url)
            print("STRAND BOARD PNG \(scheme): \(url.path)")

            // A blank render is the failure mode that matters here: `ImageRenderer` hands
            // back an empty image for a scrolling container, and an all-one-colour PNG
            // would sail past a "the file exists" check.
            let colours = try distinctColours(image)
            print("STRAND BOARD COLOURS \(scheme): \(colours)")
            XCTAssertGreaterThan(colours, 40,
                                 "\(scheme) rendered fewer colours than a board has tones")
        }
    }

    /// Roughly how many colours the render holds, sampled on a grid. Enough to tell a
    /// drawn board from a blank rectangle. The image is drawn once into an sRGB RGBA8
    /// buffer, so every pixel is read in the same space on either platform.
    private func distinctColours(_ image: CGImage) throws -> Int {
        let width = image.width, height = image.height, bytesPerRow = width * 4
        var pixels = [UInt8](repeating: 0, count: bytesPerRow * height)
        let space = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        let drawn: Bool = pixels.withUnsafeMutableBytes { buffer in
            guard let context = CGContext(data: buffer.baseAddress, width: width, height: height,
                                          bitsPerComponent: 8, bytesPerRow: bytesPerRow, space: space,
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        XCTAssertTrue(drawn, "no RGBA context for a \(width)x\(height) image")
        var seen: Set<Int> = []
        for x in stride(from: 0, to: width, by: 3) {
            for y in stride(from: 0, to: height, by: 3) {
                let i = y * bytesPerRow + x * 4
                seen.insert(Int(pixels[i]) << 16 | Int(pixels[i + 1]) << 8 | Int(pixels[i + 2]))
            }
        }
        return seen.count
    }
}
