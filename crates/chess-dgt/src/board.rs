//! What is on the board: pieces by square, and which way round the board is.

use std::fmt;

/// The piece placement of the standard starting position (FEN field one).
pub const STARTING_PLACEMENT: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PieceKind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Piece {
    pub kind: PieceKind,
    pub white: bool,
}

impl Piece {
    /// A DGT piece code: 1–6 white pawn, rook, knight, bishop, king,
    /// queen; 7–12 the same in black. 0 is an empty square; 13–15 are the
    /// draughts and special pieces, which chess has no use for.
    pub fn from_code(code: u8) -> Option<Piece> {
        use PieceKind::{Bishop, King, Knight, Pawn, Queen, Rook};
        let (kind, white) = match code {
            1 => (Pawn, true),
            2 => (Rook, true),
            3 => (Knight, true),
            4 => (Bishop, true),
            5 => (King, true),
            6 => (Queen, true),
            7 => (Pawn, false),
            8 => (Rook, false),
            9 => (Knight, false),
            10 => (Bishop, false),
            11 => (King, false),
            12 => (Queen, false),
            _ => return None,
        };
        Some(Piece { kind, white })
    }

    /// The FEN letter: upper case for White.
    pub fn letter(self) -> char {
        let c = match self.kind {
            PieceKind::Pawn => 'p',
            PieceKind::Knight => 'n',
            PieceKind::Bishop => 'b',
            PieceKind::Rook => 'r',
            PieceKind::Queen => 'q',
            PieceKind::King => 'k',
        };
        if self.white { c.to_ascii_uppercase() } else { c }
    }

    pub fn from_letter(c: char) -> Option<Piece> {
        let kind = match c.to_ascii_lowercase() {
            'p' => PieceKind::Pawn,
            'n' => PieceKind::Knight,
            'b' => PieceKind::Bishop,
            'r' => PieceKind::Rook,
            'q' => PieceKind::Queen,
            'k' => PieceKind::King,
            _ => return None,
        };
        Some(Piece { kind, white: c.is_ascii_uppercase() })
    }
}

/// Which way round the board sits. The board reports squares from its own
/// point of view; a board turned through 180° (White's pieces set up on
/// the side DGT marks as Black's) reports every square mirrored through
/// the centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal,
    Rotated,
}

/// The pieces on the 64 squares, indexed a1 = 0, b1 = 1, … h8 = 63.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Placement(pub [Option<Piece>; 64]);

impl Placement {
    pub const EMPTY: Placement = Placement([None; 64]);

    /// From a board dump's 64 piece codes, in the board's own order (a8,
    /// b8 … h8, a7 … h1).
    pub fn from_dump(codes: &[u8; 64]) -> Placement {
        let mut p = Placement::EMPTY;
        for (i, &code) in codes.iter().enumerate() {
            p.0[dump_index_to_square(i)] = Piece::from_code(code);
        }
        p
    }

    /// From the first field of a FEN.
    pub fn from_fen(placement: &str) -> Option<Placement> {
        let field = placement.split_whitespace().next()?;
        let ranks: Vec<&str> = field.split('/').collect();
        if ranks.len() != 8 {
            return None;
        }
        let mut p = Placement::EMPTY;
        for (r, rank) in ranks.iter().enumerate() {
            let rank_index = 7 - r;
            let mut file = 0usize;
            for c in rank.chars() {
                if let Some(d) = c.to_digit(10) {
                    file += d as usize;
                } else {
                    let piece = Piece::from_letter(c)?;
                    if file >= 8 {
                        return None;
                    }
                    p.0[rank_index * 8 + file] = Some(piece);
                    file += 1;
                }
                if file > 8 {
                    return None;
                }
            }
            if file != 8 {
                return None;
            }
        }
        Some(p)
    }

    /// The FEN placement field.
    pub fn fen(&self) -> String {
        let mut out = String::with_capacity(71);
        for rank in (0..8).rev() {
            let mut empty = 0;
            for file in 0..8 {
                match self.0[rank * 8 + file] {
                    Some(piece) => {
                        if empty > 0 {
                            out.push(char::from(b'0' + empty));
                            empty = 0;
                        }
                        out.push(piece.letter());
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                out.push(char::from(b'0' + empty));
            }
            if rank > 0 {
                out.push('/');
            }
        }
        out
    }

    /// The same board turned through 180°.
    #[must_use]
    pub fn rotated(&self) -> Placement {
        let mut p = Placement::EMPTY;
        for (s, piece) in self.0.iter().enumerate() {
            p.0[63 - s] = *piece;
        }
        p
    }

    /// As seen the given way round.
    #[must_use]
    pub fn oriented(&self, o: Orientation) -> Placement {
        match o {
            Orientation::Normal => *self,
            Orientation::Rotated => self.rotated(),
        }
    }

    pub fn piece_count(&self) -> usize {
        self.0.iter().filter(|p| p.is_some()).count()
    }
}

impl fmt::Debug for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Placement({})", self.fen())
    }
}

/// Byte `i` of a board dump (a8 first, h1 last) as a square index (a1 = 0).
pub(crate) fn dump_index_to_square(i: usize) -> usize {
    (7 - i / 8) * 8 + i % 8
}

/// The board's own field number (0 = a8 … 63 = h1) as a square index.
pub(crate) fn field_to_square(field: u8) -> Option<usize> {
    (field < 64).then(|| dump_index_to_square(usize::from(field)))
}

/// A square index (a1 = 0) as the board's own field number (a8 = 0).
pub(crate) fn square_to_field(square: usize) -> u8 {
    ((7 - square / 8) * 8 + square % 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fen_round_trip() {
        let p = Placement::from_fen(STARTING_PLACEMENT).unwrap();
        assert_eq!(p.fen(), STARTING_PLACEMENT);
        assert_eq!(p.piece_count(), 32);
        let odd = "8/8/3k4/8/2P5/8/8/4K2R";
        assert_eq!(Placement::from_fen(odd).unwrap().fen(), odd);
        assert!(Placement::from_fen("8/8/8").is_none());
        assert!(Placement::from_fen("9/8/8/8/8/8/8/8").is_none());
        assert!(Placement::from_fen("x7/8/8/8/8/8/8/8").is_none());
    }

    #[test]
    fn dump_order_is_a8_first() {
        // Board order: a8 b8 … h8, a7 … h1. Put a white king on the first
        // byte (a8) and a black queen on the last (h1).
        let mut codes = [0u8; 64];
        codes[0] = 5;
        codes[63] = 12;
        let p = Placement::from_dump(&codes);
        assert_eq!(p.fen(), "K7/8/8/8/8/8/8/7q");
        assert_eq!(square_to_field(56), 0, "a8 is field 0");
        assert_eq!(field_to_square(63), Some(7), "field 63 is h1");
        assert_eq!(field_to_square(64), None);
    }

    #[test]
    fn rotation_mirrors_through_the_centre() {
        let p = Placement::from_fen(STARTING_PLACEMENT).unwrap();
        let r = p.rotated();
        assert_eq!(r.fen(), "RNBKQBNR/PPPPPPPP/8/8/8/8/pppppppp/rnbkqbnr");
        assert_eq!(r.rotated(), p);
        assert_eq!(p.oriented(Orientation::Rotated), r);
    }
}
