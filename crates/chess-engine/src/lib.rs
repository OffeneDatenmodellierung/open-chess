//! Drive a UCI chess engine (Stockfish, Leela, …) as a child process.
//!
//! Used by the fair-play analyser (multi-PV evaluation of every position)
//! and by bots (best move under a clock). The [`Engine`] trait is the
//! seam: tests and other engines (a remote one, an in-process one) can
//! stand in for [`UciEngine`].

use std::fmt;
use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The engine said something unexpected, quit or took too long.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "engine I/O: {e}"),
            Error::Protocol(m) => write!(f, "engine: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An evaluation from the side to move's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Score {
    /// Centipawns.
    Cp(i32),
    /// Mate in N moves (negative: being mated).
    Mate(i32),
}

impl Score {
    /// Centipawns, with mates mapped to ±`mate_cp` (closer mates higher).
    pub fn to_cp(self, mate_cp: i32) -> i32 {
        match self {
            Score::Cp(c) => c,
            Score::Mate(n) if n > 0 => mate_cp - n.min(100),
            Score::Mate(n) if n < 0 => -mate_cp - n.max(-100),
            // "mate 0": the side to move is mated.
            Score::Mate(_) => -mate_cp,
        }
    }

    /// From the other side's point of view.
    #[must_use]
    pub fn negate(self) -> Score {
        match self {
            Score::Cp(c) => Score::Cp(-c),
            Score::Mate(n) => Score::Mate(-n),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub multipv: u32,
    pub depth: u32,
    pub score: Score,
    /// Principal variation, UCI moves.
    pub pv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Analysis {
    pub best_move: Option<String>,
    /// Best line first.
    pub lines: Vec<Line>,
}

/// How long to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Depth(u32),
    Nodes(u64),
    MoveTime(Duration),
    /// Play under a clock (milliseconds), as a bot does.
    Clock {
        wtime: i64,
        btime: i64,
        winc: i64,
        binc: i64,
    },
}

impl Limit {
    fn go(self) -> String {
        match self {
            Limit::Depth(d) => format!("go depth {d}"),
            Limit::Nodes(n) => format!("go nodes {n}"),
            Limit::MoveTime(t) => format!("go movetime {}", t.as_millis()),
            Limit::Clock { wtime, btime, winc, binc } => {
                format!("go wtime {wtime} btime {btime} winc {winc} binc {binc}")
            }
        }
    }
}

/// Anything that can analyse a position given as a FEN plus UCI moves.
pub trait Engine: Send {
    fn analyse(
        &mut self,
        fen: &str,
        moves: &[String],
        limit: Limit,
        multipv: u32,
    ) -> impl Future<Output = Result<Analysis>> + Send;

    /// Forget hash tables etc. before an unrelated game.
    fn new_game(&mut self) -> impl Future<Output = Result<()>> + Send;
}

/// A UCI engine in a child process.
pub struct UciEngine {
    name: String,
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    multipv: u32,
    /// Longest any single reply may take (a safety net, not a search limit).
    pub timeout: Duration,
}

impl fmt::Debug for UciEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UciEngine").field("name", &self.name).finish_non_exhaustive()
    }
}

impl UciEngine {
    /// Start `path` and run the UCI handshake, then set `options`
    /// (e.g. `("Threads", "1")`, `("Hash", "64")`).
    pub async fn spawn(path: &str, options: &[(&str, &str)]) -> Result<UciEngine> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Protocol("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Protocol("no stdout".into()))?;
        let mut e = UciEngine {
            name: path.to_owned(),
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            multipv: 1,
            timeout: Duration::from_mins(1),
        };
        e.send("uci").await?;
        while let Some(line) = e.line().await? {
            if let Some(n) = line.strip_prefix("id name ") {
                n.trim().clone_into(&mut e.name);
            }
            if line.trim() == "uciok" {
                break;
            }
        }
        for (k, v) in options {
            e.send(&format!("setoption name {k} value {v}")).await?;
        }
        e.ready().await?;
        Ok(e)
    }

    /// The engine's own name (`Stockfish 16`).
    pub fn name(&self) -> &str {
        &self.name
    }

