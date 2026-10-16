//! `bench`: a fixed set of positions searched to a fixed depth, single
//! thread, with the hash cleared first. The node count is the engine's
//! signature: it changes only when the search's behaviour changes, so a
//! "no functional change" refactor can prove itself by matching it, and
//! nodes per second tracks speed. OpenBench reads the last line
//! (`<nodes> nodes <nps> nps`) for exactly this.

use std::sync::Arc;
use std::time::Instant;

use chess_analysis::nnue::Network;
use chess_analysis::search::{Limits, Searcher};
use cozy_chess::Board;

/// Default depth: a few seconds on a laptop core.
pub const DEFAULT_DEPTH: u8 = 10;

/// Openings, middlegames and endgames of every kind: tactical, quiet,
/// pawn races, fortresses and a few with mate on the board. The set
/// never changes once published (a new set is a new signature).
pub const POSITIONS: &[&str] = &[
    // Openings
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3",
    "rnbqkb1r/pp2pppp/2p2n2/3p4/2PP4/2N5/PP2PPPP/R1BQKBNR w KQkq - 0 4",
    "rnbqk2r/ppp1ppbp/3p1np1/8/2PPP3/2N5/PP3PPP/R1BQKBNR w KQkq - 0 5",
    "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
    "rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2",
    "r1bqkbnr/pp1ppppp/2n5/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3",
    "rnbqkb1r/pppp1ppp/4pn2/8/2PP4/8/PP2PPPP/RNBQKBNR w KQkq - 0 3",
    // Middlegames
    "r1bq1rk1/pp2bppp/2n1pn2/3p4/2PP4/2N2N2/PP2BPPP/R2QKB1R w KQ - 0 8",
    "r2q1rk1/1b1nbppp/pp1ppn2/8/2PNP3/1PN1B3/P3BPPP/R2Q1RK1 w - - 0 12",
    "2rq1rk1/pb1nbppp/1p2pn2/2pp4/2PP4/1PN1PNP1/PB3PBP/R2QR1K1 w - - 0 12",
    "r1bqr1k1/pp3pbp/2np1np1/2p1p3/2P1P3/2NP1NP1/PP3PBP/R1BQR1K1 w - - 0 10",
    "r3k2r/ppqbbppp/2n1pn2/3p4/3P4/2NBPN2/PPQ2PPP/R3K2R w KQkq - 0 10",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
    "r1bqkb1r/2pp1ppp/p1n2n2/1p2p3/4P3/1B3N2/PPPP1PPP/RNBQK2R w KQkq - 0 6",
    "2r2rk1/1bqnbpp1/1p1ppn1p/pP6/N1P1P3/P2B1N1P/1B2QPP1/R2R2K1 b - - 0 20",
    "r1b2rk1/2q1b1pp/p2ppn2/1p6/3QP3/1BN1B3/PPP3PP/R4RK1 w - - 0 15",
    "3r1rk1/p3qppp/2bb4/2p5/3p4/1P6/PBQN1PPP/2R2RK1 w - - 0 17",
    "2r1r1k1/pp1q1ppp/3p1b2/3P4/3Q4/5N2/PP2RPPP/4R1K1 w - - 0 20",
    "r2q1rk1/ppp2ppp/2n5/2bp4/4n3/2P2N1P/PP1PBPP1/RNBQ1RK1 b - - 0 10",
    // Tactical
    "r1bqkb1r/pppp1ppp/2n2n2/4p2Q/2B1P3/8/PPPP1PPP/RNB1K1NR w KQkq - 4 4",
    "r2qkb1r/pp2nppp/3p4/2pNN1B1/2BnP3/3P4/PPP2PPP/R2bK2R w KQkq - 1 11",
    "r1b1k2r/ppppqppp/2n2n2/2b5/2B1P3/2N2N2/PPPP1PPP/R1BQK2R w KQkq - 6 6",
    "6k1/5ppp/8/8/8/8/5PPP/3R2K1 w - - 0 1",
    "r1bq1rk1/ppp2ppp/2np1n2/2b1p3/2B1P3/2NP1N2/PPP2PPP/R1BQ1RK1 w - - 0 7",
    "2kr3r/ppp1qppp/2n1bn2/3p4/3P4/2N1PN2/PPQ2PPP/2KR1B1R w - - 0 10",
    "r3kb1r/pp1n1ppp/2p1pn2/q7/2BP4/2N1PN2/PP3PPP/R2QK2R w KQkq - 0 10",
    "1r4k1/5pp1/p2p3p/1p2n3/3B4/1P4P1/P4P1P/3R2K1 w - - 0 30",
    // Endgames
    "8/8/8/4k3/8/8/4P3/4K3 w - - 0 1",
    "8/2k5/8/8/8/8/5PPP/6K1 w - - 0 1",
    "8/8/1p6/p1p5/P1P5/1P6/8/k6K w - - 0 1",
    "8/5pk1/8/3K4/8/8/5P2/8 w - - 0 1",
    "8/8/4kpp1/3p1b2/p6P/2B5/6P1/6K1 b - - 0 1",
    "2r5/8/8/8/8/5k2/8/2R3K1 w - - 0 1",
    "8/p7/1p3k2/1P6/P2K4/8/8/8 w - - 0 1",
    "6k1/5p2/6p1/8/7p/8/6PP/6K1 b - - 0 1",
    "3k4/8/8/8/8/8/3P4/3K4 w - - 0 1",
    "8/8/8/8/8/4k3/4p3/4K3 b - - 0 1",
    "5k2/8/8/8/8/8/8/4K2R w K - 0 1",
    "r3k3/8/8/8/8/8/8/4K3 b q - 0 1",
    "8/3k4/8/8/8/8/3B4/3K3N w - - 0 1",
    "8/8/8/8/3Q4/8/8/1k1K4 w - - 0 1",
    "4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1",
    "8/8/8/8/8/8/6k1/4K2R w K - 0 1",
    "8/8/1k6/8/8/8/8/K6Q w - - 0 1",
    "8/kp6/p7/8/8/8/P7/K7 w - - 0 1",
    // Mate on the board
    "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3",
    "r1bqkb1r/pppp1Qpp/2n2n2/4p3/2B1P3/8/PPPP1PPP/RNB1K1NR b KQkq - 0 4",
    "6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1",
    "7k/8/5K2/8/8/8/8/6R1 w - - 0 1",
];

