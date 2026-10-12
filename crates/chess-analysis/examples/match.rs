//! Play two UCI engines against each other and estimate the Elo difference.
//!
//! Used to measure each engine change (new build vs old build) and to
//! measure the engine against Stockfish. Openings come from the book,
//! each played twice with colours swapped.
//!
//! ```text
//! cargo run --release -p open-chess-analysis --example match -- \
//!     --a ./target/release/examples/uci --b stockfish --b-opt "UCI_LimitStrength=true" \
//!     --b-opt "UCI_Elo=2000" --games 40 --tc 10000+100
//! ```
//!
//! `--tc` is `base+inc` in milliseconds, `--movetime MS` or `--nodes N`.

#![allow(clippy::many_single_char_names)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use chess_core::{Game, Side};
use open_chess_analysis::openings;

struct Uci {
    name: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Uci {
    fn spawn(cmd: &str, opts: &[String]) -> Uci {
        let mut parts = cmd.split_whitespace();
        let prog = parts.next().expect("engine command");
        let mut child = Command::new(prog)
            .args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut e = Uci { name: cmd.to_string(), child, stdin, stdout };
        e.send("uci");
        while let Some(l) = e.line() {
            if let Some(n) = l.strip_prefix("id name ") {
                e.name = n.trim().to_string();
            }
            if l.trim() == "uciok" {
                break;
            }
        }
        for o in opts {
            let (k, v) = o.split_once('=').unwrap_or((o, ""));
            e.send(&format!("setoption name {k} value {v}"));
        }
        e.ready();
        e
    }

    fn send(&mut self, s: &str) {
        let _ = writeln!(self.stdin, "{s}");
        let _ = self.stdin.flush();
    }

    fn line(&mut self) -> Option<String> {
        let mut l = String::new();
        (self.stdout.read_line(&mut l).ok()? > 0).then_some(l)
    }

    fn ready(&mut self) {
        self.send("isready");
        while let Some(l) = self.line() {
            if l.trim() == "readyok" {
                break;
            }
        }
    }

    fn best(&mut self, position: &str, go: &str) -> String {
        self.send(position);
        self.send(go);
        while let Some(l) = self.line() {
            if let Some(m) = l.strip_prefix("bestmove ") {
                return m.split_whitespace().next().unwrap_or("0000").to_string();
            }
        }
        "0000".into()
    }
}

impl Drop for Uci {
    fn drop(&mut self) {
        self.send("quit");
        let _ = self.child.wait();
    }
}

enum Tc {
    Clock { base: u64, inc: u64 },
    MoveTime(u64),
    Nodes(u64),
}

/// 1 = first engine (white) wins, 0 = loss, 0.5 = draw.
fn play(white: &mut Uci, black: &mut Uci, opening: &[&str], tc: &Tc) -> (f64, String) {
    white.send("ucinewgame");
    black.send("ucinewgame");
    let mut game = Game::new(None).expect("start");
    for m in opening {
        game.play_uci(m).expect("book move");
    }
    let (mut wt, mut bt) = match tc {
        Tc::Clock { base, .. } => (*base as i64, *base as i64),
        _ => (0, 0),
    };
    while game.outcome().is_none() && game.ply() < 400 {
        let pos = format!("position startpos moves {}", game.uci_moves().join(" "));
        let go = match tc {
            Tc::Clock { inc, .. } => format!("go wtime {wt} btime {bt} winc {inc} binc {inc}"),
            Tc::MoveTime(ms) => format!("go movetime {ms}"),
            Tc::Nodes(n) => format!("go nodes {n}"),
        };
        let white_to_move = game.turn() == Side::White;
        let t = Instant::now();
        let mv = if white_to_move { white.best(&pos, &go) } else { black.best(&pos, &go) };
        let spent = t.elapsed().as_millis() as i64;
        if let Tc::Clock { inc, .. } = tc {
            let clock = if white_to_move { &mut wt } else { &mut bt };
            *clock -= spent;
            if *clock < 0 {
                let r = if white_to_move { 0.0 } else { 1.0 };
                return (r, "time forfeit".into());
            }
            *clock += *inc as i64;
        }
        if game.play_uci(&mv).is_err() {
            let r = if white_to_move { 0.0 } else { 1.0 };
            return (r, format!("illegal move {mv}"));
        }
    }
    match game.outcome() {
        Some(o) => match o.winner {
            Some(Side::White) => (1.0, o.status.name().into()),
            Some(Side::Black) => (0.0, o.status.name().into()),
            None => (0.5, o.status.name().into()),
        },
        None => (0.5, "move limit".into()),
    }
}

fn elo(score: f64) -> f64 {
    let s = score.clamp(0.001, 0.999);
    -400.0 * (1.0 / s - 1.0).log10()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let all =
        |k: &str| -> Vec<String> { args.windows(2).filter(|w| w[0] == k).map(|w| w[1].clone()).collect() };
    let a_cmd = get("--a").expect("--a ENGINE");
    let b_cmd = get("--b").expect("--b ENGINE");
    let games: usize = get("--games").and_then(|v| v.parse().ok()).unwrap_or(20);
    let tc = if let Some(n) = get("--nodes") {
        Tc::Nodes(n.parse().expect("nodes"))
    } else if let Some(ms) = get("--movetime") {
        Tc::MoveTime(ms.parse().expect("movetime"))
    } else {
        let tc = get("--tc").unwrap_or_else(|| "10000+100".into());
        let (b, i) = tc.split_once('+').unwrap_or((&tc, "0"));
        Tc::Clock { base: b.parse().expect("base"), inc: i.parse().expect("inc") }
    };
    let mut a = Uci::spawn(&a_cmd, &all("--a-opt"));
    let mut b = Uci::spawn(&b_cmd, &all("--b-opt"));
    // Short, varied openings: the first four to eight plies of book lines.
    let book: Vec<Vec<&'static str>> = openings::all()
        .into_iter()
        .filter(|o| o.moves.len() >= 4)
        .map(|o| o.moves.into_iter().take(8).collect())
        .collect();
    println!("{} vs {}, {games} games", a.name, b.name);
    let (mut w, mut d, mut l) = (0, 0, 0);
    for g in 0..games {
        let opening = &book[(g / 2) % book.len()];
        let a_white = g % 2 == 0;
        let (r, why) =
            if a_white { play(&mut a, &mut b, opening, &tc) } else { play(&mut b, &mut a, opening, &tc) };
        let a_score = if a_white { r } else { 1.0 - r };
        match a_score {
            x if x > 0.75 => w += 1,
            x if x < 0.25 => l += 1,
            _ => d += 1,
        }
        let n = f64::from(w + d + l);
        let score = (f64::from(w) + f64::from(d) / 2.0) / n;
        println!(
            "game {:>3}: {} {} ({why})  total +{w} ={d} -{l}  score {:.1}%  elo {:+.0}",
            g + 1,
            if a_white { "white" } else { "black" },
            match a_score {
                x if x > 0.75 => "win ",
                x if x < 0.25 => "loss",
                _ => "draw",
            },
            score * 100.0,
            elo(score)
        );
    }
    let n = f64::from(w + d + l);
    let score = (f64::from(w) + f64::from(d) / 2.0) / n;
    // 95% interval from the per-game variance.
    let var = (f64::from(w) * (1.0 - score).powi(2)
        + f64::from(d) * (0.5 - score).powi(2)
        + f64::from(l) * score.powi(2))
        / n;
    let margin = 1.96 * (var / n).sqrt();
    println!(
        "result: +{w} ={d} -{l}, score {:.1}%, Elo {:+.0} (95%: {:+.0} to {:+.0})",
        score * 100.0,
        elo(score),
        elo(score - margin),
        elo(score + margin)
    );
}