    async fn send(&mut self, cmd: &str) -> Result<()> {
        self.stdin.write_all(cmd.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn line(&mut self) -> Result<Option<String>> {
        match tokio::time::timeout(self.timeout, self.stdout.next_line()).await {
            Ok(r) => Ok(r?),
            Err(_) => Err(Error::Protocol(format!("{} didn't answer in {:?}", self.name, self.timeout))),
        }
    }

    async fn ready(&mut self) -> Result<()> {
        self.send("isready").await?;
        loop {
            match self.line().await? {
                Some(l) if l.trim() == "readyok" => return Ok(()),
                Some(_) => {}
                None => return Err(Error::Protocol(format!("{} quit", self.name))),
            }
        }
    }

    /// Ask the engine to quit (it's killed on drop anyway).
    pub async fn quit(mut self) {
        let _ = self.send("quit").await;
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
    }
}

impl Engine for UciEngine {
    async fn analyse(&mut self, fen: &str, moves: &[String], limit: Limit, multipv: u32) -> Result<Analysis> {
        let multipv = multipv.max(1);
        if multipv != self.multipv {
            self.send(&format!("setoption name MultiPV value {multipv}")).await?;
            self.multipv = multipv;
        }
        let mut pos = if fen.is_empty() || fen == "startpos" {
            "position startpos".to_owned()
        } else {
            format!("position fen {fen}")
        };
        if !moves.is_empty() {
            pos.push_str(" moves ");
            pos.push_str(&moves.join(" "));
        }
        self.send(&pos).await?;
        self.send(&limit.go()).await?;
        let mut lines: Vec<Line> = Vec::new();
        loop {
            let Some(l) = self.line().await? else {
                return Err(Error::Protocol(format!("{} quit mid-search", self.name)));
            };
            if let Some(info) = parse_info(&l) {
                match lines.iter_mut().find(|x| x.multipv == info.multipv) {
                    Some(x) if info.depth >= x.depth => *x = info,
                    Some(_) => {}
                    None => lines.push(info),
                }
            } else if let Some(rest) = l.strip_prefix("bestmove") {
                let best = rest.split_whitespace().next().filter(|m| *m != "(none)").map(str::to_owned);
                lines.sort_by_key(|x| x.multipv);
                return Ok(Analysis { best_move: best, lines });
            }
        }
    }

    async fn new_game(&mut self) -> Result<()> {
        self.send("ucinewgame").await?;
        self.ready().await
    }
}

/// Parse an `info` line that carries a scored PV (bound-only scores and
/// currmove chatter are skipped).
pub fn parse_info(line: &str) -> Option<Line> {
    let mut it = line.split_whitespace();
    if it.next()? != "info" {
        return None;
    }
    let mut depth = None;
    let mut multipv = 1;
    let mut score = None;
    let mut pv = Vec::new();
    let tokens: Vec<&str> = it.collect();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "depth" => {
                depth = tokens.get(i + 1)?.parse().ok();
                i += 2;
            }
            "multipv" => {
                multipv = tokens.get(i + 1)?.parse().ok()?;
                i += 2;
            }
            "score" => {
                let kind = *tokens.get(i + 1)?;
                let v: i32 = tokens.get(i + 2)?.parse().ok()?;
                if matches!(tokens.get(i + 3), Some(&("lowerbound" | "upperbound"))) {
                    return None;
                }
                score = Some(match kind {
                    "cp" => Score::Cp(v),
                    "mate" => Score::Mate(v),
                    _ => return None,
                });
                i += 3;
            }
            "pv" => {
                pv = tokens[i + 1..].iter().map(|s| (*s).to_owned()).collect();
                break;
            }
            _ => i += 1,
        }
    }
    if pv.is_empty() {
        return None;
    }
    Some(Line { multipv, depth: depth?, score: score?, pv })
}

/// Where Stockfish usually is: `$STOCKFISH`, then the usual paths.
pub fn find_stockfish() -> Option<String> {
    if let Ok(p) = std::env::var("STOCKFISH")
        && !p.is_empty()
    {
        return Some(p);
    }
    ["/usr/games/stockfish", "/usr/bin/stockfish", "/usr/local/bin/stockfish", "/opt/homebrew/bin/stockfish"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_info_lines() {
        let l = parse_info(
            "info depth 14 seldepth 18 multipv 2 score cp -23 nodes 12345 nps 1 hashfull 3 tbhits 0 time 40 pv e7e5 g1f3 b8c6",
        )
        .unwrap();
        assert_eq!(
            l,
            Line {
                multipv: 2,
                depth: 14,
                score: Score::Cp(-23),
                pv: vec!["e7e5".into(), "g1f3".into(), "b8c6".into()]
            }
        );
        let m = parse_info("info depth 20 score mate -3 pv a1a2").unwrap();
        assert_eq!((m.multipv, m.score), (1, Score::Mate(-3)));
        assert!(parse_info("info depth 10 currmove e2e4 currmovenumber 1").is_none());
        assert!(parse_info("info depth 10 score cp 30 lowerbound pv e2e4").is_none());
        assert!(parse_info("info string NNUE enabled").is_none());
        assert!(parse_info("bestmove e2e4").is_none());
    }

    #[test]
    fn score_to_cp() {
        assert_eq!(Score::Cp(50).to_cp(1000), 50);
        assert_eq!(Score::Mate(1).to_cp(1000), 999);
        assert_eq!(Score::Mate(-2).to_cp(1000), -998);
        assert_eq!(Score::Mate(0).to_cp(1000), -1000);
        assert_eq!(Score::Mate(3).negate(), Score::Mate(-3));
    }

    #[test]
    fn go_commands() {
        assert_eq!(Limit::Depth(12).go(), "go depth 12");
        assert_eq!(Limit::MoveTime(Duration::from_millis(150)).go(), "go movetime 150");
        assert_eq!(
            Limit::Clock { wtime: 1000, btime: 2000, winc: 0, binc: 1 }.go(),
            "go wtime 1000 btime 2000 winc 0 binc 1"
        );
    }

    #[tokio::test]
    async fn stockfish_finds_mate_in_one() {
        let Some(sf) = find_stockfish() else {
            eprintln!("no Stockfish here: skipped");
            return;
        };
        let mut e = UciEngine::spawn(&sf, &[("Threads", "1"), ("Hash", "16")]).await.unwrap();
        assert!(e.name().to_lowercase().contains("stockfish"), "{}", e.name());
        e.new_game().await.unwrap();
        let a = e.analyse("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", &[], Limit::Depth(8), 2).await.unwrap();
        assert_eq!(a.best_move.as_deref(), Some("a1a8"));
        assert_eq!(a.lines[0].score, Score::Mate(1));
        assert!(a.lines.len() <= 2);
        let a = e.analyse("startpos", &["e2e4".into()], Limit::Nodes(20_000), 1).await.unwrap();
        assert!(a.best_move.is_some());
        e.quit().await;
    }
}
