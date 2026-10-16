//! The UCI protocol over any reader and writer.
//!
//! Options: `Hash` (MB), `Threads`, `MultiPV`, `EvalFile` (an NNUE weight
//! file; empty for the hand-written evaluation), `UseTablebases` (the
//! built-in three-piece tables), and every search tunable from
//! [`chess_analysis::params::TUNABLES`] as a spin option under its own
//! name, so tuning runs (OpenBench's SPSA, fastchess `option.name=value`)
//! can set them without a rebuild.
//!
//! `go` honours `depth`, `nodes`, `movetime`, `wtime`/`btime` with
//! `winc`/`binc` and `movestogo`, and `infinite` (a very deep search; the
//! engine searches synchronously, so `stop` is only read once a search
//! has returned). Extra commands: `bench [depth]`, `eval`, `tune`, `d`.

use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::sync::Arc;

use chess_analysis::nnue::Network;
use chess_analysis::search::{Limits, Score, SearchResult, Searcher};
use chess_analysis::{SearchParams, TUNABLES, history_hashes, std_uci};
use chess_core::Game;
use cozy_chess::Color;

use crate::bench;

const NAME: &str = "open-chess";
const AUTHOR: &str = "Mark Olliver";

/// The engine's settings between commands.
pub struct Engine {
    searcher: Searcher,
    game: Game,
    hash_mb: usize,
    threads: usize,
    multipv: usize,
    net: Option<Arc<Network>>,
    net_path: String,
    params: SearchParams,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    /// Defaults: 64 MB hash, one thread, one line, the hand-written evaluation.
    ///
    /// # Panics
    ///
    /// Never (the start position is valid).
    pub fn new() -> Engine {
        let hash_mb = 64;
        Engine {
            searcher: Searcher::new((hash_mb << 20) / 32),
            game: Game::new(None).expect("start position"),
            hash_mb,
            threads: 1,
            multipv: 1,
            net: None,
            net_path: String::new(),
            params: SearchParams::default(),
        }
    }

