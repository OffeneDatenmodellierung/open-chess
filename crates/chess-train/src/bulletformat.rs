//! The `bulletformat` chess record: 32 bytes per position, the binary
//! format the [bullet] trainer reads directly (`ChessBoard` in its
//! `bulletformat` crate, MIT). Writing it straight from self-play skips
//! the text-to-binary conversion and stores a billion positions in 32 GB.
//!
//! [bullet]: https://github.com/jw1912/bullet
//!
//! Layout, little-endian, from the side to move's point of view (the
//! board is mirrored when Black is to move, so "us" is always the side
//! whose pawns move up the board):
//!
//! | bytes | | |
//! |---|---|---|
//! | 0–7 | `occ` | bitboard of every piece |
//! | 8–23 | `pcs` | one nibble per set bit of `occ` in square order: piece (0 pawn … 5 king) + 8 if the opponent's; even bits in the low nibble |
//! | 24–25 | `score` | centipawns, `i16`, for the side to move |
//! | 26 | `result` | 2 win, 1 draw, 0 loss, for the side to move |
//! | 27 | `ksq` | our king's square |
//! | 28 | `opp_ksq` | their king's square, mirrored (`^ 56`) |
//! | 29–31 | `extra` | zero |
//!
//! The text form `FEN | score | result` (White's point of view) is what
//! [`crate::datagen`] wrote before and is still accepted by bullet's
//! converter; this is the same data without the conversion step.

use cozy_chess::{Board, Color, Piece, Square};

/// One record.
pub const SIZE: usize = 32;

/// Encode `board`, with `white_cp` (White's point of view, centipawns) and
/// `white_result` (1.0 White won, 0.5 draw, 0.0 Black won).
///
/// The score is clamped to `i16`; callers should already have replaced
/// mate scores with a large centipawn value.
pub fn encode(board: &Board, white_cp: i32, white_result: f32) -> [u8; SIZE] {
    let stm = board.side_to_move();
    let flip = |sq: Square| if stm == Color::White { sq } else { sq.flip_rank() };
    let (score, result) =
        if stm == Color::White { (white_cp, white_result) } else { (-white_cp, 1.0 - white_result) };
    let mut occ = 0u64;
    let mut pieces: Vec<(usize, u8)> = Vec::with_capacity(32);
    for color in Color::ALL {
        for piece in Piece::ALL {
            for sq in board.colored_pieces(color, piece) {
                let s = flip(sq) as usize;
                occ |= 1 << s;
                let colour_bit = u8::from(color != stm) << 3;
                pieces.push((s, piece as u8 | colour_bit));
            }
        }
    }
    pieces.sort_unstable();
    let mut out = [0u8; SIZE];
    out[..8].copy_from_slice(&occ.to_le_bytes());
    for (i, &(_, nibble)) in pieces.iter().enumerate() {
        out[8 + i / 2] |= nibble << (4 * (i % 2));
    }
    let score = score.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    out[24..26].copy_from_slice(&score.to_le_bytes());
    out[26] = (result * 2.0).round().clamp(0.0, 2.0) as u8;
    out[27] = flip(board.king(stm)) as u8;
    out[28] = (flip(board.king(!stm)) as u8) ^ 0b11_1000;
    out
}

/// The text line for the same record, as `examples/datagen` wrote it.
pub fn text_line(board: &Board, white_cp: i32, white_result: f32) -> String {
    format!("{board} | {white_cp} | {white_result:.1}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_position_from_white() {
        let b = Board::default();
        let r = encode(&b, 12, 0.5);
        assert_eq!(u64::from_le_bytes(r[..8].try_into().unwrap()), 0xffff_0000_0000_ffff);
        // a1: our rook (3); b1 our knight (1) in the high nibble.
        assert_eq!(r[8], 3 | (1 << 4));
        // a8: their rook, bit 3 set.
        assert_eq!(r[8 + 12] & 0x0f, 3 | 8);
        assert_eq!(i16::from_le_bytes([r[24], r[25]]), 12);
        assert_eq!(r[26], 1);
        assert_eq!(r[27], Square::E1 as u8);
        assert_eq!(r[28], Square::E8 as u8 ^ 0b11_1000);
        assert_eq!(&r[29..], &[0, 0, 0]);
    }

    #[test]
    fn black_to_move_is_mirrored_and_negated() {
        let b = Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1", false).unwrap();
        let r = encode(&b, 30, 1.0);
        // Mirrored: our (Black's) pieces sit on ranks 1–2, the white pawn on e4 becomes e5.
        let occ = u64::from_le_bytes(r[..8].try_into().unwrap());
        assert_ne!(occ & (1 << (Square::E5 as u64)), 0);
        assert_eq!(i16::from_le_bytes([r[24], r[25]]), -30);
        assert_eq!(r[26], 0, "White's win is our loss");
        assert_eq!(r[27], Square::E1 as u8);
    }

    #[test]
    fn score_is_clamped() {
        let r = encode(&Board::default(), 40_000, 1.0);
        assert_eq!(i16::from_le_bytes([r[24], r[25]]), i16::MAX);
    }
}
