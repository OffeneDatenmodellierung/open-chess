import OpenChessCore
import SwiftUI

/// A real board that doesn't match the game: the board as it actually is,
/// with what to put right drawn over it (the core's `boardCorrections`).
struct BoardCorrections: Equatable {
    /// The board as it is.
    var seen: Placement
    var marks: BoardMarks
    /// "Knight on e5 should go back to f3", one per correction.
    var sentences: [String]
    /// Squares to light on a board that can.
    var squares: [String]

    /// Nil when there's nothing to put right (or a placement can't be read).
    static func make(expected: String, seen: String) -> BoardCorrections? {
        let expectedField = expected.split(separator: " ").first.map(String.init) ?? expected
        let seenField = seen.split(separator: " ").first.map(String.init) ?? seen
        let list = boardCorrections(expected: expectedField, seen: seenField)
        guard !list.isEmpty else { return nil }
        var marks = BoardMarks()
        var sentences: [String] = []
        var squares: [String] = []
        for c in list {
            let piece: Piece? = c.piece.first.map { Piece(letter: $0) }
            let from: Int? = c.from.flatMap { Square.index($0) }
            let to: Int? = c.to.flatMap { Square.index($0) }
            if let from, let to {
                marks.tints[from] = .ocDanger
                marks.arrows.append(BoardArrow(from: from, to: to, color: .ocDanger))
            } else if let from {
                marks.tints[from] = .ocDanger
                marks.removals.insert(from)
            } else if let to, let piece {
                marks.ghosts[to] = piece
            }
            let sentence = GameText.correction(piece: c.piece, from: c.from, to: c.to)
            if !sentence.isEmpty { sentences.append(sentence) }
            if let f = c.from { squares.append(f) }
            if let t = c.to { squares.append(t) }
        }
        return BoardCorrections(seen: Placement(fen: seenField), marks: marks, sentences: sentences, squares: squares)
    }

    var summary: String {
        sentences.joined(separator: ". ")
    }
}