/// What a bench run produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bench {
    pub nodes: u64,
    pub millis: u64,
}

impl Bench {
    pub fn nps(&self) -> u64 {
        self.nodes * 1000 / self.millis.max(1)
    }
}

/// Run the benchmark on one thread, reporting each position to `log`.
///
/// # Panics
///
/// Never: the positions are checked by a test.
pub fn run(depth: u8, net: Option<Arc<Network>>, hash_mb: usize, mut log: impl FnMut(&str)) -> Bench {
    let mut searcher = Searcher::new((hash_mb.max(1) << 20) / 32);
    searcher.set_network(net);
    let started = Instant::now();
    let mut nodes = 0;
    for (i, fen) in POSITIONS.iter().enumerate() {
        let board = Board::from_fen(fen, false).expect("bench positions are valid");
        searcher.clear();
        let r =
            searcher.search(&board, &[], Limits { depth, nodes: u64::MAX / 2, multipv: 1, time_ms: None });
        nodes += r.nodes;
        let best = r
            .lines
            .first()
            .and_then(|l| l.moves.first())
            .map_or("0000".to_string(), |m| chess_analysis::std_uci(&board, *m));
        log(&format!(
            "position {:>2}/{}: depth {} nodes {:>9} best {best}",
            i + 1,
            POSITIONS.len(),
            r.depth,
            r.nodes
        ));
    }
    Bench { nodes, millis: started.elapsed().as_millis() as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_position_is_legal_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for fen in POSITIONS {
            let b = Board::from_fen(fen, false).unwrap_or_else(|e| panic!("{fen}: {e:?}"));
            assert!(seen.insert(b.hash()), "duplicate: {fen}");
        }
    }

    #[test]
    fn a_shallow_bench_is_deterministic() {
        let a = run(3, None, 1, |_| {});
        let b = run(3, None, 1, |_| {});
        assert_eq!(a.nodes, b.nodes);
        assert!(a.nodes > 0);
    }
}
