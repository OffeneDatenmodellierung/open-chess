//! The engine, on a thread of its own so the board keeps being read while
//! it thinks: open-chess's own engine at a level, or any UCI engine
//! (Stockfish, Leela, …) given a time per move.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use chess_analysis::bot::{Level, bot_move};
use chess_analysis::search::Searcher;
use chess_core::Game;
use chess_engine::{Engine, Limit, UciEngine};

use crate::controller::Think;

/// Which engine plays.
#[derive(Debug, Clone)]
pub enum EngineSpec {
    /// open-chess's engine, at the level each request carries (1–8).
    Builtin,
    /// A UCI engine: its path, the time per move (when the game has no
    /// clock; with one it plays to the clock), and options to set
    /// (`("Skill Level", "5")`, `("Threads", "2")` …).
    Uci { path: String, move_time: Duration, options: Vec<(String, String)> },
}

/// The engine's answer for the position asked about at `ply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub ply: usize,
    pub uci: Option<String>,
    pub error: Option<String>,
}

/// A running engine.
#[derive(Debug)]
pub struct Opponent {
    tx: Sender<Think>,
    rx: Receiver<Reply>,
}

impl Opponent {
    /// Start the engine (a UCI engine is launched and handshaken here).
    pub fn start(spec: EngineSpec) -> anyhow::Result<Opponent> {
        let (tx, think_rx) = mpsc::channel::<Think>();
        let (reply_tx, rx) = mpsc::channel::<Reply>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        thread::Builder::new().name("engine".into()).spawn(move || match spec {
            EngineSpec::Builtin => {
                let _ = ready_tx.send(Ok(()));
                let mut searcher = Searcher::new(1 << 20);
                let mut seed = 0x5eed_u64;
                for t in think_rx {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let reply = match game_of(&t) {
                        Ok(game) => Reply {
                            ply: t.ply,
                            uci: bot_move(&mut searcher, &game, Level::new(t.level), seed).map(|m| m.uci),
                            error: None,
                        },
                        Err(e) => Reply { ply: t.ply, uci: None, error: Some(e) },
                    };
                    if reply_tx.send(reply).is_err() {
                        return;
                    }
                }
            }
            EngineSpec::Uci { path, move_time, options } => {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let opts: Vec<(&str, &str)> = options.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let mut engine = match rt.block_on(UciEngine::spawn(&path, &opts)) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{path}: {e}")));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                for t in think_rx {
                    let fen = t.initial_fen.clone().unwrap_or_else(|| chess_core::STARTPOS_FEN.to_owned());
                    let limit = match t.clock {
                        Some(c) => Limit::Clock {
                            wtime: c.white_ms,
                            btime: c.black_ms,
                            winc: c.increment_ms,
                            binc: c.increment_ms,
                        },
                        None => Limit::MoveTime(move_time),
                    };
                    let reply = match rt.block_on(engine.analyse(&fen, &t.moves, limit, 1)) {
                        Ok(a) => Reply { ply: t.ply, uci: a.best_move, error: None },
                        Err(e) => Reply { ply: t.ply, uci: None, error: Some(e.to_string()) },
                    };
                    if reply_tx.send(reply).is_err() {
                        break;
                    }
                }
                rt.block_on(engine.quit());
            }
        })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Opponent { tx, rx }),
            Ok(Err(e)) => anyhow::bail!("starting the engine: {e}"),
            Err(_) => anyhow::bail!("the engine thread stopped"),
        }
    }

    pub fn ask(&self, t: Think) {
        let _ = self.tx.send(t);
    }

    /// An answer, if one is ready.
    pub fn poll(&self) -> Option<Reply> {
        self.rx.try_recv().ok()
    }
}

fn game_of(t: &Think) -> Result<Game, String> {
    Game::from_moves(t.initial_fen.as_deref(), &t.moves.join(" ")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn the_builtin_engine_answers() {
        let o = Opponent::start(EngineSpec::Builtin).unwrap();
        o.ask(Think { ply: 1, initial_fen: None, moves: vec!["e2e4".into()], level: 1, clock: None });
        let started = Instant::now();
        let reply = loop {
            if let Some(r) = o.poll() {
                break r;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "no answer");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(reply.ply, 1);
        let uci = reply.uci.expect("a move");
        let mut g = Game::from_moves(None, "e2e4").unwrap();
        assert!(g.play_uci(&uci).is_ok(), "{uci} is legal for Black");
    }

    #[test]
    fn a_uci_engine_answers() {
        // CI installs Stockfish; skip where there is none.
        let Some(path) = chess_engine::find_stockfish() else { return };
        let o = Opponent::start(EngineSpec::Uci {
            path,
            move_time: Duration::from_millis(100),
            options: vec![("Threads".into(), "1".into())],
        })
        .unwrap();
        o.ask(Think {
            ply: 2,
            initial_fen: None,
            moves: vec!["e2e4".into(), "e7e5".into()],
            level: 4,
            clock: Some(crate::controller::ClockState {
                white_ms: 60_000,
                black_ms: 60_000,
                increment_ms: 0,
            }),
        });
        let started = Instant::now();
        let reply = loop {
            if let Some(r) = o.poll() {
                break r;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "no answer");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(reply.ply, 2);
        let mut g = Game::from_moves(None, "e2e4 e7e5").unwrap();
        assert!(g.play_uci(&reply.uci.unwrap()).is_ok());
    }

    #[test]
    fn a_missing_uci_engine_is_an_error() {
        let e = Opponent::start(EngineSpec::Uci {
            path: "/nonexistent/engine".into(),
            move_time: Duration::from_millis(100),
            options: Vec::new(),
        });
        assert!(e.is_err());
    }
}
