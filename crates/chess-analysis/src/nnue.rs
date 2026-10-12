//! An efficiently updatable neural network (NNUE) evaluation.
//!
//! The architecture is the common "(768 → H)×2 → 1" perspective network:
//! 768 inputs (6 piece kinds × 2 colours × 64 squares, seen from each
//! side), one hidden layer of `H` neurons per side with a squared clipped
//! ReLU, and one output. Hidden-layer sums are kept per search ply and
//! updated by the few pieces a move changes, so evaluating a position
//! costs one `2H` dot product.
//!
//! The weight file is the raw quantised layout written by
//! `examples/train.rs` and by the `bullet` trainer's simple-network export
//! (MIT licensed, GPU), so a network trained on a proper machine drops
//! straight in: `768·H` input weights, `H` input biases, `2H` output
//! weights and one output bias, all little-endian `i16`, quantised with
//! `QA = 255`, `QB = 64` and scaled by `SCALE = 400`.
//!
//! No network ships yet: the hand-written evaluation stays the default
//! until one trained on enough games beats it in a match.

use cozy_chess::{Board, Color, Piece};

/// Input-layer quantisation.
pub const QA: i32 = 255;
/// Output-layer quantisation.
pub const QB: i32 = 64;
/// Network output units to centipawns.
pub const SCALE: i32 = 400;
/// Number of inputs per perspective.
pub const INPUTS: usize = 768;

/// A loaded network.
#[derive(Debug, Clone)]
pub struct Network {
    hidden: usize,
    l0w: Vec<i16>,
    l0b: Vec<i16>,
    l1w: Vec<i16>,
    l1b: i16,
}

/// Why a weight file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadNetwork(pub String);

impl std::fmt::Display for BadNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The input index of `piece` of `color` on `sq` seen from `perspective`:
/// that side's pieces first, squares mirrored for Black.
pub fn feature(perspective: Color, piece: Piece, color: Color, sq: cozy_chess::Square) -> usize {
    let side = usize::from(color != perspective);
    let s = if perspective == Color::White { sq as usize } else { sq as usize ^ 0b11_1000 };
    side * 384 + piece as usize * 64 + s
}

impl Network {
    /// Parse a weight file. The hidden size follows from the file's length
    /// (up to 64 bytes of trailing padding are allowed).
    pub fn from_bytes(bytes: &[u8]) -> Result<Network, BadNetwork> {
        let n = bytes.len() / 2;
        let per_hidden = INPUTS + 1 + 2;
        if n < per_hidden + 1 {
            return Err(BadNetwork("the network file is too small".into()));
        }
        let hidden = (n - 1) / per_hidden;
        let used = hidden * per_hidden + 1;
        if n - used > 32 || hidden == 0 || hidden > 4096 {
            return Err(BadNetwork(format!("{} bytes isn't a (768 → H)×2 → 1 network", bytes.len())));
        }
        let vals: Vec<i16> =
            bytes.chunks_exact(2).take(used).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        let (l0w, rest) = vals.split_at(INPUTS * hidden);
        let (l0b, rest) = rest.split_at(hidden);
        let (l1w, rest) = rest.split_at(2 * hidden);
        Ok(Network { hidden, l0w: l0w.to_vec(), l0b: l0b.to_vec(), l1w: l1w.to_vec(), l1b: rest[0] })
    }

    /// The weight file for this network.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.l0w
            .iter()
            .chain(&self.l0b)
            .chain(&self.l1w)
            .chain(std::iter::once(&self.l1b))
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    /// Build from quantised parts (used by the trainer).
    ///
    /// # Panics
    ///
    /// If the parts' lengths don't match `hidden`.
    pub fn from_parts(hidden: usize, l0w: Vec<i16>, l0b: Vec<i16>, l1w: Vec<i16>, l1b: i16) -> Network {
        assert_eq!(l0w.len(), INPUTS * hidden);
        assert_eq!(l0b.len(), hidden);
        assert_eq!(l1w.len(), 2 * hidden);
        Network { hidden, l0w, l0b, l1w, l1b }
    }

    pub fn hidden(&self) -> usize {
        self.hidden
    }

    fn column(&self, f: usize) -> &[i16] {
        &self.l0w[f * self.hidden..(f + 1) * self.hidden]
    }

    /// Hidden sums for a whole board: White's perspective then Black's.
    pub fn refresh(&self, board: &Board, acc: &mut Accumulator) {
        let hidden = self.hidden;
        acc.v.clear();
        acc.v.extend_from_slice(&self.l0b);
        acc.v.extend_from_slice(&self.l0b);
        for sq in board.occupied() {
            let (Some(p), Some(c)) = (board.piece_on(sq), board.color_on(sq)) else { continue };
            let (white, black) = (feature(Color::White, p, c, sq), feature(Color::Black, p, c, sq));
            add(&mut acc.v[..hidden], self.column(white));
            add(&mut acc.v[hidden..], self.column(black));
        }
    }

