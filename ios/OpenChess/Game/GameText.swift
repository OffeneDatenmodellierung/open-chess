import Foundation

// Words for what happens in a game: results with their reasons, a move
// spoken aloud, a verdict from a score. Pure Swift (no core), so the
// logic tests cover it.

extension Placement {
    /// The square of the white or black king, if there is one.
    func kingSquare(white: Bool) -> Int? {
        let king: Character = white ? "K" : "k"
        return squares.firstIndex { $0?.letter == king }
    }

    /// Neither side can ever mate: kings only, a single knight or bishop,
    /// or bishops all on one colour of square.
    var insufficientMaterial: Bool {
        var minors: [(kind: Character, square: Int)] = []
        for (sq, piece) in squares.enumerated() {
            guard let piece else { continue }
            switch piece.kind {
            case "k":
                continue
            case "b", "n":
                minors.append((kind: piece.kind, square: sq))
            default:
                return false
            }
        }
        if minors.count <= 1 { return true }
        if minors.allSatisfy({ $0.kind == "b" }) {
            let colours = Set(minors.map { Square.isLight($0.square) })
            return colours.count == 1
        }
        return false
    }
}

/// A finished game's headline: "Checkmate · Black wins", "Draw · threefold
/// repetition", and the score.
struct ResultSummary: Equatable {
    var title: String
    var score: String
}

enum GameText {
    /// Why a game the core calls a "draw" (on the board) was drawn.
    static func drawReason(fen: String) -> String {
        let fields = fen.split(separator: " ")
        if fields.count >= 5, let half = Int(fields[4]), half >= 100 {
            return "fifty-move rule"
        }
        if Placement(fen: fen).insufficientMaterial {
            return "insufficient material"
        }
        return "threefold repetition"
    }

    /// `agreed`: the players agreed the draw (the core reports it as "draw" too).
    static func result(status: String, winner: String?, fen: String, agreed: Bool = false) -> ResultSummary {
        let score: String
        switch winner ?? "" {
        case "white": score = "1–0"
        case "black": score = "0–1"
        default: score = status == "aborted" ? "" : "½–½"
        }
        let title: String
        switch status {
        case "draw":
            title = agreed ? "Draw · agreed" : "Draw · " + drawReason(fen: fen)
        case "stalemate":
            title = "Draw · stalemate"
        case "outoftime", "timeout":
            title = winner == nil ? "Time out · Draw (no mating material)" : statusText(status, winner: winner)
        default:
            title = statusText(status, winner: winner)
        }
        return ResultSummary(title: title, score: score)
    }

    static func pieceName(_ letter: Character?) -> String {
        switch letter {
        case "K": return "King"
        case "Q": return "Queen"
        case "R": return "Rook"
        case "B": return "Bishop"
        case "N": return "Knight"
        default: return "Pawn"
        }
    }

    /// A SAN move in plain English: "Nf3+" → "Knight to f3, check",
    /// "exd5" → "e pawn takes d5", "O-O" → "Castles kingside".
    static func spoken(san: String) -> String {
        var s = san.trimmingCharacters(in: .whitespacesAndNewlines)
        while let last = s.last, last == "!" || last == "?" {
            s.removeLast()
        }
        var suffix = ""
        if s.hasSuffix("#") {
            suffix = ", checkmate"
            s.removeLast()
        } else if s.hasSuffix("+") {
            suffix = ", check"
            s.removeLast()
        }
        if s.hasPrefix("O-O-O") || s.hasPrefix("0-0-0") {
            return "Castles queenside" + suffix
        }
        if s.hasPrefix("O-O") || s.hasPrefix("0-0") {
            return "Castles kingside" + suffix
        }
        var promotion = ""
        if let eq = s.firstIndex(of: "=") {
            let after = s[s.index(after: eq)...]
            promotion = ", promotes to " + pieceName(after.first).lowercased()
            s = String(s[..<eq])
        } else if s.count >= 3, let last = s.last, "QRBN".contains(last),
                  let prev = s.dropLast().last, prev.isNumber {
            promotion = ", promotes to " + pieceName(last).lowercased()
            s.removeLast()
        }
        let capture = s.contains("x")
        s = s.replacingOccurrences(of: "x", with: "")
        var piece = "Pawn"
        if let first = s.first, "KQRBN".contains(first) {
            piece = pieceName(first)
            s.removeFirst()
        }
        guard s.count >= 2 else { return san }
        let dest = String(s.suffix(2))
        let from = String(s.dropLast(2))
        var out: String
        if piece == "Pawn" {
            out = from.isEmpty ? "Pawn" : "\(from) pawn"
        } else {
            out = from.isEmpty ? piece : "\(piece) from \(from)"
        }
        out += capture ? " takes \(dest)" : " to \(dest)"
        return out + promotion + suffix
    }

    /// A one-line verdict from a score from White's side (centipawns, or
    /// mate in N: positive for White).
    static func verdict(cp: Int?, mate: Int?) -> String {
        if let m = mate {
            if m > 0 { return "White mates in \(m)" }
            if m < 0 { return "Black mates in \(-m)" }
            return "Checkmate"
        }
        let v = cp ?? 0
        if v >= 300 { return "White is winning" }
        if v >= 80 { return "White is better" }
        if v >= 30 { return "White is slightly better" }
        if v > -30 { return "About level" }
        if v > -80 { return "Black is slightly better" }
        if v > -300 { return "Black is better" }
        return "Black is winning"
    }

    static func verdict(_ score: EngineScore?) -> String {
        guard let score else { return "About level" }
        if score.kind == "mate" {
            return verdict(cp: nil, mate: score.moves ?? 0)
        }
        return verdict(cp: score.value ?? 0, mate: nil)
    }

    /// What to put right on a real board, for one correction: the piece
    /// as a FEN letter, where it wrongly stands (nil: missing) and where it
    /// belongs (nil: it shouldn't be on the board).
    static func correction(piece: String, from: String?, to: String?) -> String {
        let letter = piece.first
        let colour = (letter?.isUppercase ?? true) ? "white" : "black"
        let name = pieceName(letter.map { Character($0.uppercased()) })
        if let from, let to {
            return "\(name) on \(from) should go back to \(to)"
        }
        if let from {
            return "Remove the \(colour) \(name.lowercased()) from \(from)"
        }
        if let to {
            return "Put the \(colour) \(name.lowercased()) back on \(to)"
        }
        return ""
    }

    /// The FEN with the other side to move (no en passant square), for
    /// "what does the other side threaten".
    static func otherSideToMove(fen: String) -> String? {
        var fields = fen.split(separator: " ").map(String.init)
        guard fields.count >= 2 else { return nil }
        fields[1] = fields[1] == "w" ? "b" : "w"
        if fields.count >= 4 { fields[3] = "-" }
        return fields.joined(separator: " ")
    }
}
