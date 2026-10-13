//! Ratings for games played on the board away from any site: against
//! open-chess's engine, or two people on one board. FIDE's Elo rules: the
//! expected score from the difference (capped at 400), K = 40 for a
//! player's first 30 games, 10 once they've reached 2400, otherwise 20; a
//! list each for blitz, rapid and classical, as FIDE keeps. A new player
//! starts at 1500. The engine's levels have fixed ratings (the same as the
//! open-chess server's computer): 800 at level 1, 200 more a level.
//!
//! The ratings are kept in `ratings.json` in the device's folder
//! ([`crate::settings::folder`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chess_core::{Side, Status};
use serde::{Deserialize, Serialize};

use crate::controller::TimeControl;

/// Where a new player starts.
pub const START: i32 = 1500;

/// FIDE's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Blitz,
    Rapid,
    Classical,
}

impl List {
    /// FIDE's split, on a game's expected length (base + 60 increments):
    /// blitz up to 10 minutes, rapid under 60, classical beyond. A game
    /// with no clock counts as classical.
    pub fn of(time: Option<TimeControl>) -> List {
        let Some(t) = time else { return List::Classical };
        let minutes = t.minutes + t.increment_secs;
        if minutes <= 10 {
            List::Blitz
        } else if minutes < 60 {
            List::Rapid
        } else {
            List::Classical
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            List::Blitz => "blitz",
            List::Rapid => "rapid",
            List::Classical => "classical",
        }
    }
}

/// The rating of open-chess's engine at `level` (1–8).
pub fn engine_rating(level: u8) -> i32 {
    800 + 200 * (i32::from(level.clamp(1, 8)) - 1)
}

/// One player's standing on one list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub rating: i32,
    pub games: u32,
    pub peak: i32,
}

impl Default for Entry {
    fn default() -> Self {
        Entry { rating: START, games: 0, peak: START }
    }
}

impl Entry {
    pub fn k(&self) -> f64 {
        if self.games < 30 {
            40.0
        } else if self.peak >= 2400 {
            10.0
        } else {
            20.0
        }
    }
}

/// The expected score against a rating `diff` points lower (FIDE caps the
/// difference at 400).
pub fn expected(diff: i32) -> f64 {
    let d = f64::from(diff.clamp(-400, 400));
    1.0 / (1.0 + 10f64.powf(-d / 400.0))
}

/// The change, rounded, for `score` (1, ½, 0) against `opponent`.
pub fn change(me: &Entry, opponent: i32, score: f64) -> i32 {
    // Within ±400 by the cap, and K ≤ 40: always a small whole number.
    #[allow(clippy::cast_possible_truncation)]
    let d = (me.k() * (score - expected(me.rating - opponent))).round() as i32;
    d
}

/// A player's line at the end of a game: who, their rating before it and
/// what the game did to it (`None` when it isn't rated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerResult {
    pub name: String,
    pub rating: Option<i32>,
    pub diff: Option<i32>,
}

