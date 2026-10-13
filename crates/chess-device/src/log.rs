//! The device's log: what the board, the engine, the server and the web
//! clients did, kept for the terminal screen (newest first) and written to
//! stderr when there's no screen.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// How many lines the screen keeps.
const KEEP: usize = 2000;

/// Where a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Board,
    Engine,
    Game,
    Online,
    Web,
    LiveChess,
    Clock,
    Device,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Board => "board",
            Source::Engine => "engine",
            Source::Game => "game",
            Source::Online => "online",
            Source::Web => "web",
            Source::LiveChess => "livechess",
            Source::Clock => "clock",
            Source::Device => "device",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Line {
    pub at: SystemTime,
    pub source: Source,
    pub text: String,
}

#[derive(Debug, Default)]
struct Log {
    lines: VecDeque<Line>,
    /// A screen is showing the log: don't also write to stderr.
    quiet: bool,
    /// Lines written since the start (to tell the screen something's new).
    count: u64,
}

fn log() -> &'static Mutex<Log> {
    static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
    LOG.get_or_init(Mutex::default)
}

fn lock() -> std::sync::MutexGuard<'static, Log> {
    log().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write a line.
pub fn line(source: Source, text: impl Into<String>) {
    let text = text.into();
    let mut l = lock();
    if !l.quiet {
        eprintln!("{}: {text}", source.label());
    }
    l.lines.push_front(Line { at: SystemTime::now(), source, text });
    l.lines.truncate(KEEP);
    l.count += 1;
}

/// A screen takes over: lines are kept, not written to stderr.
pub fn quiet(on: bool) {
    lock().quiet = on;
}

/// The newest `n` lines, newest first, and how many have ever been written.
pub fn newest(n: usize) -> (Vec<Line>, u64) {
    let l = lock();
    (l.lines.iter().take(n).cloned().collect(), l.count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_first() {
        quiet(true);
        line(Source::Board, "first");
        line(Source::Engine, "second");
        let (lines, count) = newest(2);
        assert_eq!(lines[0].text, "second");
        assert_eq!(lines[1].text, "first");
        assert!(count >= 2);
    }
}
