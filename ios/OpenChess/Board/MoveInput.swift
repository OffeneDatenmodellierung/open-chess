import Foundation

/// Tap-tap move input: tap a piece, then where it goes. Pure Swift, so the
/// logic tests cover it.
struct MoveInput {
    enum Outcome: Equatable {
        /// Nothing to do (a piece selected, or the selection cleared).
        case none
        /// A move to play (UCI).
        case move(String)
        /// A pawn reaching the last rank: ask which piece.
        case promote(from: Int, to: Int)
        /// Not a move: shake the piece on this square.
        case illegal(Int)
    }

    var selected: Int?
    var targets: Set<Int> = []

    mutating func clear() {
        selected = nil
        targets = []
    }

    private func moves(from sq: Int, legal: [String]) -> [UCIMove] {
        legal.compactMap { UCIMove($0) }.filter { $0.from == sq }
    }

    mutating func tap(_ sq: Int, legal: [String], placement: Placement, whiteToMove: Bool) -> Outcome {
        if let from = selected {
            if from == sq {
                clear()
                return .none
            }
            let candidates = legal.filter { m in
                guard let u = UCIMove(m) else { return false }
                return u.from == from && u.to == sq
            }
            if candidates.count > 1 {
                return .promote(from: from, to: sq)
            }
            if let m = candidates.first {
                clear()
                return .move(m)
            }
            let other = moves(from: sq, legal: legal)
            if !other.isEmpty {
                selected = sq
                targets = Set(other.map { $0.to })
                return .none
            }
            clear()
            return .illegal(from)
        }
        let mine = moves(from: sq, legal: legal)
        if !mine.isEmpty {
            selected = sq
            targets = Set(mine.map { $0.to })
            return .none
        }
        if let p = placement[sq], p.isWhite == whiteToMove {
            // The side to move's piece, with no legal move (pinned, blocked).
            return .illegal(sq)
        }
        return .none
    }
}