impl PlayerResult {
    /// "Mark 1500 → 1520 (+20)", or "open-chess level 4 1400".
    pub fn describe(&self) -> String {
        match (self.rating, self.diff) {
            (Some(r), Some(d)) => format!("{} {r} → {} ({d:+})", self.name, r + d),
            (Some(r), None) => format!("{} {r}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// What a finished game scored for White, or `None` when it shouldn't be
/// rated (aborted, or too short to count).
pub fn white_score(status: Status, winner: Option<Side>, plies: usize) -> Option<f64> {
    if plies < 2 || matches!(status, Status::Created | Status::Started | Status::Aborted | Status::NoStart) {
        return None;
    }
    Some(match winner {
        Some(Side::White) => 1.0,
        Some(Side::Black) => 0.0,
        None => 0.5,
    })
}

#[cfg(test)]
thread_local! {
    /// Where [`Store::open`] reads and writes in this thread's tests.
    pub static TEST_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Everyone's ratings, read from and saved to a file.
#[derive(Debug, Default)]
pub struct Store {
    path: Option<PathBuf>,
    players: BTreeMap<String, BTreeMap<String, Entry>>,
}

impl Store {
    /// The store in the device's folder (empty if there's none yet).
    pub fn open() -> Store {
        #[cfg(test)]
        if let Some(p) = TEST_PATH.with(|p| p.borrow().clone()) {
            return Store::at(Some(p));
        }
        Store::at(crate::settings::folder().map(|d| d.join("ratings.json")))
    }

    pub fn at(path: Option<PathBuf>) -> Store {
        let players = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Store { path, players }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self, name: &str, list: List) -> Entry {
        self.players.get(name).and_then(|l| l.get(list.key())).copied().unwrap_or_default()
    }

    fn put(&mut self, name: &str, list: List, e: Entry) {
        self.players.entry(name.to_owned()).or_default().insert(list.key().to_owned(), e);
    }

    /// Rate a game between two players kept here.
    pub fn rate_pair(&mut self, white: &str, black: &str, list: List, white_score: f64) -> [PlayerResult; 2] {
        let (w, b) = (self.get(white, list), self.get(black, list));
        let dw = change(&w, b.rating, white_score);
        let db = change(&b, w.rating, 1.0 - white_score);
        self.record(white, list, w, dw);
        self.record(black, list, b, db);
        [
            PlayerResult { name: white.to_owned(), rating: Some(w.rating), diff: Some(dw) },
            PlayerResult { name: black.to_owned(), rating: Some(b.rating), diff: Some(db) },
        ]
    }

    /// Rate a game against a fixed rating (the engine): only the player's
    /// changes. `score` is the player's.
    pub fn rate_against(&mut self, name: &str, list: List, opponent: i32, score: f64) -> PlayerResult {
        let e = self.get(name, list);
        let d = change(&e, opponent, score);
        self.record(name, list, e, d);
        PlayerResult { name: name.to_owned(), rating: Some(e.rating), diff: Some(d) }
    }

    fn record(&mut self, name: &str, list: List, e: Entry, diff: i32) {
        let rating = e.rating + diff;
        self.put(name, list, Entry { rating, games: e.games + 1, peak: e.peak.max(rating) });
    }

    /// Write the file (best effort: a failure is logged, not fatal).
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let bytes = serde_json::to_vec_pretty(&self.players).map_err(std::io::Error::other)?;
            std::fs::write(path, bytes)
        };
        if let Err(e) = write() {
            crate::log::line(
                crate::log::Source::Game,
                format!("saving the ratings to {}: {e}", path.display()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fide_elo() {
        let new = Entry::default();
        // Even players: a win is worth K/2.
        assert_eq!(change(&new, 1500, 1.0), 20);
        assert_eq!(change(&new, 1500, 0.5), 0);
        // 400+ apart is capped: the favourite gains little.
        assert_eq!(change(&Entry { rating: 2000, games: 50, peak: 2000 }, 1000, 1.0), 2);
        assert_eq!(change(&Entry { rating: 2500, games: 50, peak: 2500 }, 2500, 1.0), 5);
        assert_eq!(engine_rating(1), 800);
        assert_eq!(engine_rating(8), 2200);
    }

    #[test]
    fn lists_follow_fide() {
        let t = |minutes, increment_secs| Some(TimeControl { minutes, increment_secs });
        assert_eq!(List::of(t(5, 3)), List::Blitz);
        assert_eq!(List::of(t(10, 0)), List::Blitz);
        assert_eq!(List::of(t(10, 5)), List::Rapid);
        assert_eq!(List::of(t(90, 30)), List::Classical);
        assert_eq!(List::of(None), List::Classical);
    }

    #[test]
    fn a_two_player_game_is_kept() {
        let dir = std::env::temp_dir().join(format!("oc-ratings-{}", std::process::id()));
        let path = dir.join("ratings.json");
        let mut s = Store::at(Some(path.clone()));
        let [w, b] = s.rate_pair("Ann", "Bob", List::Rapid, 0.0);
        assert_eq!((w.diff, b.diff), (Some(-20), Some(20)));
        assert_eq!(b.describe(), "Bob 1500 → 1520 (+20)");
        s.save();
        let again = Store::at(Some(path));
        assert_eq!(again.get("Bob", List::Rapid), Entry { rating: 1520, games: 1, peak: 1520 });
        assert_eq!(again.get("Bob", List::Blitz), Entry::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_finished_games_count() {
        assert_eq!(white_score(Status::Mate, Some(Side::Black), 30), Some(0.0));
        assert_eq!(white_score(Status::Stalemate, None, 30), Some(0.5));
        assert_eq!(white_score(Status::Aborted, None, 30), None);
        assert_eq!(white_score(Status::Resign, Some(Side::White), 1), None);
    }
}