    /// `child`'s sums from `parent`'s, applying only the pieces that differ.
    pub fn update(&self, parent: &Board, child: &Board, from: &Accumulator, to: &mut Accumulator) {
        let h = self.hidden;
        to.v.clear();
        to.v.extend_from_slice(&from.v);
        for c in Color::ALL {
            for p in Piece::ALL {
                let before = parent.colored_pieces(c, p);
                let after = child.colored_pieces(c, p);
                for sq in before & !after {
                    sub(&mut to.v[..h], self.column(feature(Color::White, p, c, sq)));
                    sub(&mut to.v[h..], self.column(feature(Color::Black, p, c, sq)));
                }
                for sq in after & !before {
                    add(&mut to.v[..h], self.column(feature(Color::White, p, c, sq)));
                    add(&mut to.v[h..], self.column(feature(Color::Black, p, c, sq)));
                }
            }
        }
    }

    /// Centipawns for the side to move.
    pub fn evaluate(&self, acc: &Accumulator, stm: Color) -> i32 {
        let h = self.hidden;
        let (us, them) =
            if stm == Color::White { (&acc.v[..h], &acc.v[h..]) } else { (&acc.v[h..], &acc.v[..h]) };
        let screlu = |x: i16| {
            let c = i64::from(i32::from(x).clamp(0, QA));
            c * c
        };
        let mut sum: i64 = 0;
        for (i, &x) in us.iter().enumerate() {
            sum += screlu(x) * i64::from(self.l1w[i]);
        }
        for (i, &x) in them.iter().enumerate() {
            sum += screlu(x) * i64::from(self.l1w[h + i]);
        }
        let out = sum / i64::from(QA) + i64::from(self.l1b);
        (out * i64::from(SCALE) / i64::from(QA * QB)) as i32
    }

    /// Evaluate a board from scratch (for tests and one-off use).
    pub fn evaluate_board(&self, board: &Board) -> i32 {
        let mut acc = Accumulator::default();
        self.refresh(board, &mut acc);
        self.evaluate(&acc, board.side_to_move())
    }
}

fn add(acc: &mut [i16], w: &[i16]) {
    for (a, &b) in acc.iter_mut().zip(w) {
        *a = a.wrapping_add(b);
    }
}

fn sub(acc: &mut [i16], w: &[i16]) {
    for (a, &b) in acc.iter_mut().zip(w) {
        *a = a.wrapping_sub(b);
    }
}

/// Hidden-layer sums for one position, both perspectives.
#[derive(Debug, Clone, Default)]
pub struct Accumulator {
    v: Vec<i16>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small network with deterministic pseudo-random weights.
    pub(crate) fn toy(hidden: usize) -> Network {
        let mut x: u32 = 12345;
        let mut next = |m: i32| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            ((x % (2 * m as u32 + 1)) as i32 - m) as i16
        };
        let l0w = (0..INPUTS * hidden).map(|_| next(60)).collect();
        let l0b = (0..hidden).map(|_| next(40)).collect();
        let l1w = (0..2 * hidden).map(|_| next(100)).collect();
        Network::from_parts(hidden, l0w, l0b, l1w, next(500))
    }

    #[test]
    fn bytes_round_trip_and_hidden_size() {
        let n = toy(16);
        let mut bytes = n.to_bytes();
        let back = Network::from_bytes(&bytes).unwrap();
        assert_eq!(back.hidden(), 16);
        assert_eq!(back.to_bytes(), bytes);
        bytes.extend_from_slice(&[0; 20]); // padding is fine
        assert_eq!(Network::from_bytes(&bytes).unwrap().hidden(), 16);
        assert!(Network::from_bytes(&[0; 100]).is_err());
    }

    #[test]
    fn incremental_updates_match_a_full_refresh() {
        let net = toy(32);
        let mut game = chess_core::Game::new(None).unwrap();
        // Captures, castling both ways, en passant and promotion.
        let moves = "e2e4 d7d5 e4d5 g8f6 f1b5 c7c6 d5c6 d8a5 c6b7 e8d8 b7a8q b8d7 g1f3 e7e5 e1g1 f8c5 d2d4 e5d4 c2c4 d4c3";
        let mut acc = Accumulator::default();
        net.refresh(game.board(), &mut acc);
        for m in moves.split_whitespace() {
            let parent = game.board().clone();
            game.play_uci(m).unwrap();
            let mut updated = Accumulator::default();
            net.update(&parent, game.board(), &acc, &mut updated);
            let mut full = Accumulator::default();
            net.refresh(game.board(), &mut full);
            assert_eq!(updated.v, full.v, "after {m}");
            acc = updated;
        }
    }

    #[test]
    fn mirrored_positions_evaluate_the_same() {
        let net = toy(24);
        let a = Board::from_fen("r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3", false)
            .unwrap();
        let b = Board::from_fen("rnbqkb1r/pppp1ppp/5n2/4p3/4P3/2N5/PPPP1PPP/R1BQKBNR b KQkq - 2 3", false)
            .unwrap();
        assert_eq!(net.evaluate_board(&a), net.evaluate_board(&b));
    }
}
