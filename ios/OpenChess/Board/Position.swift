import Foundation

/// A piece as a FEN letter: "P N B R Q K" white, "p n b r q k" black.
struct Piece: Hashable {
    let letter: Character

    var isWhite: Bool { letter.isUppercase }
    var kind: Character { Character(letter.lowercased()) }

    /// The Unicode glyph, text presentation (U+FE0E) so it never turns
    /// into an emoji. The solid glyphs are used for both colours and
    /// coloured in the view.
    var glyph: String {
        let base: String
        switch kind {
        case "k": base = "\u{265A}"
        case "q": base = "\u{265B}"
        case "r": base = "\u{265C}"
        case "b": base = "\u{265D}"
        case "n": base = "\u{265E}"
        default: base = "\u{265F}"
        }
        return base + "\u{FE0E}"
    }

    static let palette: [Piece?] = [
        Piece(letter: "K"), Piece(letter: "Q"), Piece(letter: "R"), Piece(letter: "B"), Piece(letter: "N"), Piece(letter: "P"),
        Piece(letter: "k"), Piece(letter: "q"), Piece(letter: "r"), Piece(letter: "b"), Piece(letter: "n"), Piece(letter: "p"),
        nil,
    ]
}

/// Squares are 0...63 with a1 = 0, b1 = 1, ... h8 = 63 (as the core counts them).
enum Square {
    static func name(_ sq: Int) -> String {
        let files = Array("abcdefgh")
        return "\(files[sq % 8])\(sq / 8 + 1)"
    }

    static func index(_ name: String) -> Int? {
        let chars = Array(name)
        guard chars.count >= 2,
              let f = chars[0].asciiValue, let r = chars[1].asciiValue,
              f >= 97, f <= 104, r >= 49, r <= 56 else { return nil }
        return Int(r - 49) * 8 + Int(f - 97)
    }

    static func isLight(_ sq: Int) -> Bool {
        (sq / 8 + sq % 8) % 2 == 1
    }
}

/// The piece placement of a position (FEN's first field) as 64 squares.
struct Placement: Equatable {
    var squares: [Piece?] = Array(repeating: nil, count: 64)

    init() {}

    init(fen: String) {
        let field = fen.split(separator: " ").first.map(String.init) ?? ""
        let ranks = field.split(separator: "/", omittingEmptySubsequences: false)
        guard ranks.count == 8 else { return }
        for (i, rank) in ranks.enumerated() {
            let r = 7 - i
            var f = 0
            for ch in rank {
                if let n = ch.wholeNumberValue {
                    f += n
                } else if f < 8 {
                    squares[r * 8 + f] = Piece(letter: ch)
                    f += 1
                }
            }
        }
    }

    subscript(sq: Int) -> Piece? {
        get { squares[sq] }
        set { squares[sq] = newValue }
    }

    /// FEN's first field.
    var fenField: String {
        var out: [String] = []
        for r in stride(from: 7, through: 0, by: -1) {
            var row = ""
            var empty = 0
            for f in 0..<8 {
                if let p = squares[r * 8 + f] {
                    if empty > 0 { row += String(empty); empty = 0 }
                    row.append(p.letter)
                } else {
                    empty += 1
                }
            }
            if empty > 0 { row += String(empty) }
            out.append(row)
        }
        return out.joined(separator: "/")
    }

    static let start = Placement(fen: "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR")
}

/// A move's squares from UCI ("e2e4", "e7e8q").
struct UCIMove {
    let from: Int
    let to: Int
    let promotion: Character?

    init?(_ uci: String) {
        let chars = Array(uci)
        guard chars.count >= 4,
              let f = Square.index(String(chars[0...1])),
              let t = Square.index(String(chars[2...3])) else { return nil }
        from = f
        to = t
        promotion = chars.count > 4 ? chars[4] : nil
    }
}