    /// The `uci` reply: identity and options.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "id name {NAME} {}\nid author {AUTHOR}\n\
             option name Hash type spin default {} min 1 max 65536\n\
             option name Threads type spin default {} min 1 max 512\n\
             option name MultiPV type spin default {} min 1 max 64\n\
             option name EvalFile type string default <empty>\n\
             option name UseTablebases type check default true\n",
            chess_analysis::engine_id(),
            self.hash_mb,
            self.threads,
            self.multipv
        );
        for t in TUNABLES {
            let _ = writeln!(
                s,
                "option name {} type spin default {} min {} max {}",
                t.name, t.default, t.min, t.max
            );
        }
        s.push_str("uciok");
        s
    }

    /// OpenBench's SPSA input: `name, int, default, min, max, c_end, r_end`.
    pub fn tune_lines() -> String {
        TUNABLES
            .iter()
            .map(|t| {
                format!("{}, int, {}, {}, {}, {}, {}", t.name, t.default, t.min, t.max, t.c_end, t.r_end)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn set_option(&mut self, name: &str, value: &str) -> Result<(), String> {
        match name.to_ascii_lowercase().as_str() {
            "hash" => {
                let mb: usize = value.parse().map_err(|_| format!("Hash {value:?} isn't a number"))?;
                self.hash_mb = mb.clamp(1, 65_536);
                self.searcher = Searcher::new((self.hash_mb << 20) / 32);
                self.searcher.set_params(self.params);
                self.searcher.set_network(self.net.clone());
            }
            "threads" => {
                self.threads = value
                    .parse::<usize>()
                    .map_err(|_| format!("Threads {value:?} isn't a number"))?
                    .clamp(1, 512);
            }
            "multipv" => {
                self.multipv = value
                    .parse::<usize>()
                    .map_err(|_| format!("MultiPV {value:?} isn't a number"))?
                    .clamp(1, 64);
            }
            "evalfile" => {
                let path = value.trim();
                self.net = if path.is_empty() || path == "<empty>" {
                    None
                } else {
                    let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
                    Some(Arc::new(Network::from_bytes(&bytes).map_err(|e| format!("{path}: {e}"))?))
                };
                self.net_path = path.to_string();
                self.searcher.set_network(self.net.clone());
            }
            "usetablebases" => {
                self.searcher.use_tablebases = value.eq_ignore_ascii_case("true");
            }
            other => {
                let v: i32 = value.parse().map_err(|_| format!("{name} {value:?} isn't a number"))?;
                if !self.params.set(other, v) {
                    return Err(format!("no option named {name}"));
                }
                self.searcher.set_params(self.params);
            }
        }
        Ok(())
    }

    fn go(&mut self, words: &[&str], out: &mut impl Write) {
        let limits = Limits { multipv: self.multipv, ..limits(words, &self.game) };
        let board = self.game.board().clone();
        let history = history_hashes(&self.game);
        let r = if self.threads > 1 {
            self.searcher.search_threads(&board, &history, limits, self.threads)
        } else {
            self.searcher.search(&board, &history, limits)
        };
        // The final iteration's lines were already printed by the
        // callback; repeat them if nothing was (an immediate stop).
        if self.threads > 1 || r.depth == 0 {
            let _ = writeln!(out, "{}", info_lines(&board, &r));
        }
        let best = r
            .lines
            .first()
            .and_then(|l| l.moves.first())
            .map_or_else(|| "0000".to_string(), |&m| std_uci(&board, m));
        let _ = writeln!(out, "bestmove {best}");
    }
}

/// `info` lines for a result (one per line of play).
pub fn info_lines(board: &cozy_chess::Board, r: &SearchResult) -> String {
    let nps = r.nodes * 1000 / r.elapsed_ms.max(1);
    r.lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
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
            format!(
                "info depth {} multipv {} score {score} nodes {} nps {nps} time {} pv {}",
                r.depth,
                i + 1,
                r.nodes,
                r.elapsed_ms,
                pv.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn num(w: &[&str], key: &str) -> Option<u64> {
    w.iter().position(|&x| x == key).and_then(|i| w.get(i + 1)).and_then(|v| v.parse().ok())
}

/// `go` arguments into limits. A clock becomes a budget of the time left
/// divided by the moves to go (or 25) plus most of the increment, never
/// more than a third of the time left.
pub fn limits(w: &[&str], game: &Game) -> Limits {
    let white = game.board().side_to_move() == Color::White;
    let nodes = num(w, "nodes").unwrap_or(u64::MAX / 2);
    let depth = num(w, "depth").map_or(64, |d| d.min(64) as u8);
    let mut time_ms = num(w, "movetime");
    let (t, inc) = if white { (num(w, "wtime"), num(w, "winc")) } else { (num(w, "btime"), num(w, "binc")) };
    if let Some(t) = t {
        let mtg = num(w, "movestogo").unwrap_or(25).clamp(1, 50);
        let inc = inc.unwrap_or(0);
        // Never plan to spend more than a third of what is left: the hard
        // stop is checked every few thousand nodes and the GUI's clock
        // includes the pipe round trip, so the plan must leave slack.
        let budget = (t / mtg + inc * 3 / 4).min(t / 3).max(5);
        time_ms = Some(time_ms.map_or(budget, |m| m.min(budget)));
    }
    Limits { depth, nodes, multipv: 1, time_ms }
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

/// The output, shared by the command loop and the search's live `info`
/// callback.
type Shared = Arc<std::sync::Mutex<Box<dyn Write + Send>>>;

/// Run the protocol until `quit` or end of input.
///
/// # Panics
///
/// Never (the start position is valid).
pub fn run(input: impl BufRead, out: impl Write + Send + 'static) -> std::io::Result<()> {
    let mut engine = Engine::new();
    let out: Shared = Arc::new(std::sync::Mutex::new(Box::new(out)));
    let board_cell = Arc::new(std::sync::Mutex::new(engine.game.board().clone()));
    let (out_cb, board_cb) = (out.clone(), board_cell.clone());
    engine.searcher.set_on_iteration(Some(Box::new(move |r: &SearchResult| {
        let b = board_cb.lock().map(|b| b.clone()).unwrap_or_default();
        if let Ok(mut o) = out_cb.lock() {
            let _ = writeln!(o, "{}", info_lines(&b, r));
            let _ = o.flush();
        }
    })));
    for line in input.lines() {
        let line = line?;
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut sink = SharedWriter(out.clone());
        match words.first().copied() {
            Some("uci") => writeln!(sink, "{}", engine.describe())?,
            Some("isready") => writeln!(sink, "readyok")?,
            Some("ucinewgame") => {
                engine.searcher.clear();
                engine.game = Game::new(None).expect("start position");
            }
            Some("setoption") => {
                let name_at = words.iter().position(|&w| w == "name");
                let value_at = words.iter().position(|&w| w == "value");
                let name = name_at
                    .map(|i| words[i + 1..value_at.unwrap_or(words.len())].join(" "))
                    .unwrap_or_default();
                let value = value_at.map(|i| words[i + 1..].join(" ")).unwrap_or_default();
                if let Err(e) = engine.set_option(&name, &value) {
                    writeln!(sink, "info string {e}")?;
                }
            }
            Some("position") => {
                if let Some(g) = position(&words[1..]) {
                    if let Ok(mut b) = board_cell.lock() {
                        *b = g.board().clone();
                    }
                    engine.game = g;
                } else {
                    writeln!(sink, "info string bad position")?;
                }
            }
            Some("go") => engine.go(&words[1..], &mut sink),
            Some("bench") => {
                let depth = words.get(1).and_then(|d| d.parse().ok()).unwrap_or(bench::DEFAULT_DEPTH);
                let mut log = SharedWriter(out.clone());
                let b = bench::run(depth, engine.net.clone(), engine.hash_mb, |s| {
                    let _ = writeln!(log, "{s}");
                });
                writeln!(sink, "{} nodes {} nps", b.nodes, b.nps())?;
            }
            Some("eval") => {
                let v = chess_analysis::eval::evaluate(engine.game.board());
                writeln!(sink, "info string static eval {v} cp (side to move)")?;
            }
            Some("tune") => writeln!(sink, "{}", Engine::tune_lines())?,
            Some("d") => writeln!(sink, "{}", engine.game.fen())?,
            Some("quit") => break,
            Some("stop") | None => {}
            Some(other) => writeln!(sink, "info string unknown command {other}")?,
        }
        sink.flush()?;
    }
    Ok(())
}

/// A writer onto the shared output.
struct SharedWriter(Shared);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut o) => o.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().map_or(Ok(()), |mut o| o.flush())
    }
}
