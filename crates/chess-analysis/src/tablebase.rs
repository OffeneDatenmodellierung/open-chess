//! Exact endgame tables for positions with three pieces (two kings and
//! one other piece), built on demand by retrograde analysis.
//!
//! A tablebase is a dictionary of every position of a given material
//! with its exact result and distance to mate. Three-piece tables are
//! small enough to build in a moment, even in a phone browser, so the
//! engine plays king-and-pawn, king-and-rook and king-and-queen endings
//! perfectly offline. Larger tables (up to seven pieces) are tens of
//! terabytes, so the app asks the Lichess tablebase server for those when
//! it's online.
//!
//! Each table covers every placement of white king, black king and the
//! third piece with either side to move. It is solved by passes: pass `n`
//! marks positions that win in `n` plies (some move reaches a position
//! lost in `n − 1`) and positions lost in `n` plies (every move reaches a
//! position won in at most `n − 1`). What is left at the end is drawn.
//! Promotions look into the table for the new piece, captures leave bare
//! kings (a draw).

use std::collections::HashMap;

use cozy_chess::{Board, BoardBuilder, Color, Piece, Square};
use serde::Serialize;

use crate::util::legal;

/// The exact result of a position for the side to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Probe {
    /// Wins, mating in `plies` half-moves with best play.
    Win {
        plies: u16,
    },
    Draw,
    /// Loses, mated in `plies` half-moves with best defence (0: mated now).
    Loss {
        plies: u16,
    },
}

impl Probe {
    /// Win / draw / loss as 1 / 0 / -1.
    pub fn wdl(self) -> i8 {
        match self {
            Probe::Win { .. } => 1,
            Probe::Draw => 0,
            Probe::Loss { .. } => -1,
        }
    }
}

const UNKNOWN: i16 = i16::MIN;
const INVALID: i16 = i16::MIN + 1;
const DRAW: i16 = 0;

fn enc_win(plies: u16) -> i16 {
    plies as i16
}

fn enc_loss(plies: u16) -> i16 {
    -(plies as i16) - 1
}

fn decode(v: i16) -> Option<Probe> {
    match v {
        UNKNOWN | INVALID => None,
        0 => Some(Probe::Draw),
        v if v > 0 => Some(Probe::Win { plies: v as u16 }),
        v => Some(Probe::Loss { plies: (-v - 1) as u16 }),
    }
}

/// One solved table: the third piece belongs to White.
struct Table {
    values: Vec<i16>,
}

fn index(wk: Square, bk: Square, x: Square, stm: Color) -> usize {
    (((wk as usize) * 64 + bk as usize) * 64 + x as usize) * 2 + stm as usize
}

const SIZE: usize = 64 * 64 * 64 * 2;

/// Built tables, kept for reuse. Building one takes well under a second
/// natively and a second or two in wasm, the first time it is needed.
#[derive(Default)]
pub struct Tablebases {
    tables: HashMap<Piece, Table>,
}

/// The board with colours swapped and ranks mirrored, so the side with
/// the extra piece is always White.
fn flipped(board: &Board) -> Option<Board> {
    let mut b = BoardBuilder::empty();
    for sq in board.occupied() {
        let p = board.piece_on(sq)?;
        let c = board.color_on(sq)?;
        *b.square_mut(sq.flip_rank()) = Some((p, !c));
    }
    b.side_to_move = !board.side_to_move();
    b.fullmove_number = 1;
    b.build().ok()
}

impl Tablebases {
    /// The exact result for the side to move, for positions with exactly
    /// three pieces; `None` otherwise (or with castling rights left).
    pub fn probe(&mut self, board: &Board) -> Option<Probe> {
        if board.occupied().len() != 3 {
            return None;
        }
        let strong_white = board.colors(Color::White).len() == 2;
        let b = if strong_white { board.clone() } else { flipped(board)? };
        let x = (b.colors(Color::White) & !b.pieces(Piece::King)).next_square()?;
        let piece = b.piece_on(x)?;
        let idx = index(b.king(Color::White), b.king(Color::Black), x, b.side_to_move());
        decode(self.table(piece).values[idx])
    }

