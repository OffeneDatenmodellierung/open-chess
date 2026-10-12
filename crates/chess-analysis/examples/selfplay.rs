//! Bot-vs-bot games with a full analysis at every ply: a smoke test for
//! panics and a rough speed and strength check.
//!
//! cargo run --release -p open-chess-analysis --example selfplay -- 6 2 4

use std::time::Instant;

use chess_core::Game;
use open_chess_analysis::search::Searcher;
use open_chess_analysis::{AnalyseOptions, Level, analyse_with, bot_move};

fn main() {
    let args: Vec<u8> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (white, black, games) = (
        args.first().copied().unwrap_or(6),
        args.get(1).copied().unwrap_or(2),
        args.get(2).copied().unwrap_or(2),
    );
    let mut score = 0.0;
    let mut analysis_ms = Vec::new();
    for g in 0..games {
        let (lw, lb) = if g % 2 == 0 { (white, black) } else { (black, white) };
        let mut game = Game::new(None).unwrap();
        let (mut sw, mut sb, mut sa) = (Searcher::default(), Searcher::default(), Searcher::default());
        while game.outcome().is_none() && game.ply() < 300 {
            let started = Instant::now();
            let analysis = analyse_with(&mut sa, &game, AnalyseOptions::default());
            analysis_ms.push(started.elapsed().as_millis());
            assert!(!analysis.lines.is_empty());
            let white_to_move = game.ply().is_multiple_of(2);
            let (s, l) = if white_to_move { (&mut sw, lw) } else { (&mut sb, lb) };
            let m = bot_move(s, &game, Level::new(l), u64::from(g) * 1000 + game.ply() as u64).unwrap();
            game.play_uci(&m.uci).unwrap();
        }
        let o = game.outcome();
        let w = o.as_ref().and_then(|o| o.winner).map(chess_core::Side::name);
        let first_is_white = g % 2 == 0;
        score += match w {
            Some("white") if first_is_white => 1.0,
            Some("black") if !first_is_white => 1.0,
            None => 0.5,
            _ => 0.0,
        };
        println!("game {g}: L{lw} vs L{lb}: {:?} after {} plies; {}", w, game.ply(), game.sans().join(" "));
    }
    analysis_ms.sort_unstable();
    let n = analysis_ms.len();
    println!(
        "level {white} scored {score}/{games}; analysis ms p50 {} p90 {} max {}",
        analysis_ms[n / 2],
        analysis_ms[n * 9 / 10],
        analysis_ms[n - 1]
    );
}
