import SwiftUI

/// A piece glyph: white pieces white with a dark outline, black pieces #020825.
@MainActor
struct PieceGlyph: View {
    let piece: Piece
    let size: CGFloat

    var body: some View {
        let t = Text(piece.glyph).font(.system(size: size * 0.82))
        if piece.isWhite {
            ZStack {
                // A dark outline from four offset copies, then the white fill.
                t.foregroundStyle(Color.ocBlackPiece).offset(x: 1, y: 0)
                t.foregroundStyle(Color.ocBlackPiece).offset(x: -1, y: 0)
                t.foregroundStyle(Color.ocBlackPiece).offset(x: 0, y: 1)
                t.foregroundStyle(Color.ocBlackPiece).offset(x: 0, y: -1)
                t.foregroundStyle(Color.white)
            }
            .shadow(color: Color.black.opacity(0.35), radius: 1, x: 0, y: 1)
        } else {
            t.foregroundStyle(Color.ocBlackPiece)
                .shadow(color: Color.white.opacity(0.25), radius: 0.5, x: 0, y: 0)
        }
    }
}

/// An arrow drawn over the board, square to square.
struct BoardArrow: Hashable {
    let from: Int
    let to: Int
    let color: Color
}

/// What's drawn over the board besides the pieces: arrows (a best move, a
/// piece to put back), square tints (a piece on the wrong square), ghosted
/// pieces (one missing from the board) and remove badges.
struct BoardMarks: Equatable {
    var arrows: [BoardArrow] = []
    var tints: [Int: Color] = [:]
    var ghosts: [Int: Piece] = [:]
    var removals: Set<Int> = []

    var isEmpty: Bool { arrows.isEmpty && tints.isEmpty && ghosts.isEmpty && removals.isEmpty }
}

/// A short side-to-side shake (an illegal tap): animate `shakes` up by one.
struct ShakeEffect: GeometryEffect {
    var shakes: CGFloat

    var animatableData: CGFloat {
        get { shakes }
        set { shakes = newValue }
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        let x = 6 * sin(shakes * .pi * 4)
        return ProjectionTransform(CGAffineTransform(translationX: x, y: 0))
    }
}

/// An arrow as one filled outline: a shaft and a clear head.
struct ArrowShape: Shape {
    let from: CGPoint
    let to: CGPoint
    let width: CGFloat

    func path(in rect: CGRect) -> Path {
        var p = Path()
        let dx = to.x - from.x
        let dy = to.y - from.y
        let length = max(1, (dx * dx + dy * dy).squareRoot())
        let ux = dx / length
        let uy = dy / length
        let nx = -uy
        let ny = ux
        let head = min(length * 0.55, width * 2.6)
        let headHalf = width * 1.5
        let half = width / 2
        let neck = CGPoint(x: to.x - ux * head, y: to.y - uy * head)
        p.move(to: CGPoint(x: from.x + nx * half, y: from.y + ny * half))
        p.addLine(to: CGPoint(x: neck.x + nx * half, y: neck.y + ny * half))
        p.addLine(to: CGPoint(x: neck.x + nx * headHalf, y: neck.y + ny * headHalf))
        p.addLine(to: to)
        p.addLine(to: CGPoint(x: neck.x - nx * headHalf, y: neck.y - ny * headHalf))
        p.addLine(to: CGPoint(x: neck.x - nx * half, y: neck.y - ny * half))
        p.addLine(to: CGPoint(x: from.x - nx * half, y: from.y - ny * half))
        p.closeSubpath()
        return p
    }
}

/// The board: tap a piece, then tap where it goes. The caller decides what
/// a tap means (moves in a game, setting pieces in the editor).
@MainActor
struct BoardView: View {
    let placement: Placement
    var flipped = false
    var selected: Int?
    var lastMove: UCIMove?
    var hint: UCIMove?
    var targets: Set<Int> = []
    var unsure: Set<Int> = []
    /// The king in check (tinted red).
    var check: Int?
    /// The piece to shake, and a counter bumped for each shake.
    var shakeSquare: Int?
    var shakes: Int = 0
    var marks = BoardMarks()
    var onTap: (Int) -> Void = { _ in }