    fn table(&mut self, piece: Piece) -> &Table {
        if !self.tables.contains_key(&piece) && matches!(piece, Piece::Bishop | Piece::Knight) {
            // A lone minor piece can never mate: every position is a draw.
            self.tables.insert(piece, Table { values: vec![DRAW; SIZE] });
        }
        if !self.tables.contains_key(&piece) {
            if piece == Piece::Pawn {
                // Pawns promote: solve the tables they can promote into first.
                for p in [Piece::Queen, Piece::Rook, Piece::Bishop, Piece::Knight] {
                    self.table(p);
                }
            }
            let t = self.build(piece);
            self.tables.insert(piece, t);
        }
        &self.tables[&piece]
    }

    fn build(&self, piece: Piece) -> Table {
        let mut values = vec![INVALID; SIZE];
        // Children within the table, as a flat list with offsets, plus what
        // is known about the children that leave it.
        let mut offsets: Vec<u32> = vec![0; SIZE + 1];
        let mut children: Vec<u32> = Vec::with_capacity(SIZE * 6);
        // Per position: shortest external child loss, longest external
        // child win, and whether some external child is a draw.
        let mut ext_loss: Vec<i16> = vec![i16::MAX; SIZE];
        let mut ext_win: Vec<i16> = vec![-1; SIZE];
        let mut ext_draw: Vec<bool> = vec![false; SIZE];
        let mut builder = BoardBuilder::empty();
        builder.fullmove_number = 1;
        for wk in Square::ALL {
            for bk in Square::ALL {
                for x in Square::ALL {
                    for stm in Color::ALL {
                        let i = index(wk, bk, x, stm);
                        offsets[i] = children.len() as u32;
                        if wk == bk || wk == x || bk == x {
                            continue;
                        }
                        if piece == Piece::Pawn
                            && matches!(x.rank(), cozy_chess::Rank::First | cozy_chess::Rank::Eighth)
                        {
                            continue;
                        }
                        builder.board = [None; 64];
                        builder.board[wk as usize] = Some((Piece::King, Color::White));
                        builder.board[bk as usize] = Some((Piece::King, Color::Black));
                        builder.board[x as usize] = Some((piece, Color::White));
                        builder.side_to_move = stm;
                        let Ok(board) = builder.build() else { continue };
                        let moves = legal(&board);
                        if moves.is_empty() {
                            values[i] = if board.checkers().is_empty() { DRAW } else { enc_loss(0) };
                            continue;
                        }
                        values[i] = UNKNOWN;
                        for mv in moves {
                            let mut c = board.clone();
                            c.play_unchecked(mv);
                            if c.occupied().len() == 2 {
                                ext_draw[i] = true;
                                continue;
                            }
                            let cx =
                                (c.colors(Color::White) & !c.pieces(Piece::King)).next_square().unwrap_or(x);
                            let cp = c.piece_on(cx).unwrap_or(piece);
                            let ci = index(c.king(Color::White), c.king(Color::Black), cx, c.side_to_move());
                            if cp == piece {
                                children.push(ci as u32);
                            } else {
                                // A promotion: the result comes from that table.
                                let v = self.tables.get(&cp).map_or(DRAW, |t| t.values[ci]);
                                match decode(v) {
                                    Some(Probe::Loss { plies }) => {
                                        ext_loss[i] = ext_loss[i].min(plies as i16);
                                    }
                                    Some(Probe::Win { plies }) => ext_win[i] = ext_win[i].max(plies as i16),
                                    _ => ext_draw[i] = true,
                                }
                            }
                        }
                    }
                }
            }
        }
        offsets[SIZE] = children.len() as u32;

        let max_ext =
            ext_loss.iter().filter(|&&v| v != i16::MAX).chain(ext_win.iter()).copied().max().unwrap_or(0);
        let mut quiet_passes = 0;
        for n in 1..=1000i16 {
            let mut changed = false;
            let mut updates: Vec<(usize, i16)> = Vec::new();
            for i in 0..SIZE {
                if values[i] != UNKNOWN {
                    continue;
                }
                let kids = &children[offsets[i] as usize..offsets[i + 1] as usize];
                // Win in n: some move reaches a position lost in n − 1.
                let target = enc_loss((n - 1) as u16);
                if ext_loss[i] == n - 1 || kids.iter().any(|&c| values[c as usize] == target) {
                    updates.push((i, enc_win(n as u16)));
                    continue;
                }
                // Loss in n: every move reaches a position won in ≤ n − 1,
                // and at least one in exactly n − 1.
                if ext_draw[i] || ext_loss[i] != i16::MAX {
                    continue;
                }
                let mut all_win = true;
                let mut longest = ext_win[i];
                for &c in kids {
                    let v = values[c as usize];
                    if v > 0 && v < n {
                        longest = longest.max(v);
                    } else {
                        all_win = false;
                        break;
                    }
                }
                if all_win && longest == n - 1 {
                    updates.push((i, enc_loss(n as u16)));
                }
            }
            for (i, v) in updates {
                values[i] = v;
                changed = true;
            }
            if changed {
                quiet_passes = 0;
            } else {
                quiet_passes += 1;
                if quiet_passes >= 2 && n > max_ext + 2 {
                    break;
                }
            }
        }
        for v in &mut values {
            if *v == UNKNOWN {
                *v = DRAW;
            }
        }
        Table { values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(fen: &str) -> Probe {
        Tablebases::default().probe(&Board::from_fen(fen, false).unwrap()).unwrap()
    }

    #[test]
    fn mate_on_the_board_and_bare_minor_draws() {
        assert_eq!(probe("k7/1Q6/1K6/8/8/8/8/8 b - - 0 1"), Probe::Loss { plies: 0 });
        assert_eq!(probe("k7/8/1K6/8/8/8/8/1N6 w - - 0 1"), Probe::Draw);
        assert_eq!(probe("k7/8/1K6/8/8/8/8/1B6 b - - 0 1"), Probe::Draw);
    }

    #[test]
    fn queen_and_rook_win_with_known_maximums() {
        let mut tb = Tablebases::default();
        let q = tb.table(Piece::Queen).values.iter().filter(|&&v| v > 0).max().copied().unwrap();
        let r = tb.table(Piece::Rook).values.iter().filter(|&&v| v > 0).max().copied().unwrap();
        // Longest KQK win is mate in 10 (19 plies), KRK mate in 16 (31 plies).
        assert_eq!(q, 19);
        assert_eq!(r, 31);
        assert!(matches!(probe("8/8/8/4k3/8/8/8/4K2R w - - 0 1"), Probe::Win { .. }));
    }

    #[test]
    fn king_and_pawn() {
        // Opposition: wins with White to move from the classic square,
        // draws when the defending king is in front with the opposition.
        assert!(matches!(probe("8/8/8/4k3/8/4K3/4P3/8 w - - 0 1"), Probe::Draw));
        assert!(matches!(probe("4k3/8/4K3/4P3/8/8/8/8 w - - 0 1"), Probe::Win { .. }));
        // Results are for the side to move: here Black, who is lost.
        assert!(matches!(probe("4k3/8/4K3/4P3/8/8/8/8 b - - 0 1"), Probe::Loss { .. }));
        // Rook pawn with the defender in the corner: a draw.
        assert_eq!(probe("k7/8/1K6/P7/8/8/8/8 w - - 0 1"), Probe::Draw);
        // Black side with the pawn (colours flipped internally).
        assert!(matches!(probe("8/4p3/4k3/8/8/8/8/4K3 b - - 0 1"), Probe::Win { .. }));
    }

    #[test]
    fn not_three_pieces() {
        assert!(Tablebases::default().probe(&Board::default()).is_none());
    }
}
