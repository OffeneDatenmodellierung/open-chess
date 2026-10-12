//! Self-play training data for the evaluation network.
//!
//! Each game starts with a few random moves (for variety), then the
//! engine plays itself at a fixed node count. Quiet positions are written
//! one per line as `FEN | score | result`, with the score in centipawns
//! and the result (1.0, 0.5, 0.0) both from White's side: the text format
//! `examples/train.rs` and the `bullet` trainer read.
//!
//! ```text
//! cargo run --release -p open-chess-analysis --example datagen -- \
//!     --games 1000 --nodes 5000 --threads 8 --out data.txt
//! ```

use std::io::{BufWriter, Write};
use std::sync::mpsc;
use std::thread;

use chess_core::{Game, Side};
use cozy_chess::{Board, Color};
use open_chess_analysis::search::{Limits, Score, Searcher};
use open_chess_analysis::{history_hashes, std_uci};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

fn legal(board: &Board) -> Vec<cozy_chess::Move> {
    let mut v = Vec::new();
    board.generate_moves(|pm| {
        v.extend(pm);
        false
    });
    v
}

/// One game: the lines to write.
fn game(rng: &mut Rng, nodes: u64, searcher: &mut Searcher) -> Vec<String> {
    let mut g = Game::new(None).expect("start");
    searcher.clear();
    // 8–9 random plies, retrying if the game ends or gets lopsided.
    let random = 8 + (rng.next() % 2) as usize;
    for _ in 0..random {
        let b = g.board().clone();
        let moves = legal(&b);
        if moves.is_empty() {
            return Vec::new();
        }
        let m = moves[(rng.next() % moves.len() as u64) as usize];
        if g.play_uci(&std_uci(&b, m)).is_err() {
            return Vec::new();
        }
    }
    let mut records: Vec<(String, i32)> = Vec::new();
    let mut decisive = 0;
    let mut result: Option<f32> = None;
    while g.outcome().is_none() && g.ply() < 400 {
        let b = g.board().clone();
        let r =
            searcher.search(&b, &history_hashes(&g), Limits { depth: 64, nodes, multipv: 1, time_ms: None });
        let Some(line) = r.lines.first() else { break };
        let mv = line.moves[0];
        let white = b.side_to_move() == Color::White;
        let stm_cp = match line.score {
            Score::Cp { value } => value,
            Score::Mate { moves } => {
                if moves > 0 {
                    20_000
                } else {
                    -20_000
                }
            }
        };
        let white_cp = if white { stm_cp } else { -stm_cp };
        // Adjudicate clear wins to save time.
        decisive = if white_cp.abs() >= 2000 { decisive + 1 } else { 0 };
        if decisive >= 6 {
            result = Some(if white_cp > 0 { 1.0 } else { 0.0 });
            break;
        }
        // Keep quiet positions only: not in check, best move not a capture,
        // no mate score.
        let quiet =
            b.checkers().is_empty() && b.color_on(mv.to).is_none() && matches!(line.score, Score::Cp { .. });
        if quiet {
            records.push((format!("{b}"), white_cp));
        }
        if g.play_uci(&std_uci(&b, mv)).is_err() {
            break;
        }
    }
    let result = result.unwrap_or_else(|| match g.outcome().and_then(|o| o.winner) {
        Some(Side::White) => 1.0,
        Some(Side::Black) => 0.0,
        None => 0.5,
    });
    records.into_iter().map(|(fen, cp)| format!("{fen} | {cp} | {result:.1}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let games: u64 = get("--games").and_then(|v| v.parse().ok()).unwrap_or(100);
    let nodes: u64 = get("--nodes").and_then(|v| v.parse().ok()).unwrap_or(5000);
    let threads: u64 = get("--threads").and_then(|v| v.parse().ok()).unwrap_or(2).max(1);
    let seed: u64 = get("--seed").and_then(|v| v.parse().ok()).unwrap_or(1);
    let out = get("--out").unwrap_or_else(|| "data.txt".into());
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    for t in 0..threads {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut rng = Rng(seed.wrapping_mul(1000).wrapping_add(t));
            let mut searcher = Searcher::new(1 << 16);
            let mine = games / threads + u64::from(t < games % threads);
            for _ in 0..mine {
                if tx.send(game(&mut rng, nodes, &mut searcher)).is_err() {
                    return;
                }
            }
        });
    }
    drop(tx);
    let mut w = BufWriter::new(std::fs::File::create(&out).expect("output file"));
    let (mut n_games, mut n_pos) = (0u64, 0u64);
    for lines in rx {
        n_games += 1;
        n_pos += lines.len() as u64;
        for l in lines {
            let _ = writeln!(w, "{l}");
        }
        if n_games.is_multiple_of(50) {
            eprintln!("{n_games}/{games} games, {n_pos} positions");
        }
    }
    let _ = w.flush();
    eprintln!("done: {n_games} games, {n_pos} positions in {out}");
}