    var body: some View {
        GeometryReader { geo in
            let side = min(geo.size.width, geo.size.height)
            let cell = side / 8
            VStack(spacing: 0) {
                ForEach(0..<8, id: \.self) { row in
                    HStack(spacing: 0) {
                        ForEach(0..<8, id: \.self) { col in
                            let sq = square(row: row, col: col)
                            squareView(sq, cell: cell, row: row, col: col)
                                .frame(width: cell, height: cell)
                                .contentShape(Rectangle())
                                .onTapGesture { onTap(sq) }
                        }
                    }
                }
            }
            .frame(width: side, height: side)
            .overlay(arrowLayer(cell: cell).allowsHitTesting(false))
            .overlay(Rectangle().stroke(Color.ocRule, lineWidth: 1))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .aspectRatio(1, contentMode: .fit)
    }

    private func square(row: Int, col: Int) -> Int {
        flipped ? (row * 8 + (7 - col)) : ((7 - row) * 8 + col)
    }

    /// The centre of a square in the board's coordinates.
    private func centre(_ sq: Int, cell: CGFloat) -> CGPoint {
        let file = sq % 8
        let rank = sq / 8
        let col = flipped ? 7 - file : file
        let row = flipped ? rank : 7 - rank
        return CGPoint(x: (CGFloat(col) + 0.5) * cell, y: (CGFloat(row) + 0.5) * cell)
    }

    private var allArrows: [BoardArrow] {
        var out: [BoardArrow] = []
        if let hint {
            out.append(BoardArrow(from: hint.from, to: hint.to, color: .ocAction))
        }
        out.append(contentsOf: marks.arrows)
        return out
    }

    @ViewBuilder
    private func arrowLayer(cell: CGFloat) -> some View {
        let arrows = allArrows
        ZStack {
            ForEach(0..<arrows.count, id: \.self) { i in
                let a = arrows[i]
                ArrowShape(from: centre(a.from, cell: cell), to: centre(a.to, cell: cell), width: max(4, cell * 0.16))
                    .fill(a.color.opacity(0.85))
                    .overlay(
                        ArrowShape(from: centre(a.from, cell: cell), to: centre(a.to, cell: cell), width: max(4, cell * 0.16))
                            .stroke(Color.black.opacity(0.35), lineWidth: 1)
                    )
            }
        }
        .frame(width: cell * 8, height: cell * 8)
    }

    @ViewBuilder
    private func squareView(_ sq: Int, cell: CGFloat, row: Int, col: Int) -> some View {
        let light = Square.isLight(sq)
        let isLast = lastMove.map { $0.from == sq || $0.to == sq } ?? false
        let isHint = hint.map { $0.from == sq || $0.to == sq } ?? false
        ZStack {
            Rectangle().fill(baseColor(light: light, last: isLast))
            if check == sq {
                Rectangle().fill(Color.ocDanger.opacity(0.55))
            }
            if let tint = marks.tints[sq] {
                Rectangle().fill(tint.opacity(0.55))
            }
            if selected == sq {
                Rectangle().fill(Color.ocAction.opacity(0.35))
            }
            if unsure.contains(sq) {
                Rectangle().stroke(Color.ocAmber, style: StrokeStyle(lineWidth: 2, dash: [4, 3]))
                    .padding(1)
            }
            if isHint {
                Rectangle().stroke(Color.ocAction, lineWidth: max(2, cell * 0.06))
                    .padding(max(1, cell * 0.03))
            }
            if let p = placement[sq] {
                PieceGlyph(piece: p, size: cell)
                    .modifier(ShakeEffect(shakes: shakeSquare == sq ? CGFloat(shakes) : 0))
                    .animation(.linear(duration: 0.3), value: shakes)
            } else if let ghost = marks.ghosts[sq] {
                ZStack {
                    Rectangle().stroke(Color.ocDanger, lineWidth: max(2, cell * 0.05))
                        .padding(max(1, cell * 0.03))
                    PieceGlyph(piece: ghost, size: cell).opacity(0.4)
                }
            }
            if marks.removals.contains(sq) {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: max(10, cell * 0.3), weight: .bold))
                    .foregroundStyle(Color.white, Color.ocDanger)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                    .padding(2)
            }
            if targets.contains(sq) {
                if placement[sq] == nil {
                    Circle().fill(Color.ocBSeg.opacity(0.3)).frame(width: cell * 0.28, height: cell * 0.28)
                } else {
                    Circle().stroke(Color.ocBSeg.opacity(0.4), lineWidth: cell * 0.07).padding(cell * 0.04)
                }
            }
            coordinates(sq, row: row, col: col, light: light, cell: cell)
        }
    }

    private func baseColor(light: Bool, last: Bool) -> Color {
        if last { return light ? .ocLastLight : .ocLastDark }
        return light ? .ocSqLight : .ocSqDark
    }

    @ViewBuilder
    private func coordinates(_ sq: Int, row: Int, col: Int, light: Bool, cell: CGFloat) -> some View {
        let color: Color = light ? .ocSqDark : .ocSqLight
        let font = OCFont.mono(max(8, cell * 0.17), weight: .semibold)
        ZStack {
            if col == 0 {
                Text("\(sq / 8 + 1)").font(font).foregroundStyle(color)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .padding(2)
            }
            if row == 7 {
                Text(String(Array("abcdefgh")[sq % 8])).font(font).foregroundStyle(color)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                    .padding(2)
            }
        }
        .allowsHitTesting(false)
    }
}

/// Choose the piece a pawn promotes to.
@MainActor
struct PromotionPicker: View {
    let white: Bool
    let onPick: (Character) -> Void
    let onCancel: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            Text("Promote to").font(OCFont.text(15, weight: .semibold))
            HStack(spacing: 10) {
                ForEach(["q", "r", "b", "n"], id: \.self) { k in
                    let letter: Character = white ? Character(k.uppercased()) : Character(k)
                    Button {
                        onPick(Character(k))
                    } label: {
                        PieceGlyph(piece: Piece(letter: letter), size: 52)
                            .frame(width: 56, height: 56)
                            .background(Color.ocSqLight)
                            .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
                    }
                    .buttonStyle(.plain)
                }
            }
            Button("Cancel", action: onCancel)
                .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
        }
        .padding(16)
        .background(Color.ocPanel)
        .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner))
        .overlay(RoundedRectangle(cornerRadius: OCMetrics.corner).stroke(Color.ocRule, lineWidth: 1))
        .foregroundStyle(Color.ocInk)
    }
}

/// The win/draw/loss bar, White's share on the left.
@MainActor
struct WDLBar: View {
    let white: Double
    let draw: Double
    let black: Double

    var body: some View {
        let total = max(0.0001, white + draw + black)
        GeometryReader { geo in
            HStack(spacing: 0) {
                Rectangle().fill(Color.ocWSeg).frame(width: geo.size.width * CGFloat(white / total))
                Rectangle().fill(Color.ocDSeg).frame(width: geo.size.width * CGFloat(draw / total))
                Rectangle().fill(Color.ocBSeg)
            }
        }
        .frame(height: 10)
        .overlay(Rectangle().stroke(Color.ocRule, lineWidth: 1))
        .accessibilityLabel(String(format: "White %.0f%%, draw %.0f%%, Black %.0f%%",
                                   white / total * 100, draw / total * 100, black / total * 100))
    }
}
