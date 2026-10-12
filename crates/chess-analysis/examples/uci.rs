//! A minimal UCI front end for the engine, so it can play in standard
//! tools (cutechess-cli, fastchess, lichess-bot) and in the match harness
//! against Stockfish.
//!
//! cargo build --release -p open-chess-analysis --example uci

use std::io::{self, BufRead, Write};

use chess_core::Game;
use open_chess_analysis::search::{Limits, Score, Searcher};
use open_chess_analysis::{history_hashes, std_uci};

fn main() {
    let mut searcher = Searcher::new(1 << 20);
    let mut game = Game::new(None).expect("start position");
    let mut threads = 1usize;
    let stdin = io::stdin();
    let mut out = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.first().copied() {
            Some("uci") => {
                let _ = writeln!(
                    out,
                    "id name open-chess engine\nid author Mark Olliver\noption name EvalFile type string default <empty>\noption name Threads type spin default 1 min 1 max 256\nuciok"
                );
            }
            Some("isready") => {
                let _ = writeln!(out, "readyok");
            }
            Some("ucinewgame") => searcher.clear(),
            Some("setoption") if words.iter().any(|w| w.eq_ignore_ascii_case("Threads")) => {
                if let Some(v) = words.iter().position(|&w| w == "value").and_then(|i| words.get(i + 1)) {
                    threads = v.parse().unwrap_or(1).clamp(1, 256);
                }
            }
            Some("setoption") => {
                // setoption name EvalFile value <path>: an NNUE weight file.
                if let Some(i) = words.iter().position(|&w| w == "value")
                    && words.iter().any(|&w| w.eq_ignore_ascii_case("EvalFile"))
                {
                    let path = words[i + 1..].join(" ");
                    let net = if path.is_empty() || path == "<empty>" {
                        None
                    } else {
                        match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| {
                            open_chess_analysis::nnue::Network::from_bytes(&b).map_err(|e| e.to_string())
                        }) {
                            Ok(n) => Some(std::sync::Arc::new(n)),
                            Err(e) => {
                                let _ = writeln!(out, "info string can't load {path}: {e}");
                                None
                            }
                        }
                    };
                    searcher.set_network(net);
                }
            }
            Some("position") => {
                if let Some(g) = position(&words[1..]) {
                    game = g;
                }
            }
            Some("go") => {
                let limits = go(&words[1..], &game);
                let board = game.board().clone();
                let r = searcher.search_threads(&board, &history_hashes(&game), limits, threads);
                if let Some(l) = r.lines.first() {
                    let score = match l.score {
                        Score::Cp { value } => format!("cp {value}"),
                        Score::Mate { moves } => format!("mate {moves}"),
                    };
                    let mut b = board.clone();
                    let pv: Vec<String> = l
                        .moves
                        .iter()
                        .map(|&m| {
                            let s = std_uci(&b, m);
                            b.play_unchecked(m);
                            s
                        })
                        .collect();
                    let _ = writeln!(
                        out,
                        "info depth {} nodes {} score {score} pv {}",
                        r.depth,
                        r.nodes,
                        pv.join(" ")
                    );
                    let _ = writeln!(out, "bestmove {}", pv[0]);
                } else {
                    let _ = writeln!(out, "bestmove 0000");
                }
            }
            Some("quit") => break,
            _ => {}
        }
        let _ = out.flush();
    }
}

fn position(w: &[&str]) -> Option<Game> {
    let moves_at = w.iter().position(|&x| x == "moves");
    let head = &w[..moves_at.unwrap_or(w.len())];
    let fen = match head.first().copied() {
        Some("startpos") => None,
        Some("fen") => Some(head[1..].join(" ")),
        _ => return None,
    };
    let mut g = Game::new(fen.as_deref()).ok()?;
    if let Some(i) = moves_at {
        for m in &w[i + 1..] {
            g.play_uci(m).ok()?;
        }
    }
    Some(g)
}

fn num(w: &[&str], key: &str) -> Option<u64> {
    w.iter().position(|&x| x == key).and_then(|i| w.get(i + 1)).and_then(|v| v.parse().ok())
}

/// Turn a `go` command into limits: a fixed move time, or a share of the
/// clock (a 25th of what's left plus half the increment).
fn go(w: &[&str], game: &Game) -> Limits {
    let white = game.board().side_to_move() == cozy_chess::Color::White;
    let nodes = num(w, "nodes").unwrap_or(u64::MAX / 2);
    let depth = num(w, "depth").map_or(64, |d| d.min(64) as u8);
    let mut time_ms = num(w, "movetime");
    let (t, inc) = if white { (num(w, "wtime"), num(w, "winc")) } else { (num(w, "btime"), num(w, "binc")) };
    if let Some(t) = t {
        let budget = (t / 25 + inc.unwrap_or(0) / 2).min(t.saturating_sub(50)).max(10);
        time_ms = Some(time_ms.map_or(budget, |m| m.min(budget)));
    }
    Limits { depth, nodes, multipv: 1, time_ms }
}
