//! Labelled photos and video frames from a real game.
//!
//! While a game is played on a real board, every photo or video frame of
//! it shows a position we usually already know:
//!
//! - **A connected board** (Chessnut): the board reports every change, so
//!   a frame is stamped with what the board showed at that instant. Frames
//!   taken while the position was changing (a hand over the board, a
//!   capture half made) are skipped, so nothing gets a wrong label.
//! - **An ordinary board**: after each move the player takes a photo or
//!   two; [`Capture::match_photos`] compares the readings with every legal
//!   move from the last position (adding up the evidence from each angle),
//!   and the move the player confirms labels the photos.
//!
//! [`Capture::offer_frame`] decides which frames are worth keeping: those
//! the board reader got wrong (they teach it most), a few it got right per
//! position and camera angle, nothing that looks like something else.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use chess_analysis::photo::{self, ScoredCandidate, SquareProbs};
use chess_core::{Game, STARTPOS_FEN};
use serde::Serialize;

use crate::{MobileError, json};

/// How choosy to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CapturePolicy {
    /// A position counts only once it has been still this long (ms), and a
    /// frame only if it stayed so this long afterwards too.
    pub settle_ms: u32,
    /// Frames the reader got right to keep per position and angle.
    pub max_agreeing: u32,
    /// …and of those, one every this many offered (1: the first ones).
    pub agreeing_every: u32,
    /// Frames the reader got wrong to keep per position and angle.
    pub max_disagreeing: u32,
    /// A reading this many squares off (or more) isn't a mistake to learn
    /// from but something else: a hand over the board, the camera
    /// elsewhere. Skipped.
    pub max_distance: u32,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        CapturePolicy {
            settle_ms: 600,
            max_agreeing: 3,
            agreeing_every: 15,
            max_disagreeing: 12,
            max_distance: 6,
        }
    }
}

/// The default policy (UniFFI records have no defaults of their own).
#[uniffi::export]
pub fn default_capture_policy() -> CapturePolicy {
    CapturePolicy::default()
}

/// What to do with a frame.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FrameDecision {
    /// Keep it (upload it, with `label`).
    pub keep: bool,
    /// The position it shows (FEN placement, White's side), when known.
    pub label: Option<String>,
    /// Squares the reader got wrong (none when there was no reading).
    pub distance: Option<u32>,
    /// Why: "disagrees", "agrees", "sampled out", "enough", "unsettled",
    /// "unknown", "too soon", "not the board".
    pub reason: String,
}

impl FrameDecision {
    fn skip(reason: &str, label: Option<String>, distance: Option<u32>) -> Self {
        FrameDecision { keep: false, label, distance, reason: reason.into() }
    }
}

struct Inner {
    game: Game,
    policy: CapturePolicy,
    /// What the board showed, oldest first: (from ms, placement, still).
    timeline: VecDeque<(i64, String, bool)>,
    /// Frames offered and kept per (placement, angle): (offered agreeing,
    /// kept agreeing, kept disagreeing).
    counts: HashMap<(String, u8), (u32, u32, u32)>,
}

impl Inner {
    fn record(&mut self, at_ms: i64, placement: String, still: bool) {
        if self.timeline.back().is_some_and(|(_, p, s)| *p == placement && *s == still) {
            return;
        }
        self.timeline.push_back((at_ms, placement, still));
        // Frames are offered within seconds: an hour of history is plenty.
        while self.timeline.len() > 2 && self.timeline.front().is_some_and(|(t, _, _)| *t < at_ms - 3_600_000)
        {
            self.timeline.pop_front();
        }
    }

    /// The position shown, still, from `from` to `to` (ms), if one was.
    fn still_between(&self, from: i64, to: i64) -> Option<String> {
        let mut shown: Option<&(i64, String, bool)> = None;
        for e in &self.timeline {
            if e.0 <= from {
                shown = Some(e);
            } else if e.0 <= to {
                // It changed inside the window.
                return None;
            }
        }
        let (_, p, still) = shown?;
        still.then(|| p.clone())
    }
}

/// One real game being captured.
#[derive(uniffi::Object)]
pub struct Capture {
    inner: Mutex<Inner>,
}

/// What [`Capture::match_photos`] found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Matched {
    /// The likeliest moves (empty `uci`: no move made yet), best first.
    pub candidates: Vec<ScoredCandidate>,
    /// The best is clear enough to play without asking.
    pub sure: bool,
}

#[uniffi::export]
impl Capture {
    /// A game from its moves so far (space-separated UCI), from a FEN or
    /// the start.
    #[uniffi::constructor]
    pub fn new(fen: Option<String>, moves: String, policy: CapturePolicy) -> Result<Self, MobileError> {
        let fen = fen.map(|f| f.trim().to_owned()).filter(|f| !f.is_empty() && f != STARTPOS_FEN);
        let game = Game::from_moves(fen.as_deref(), &moves)
            .map_err(|e| MobileError::InvalidPosition { message: format!("{e:?}") })?;
        Ok(Capture {
            inner: Mutex::new(Inner { game, policy, timeline: VecDeque::new(), counts: HashMap::new() }),
        })
    }

    /// What a connected board shows now (FEN placement) and whether it has
    /// settled, as [`crate::ChessnutBoard::board_now`] says. Call on every
    /// change, with the time it was seen.
    pub fn board_state(&self, placement: String, settled: bool, at_ms: i64) {
        let placement = placement.split_whitespace().next().unwrap_or_default().to_owned();
        self.lock().record(at_ms, placement, settled);
    }

    /// A move was made (on the board, confirmed from photos, or the other
    /// side's online). Without a connected board this is also what the
    /// board shows from now on.
    pub fn move_played(&self, uci: String, at_ms: i64, connected_board: bool) -> Result<String, MobileError> {
        let mut i = self.lock();
        let p = i
            .game
            .play_uci(&uci)
            .map_err(|e| MobileError::IllegalMove { message: format!("{uci}: {e:?}") })?;
        if !connected_board {
            let placement = placement_of(&i.game);
            i.record(at_ms, placement, true);
        }
        Ok(p.san)
    }

    /// Take back plies (as the game was).
    pub fn undo(&self, plies: u32, at_ms: i64, connected_board: bool) {
        let mut i = self.lock();
        i.game.undo(plies as usize);
        if !connected_board {
            let placement = placement_of(&i.game);
            i.record(at_ms, placement, true);
        }
    }

    /// The game's position now (FEN placement): the label for photos taken
    /// of it.
    pub fn placement_now(&self) -> String {
        placement_of(&self.lock().game)
    }

    /// A frame (or photo) taken at `at_ms`, from camera `angle`, which the
    /// reader read as `reader_placement` (none if it wasn't run). Offer
    /// frames a moment after taking them (`now_ms` at least `settle_ms`
    /// later), so a change just after is seen too.
    pub fn offer_frame(
        &self,
        at_ms: i64,
        now_ms: i64,
        angle: u8,
        reader_placement: Option<String>,
    ) -> FrameDecision {
        let mut i = self.lock();
        let settle = i64::from(i.policy.settle_ms);
        if now_ms < at_ms + settle {
            return FrameDecision::skip("too soon", None, None);
        }
        if i.timeline.is_empty() {
            return FrameDecision::skip("unknown", None, None);
        }
        let Some(label) = i.still_between(at_ms - settle, at_ms + settle) else {
            return FrameDecision::skip("unsettled", None, None);
        };
        let policy = i.policy;
        let distance = reader_placement.as_deref().and_then(|r| {
            let seen = photo::parse_placement(r)?;
            let truth = photo::parse_placement(&label)?;
            // Either way round: the camera may face the board from Black's side.
            u32::try_from(photo::distance(&truth, &seen).min(photo::distance(&truth, &photo::rotated(&seen))))
                .ok()
        });
        if distance.is_some_and(|d| d >= policy.max_distance) {
            return FrameDecision::skip("not the board", Some(label), distance);
        }
        let c = i.counts.entry((label.clone(), angle)).or_insert((0, 0, 0));
        if distance.is_some_and(|d| d > 0) {
            if c.2 >= policy.max_disagreeing {
                return FrameDecision::skip("enough", Some(label), distance);
            }
            c.2 += 1;
            return FrameDecision { keep: true, label: Some(label), distance, reason: "disagrees".into() };
        }
        c.0 += 1;
        if c.1 >= policy.max_agreeing {
            return FrameDecision::skip("enough", Some(label), distance);
        }
        if !(c.0 - 1).is_multiple_of(policy.agreeing_every.max(1)) {
            return FrameDecision::skip("sampled out", Some(label), distance);
        }
        c.1 += 1;
        FrameDecision { keep: true, label: Some(label), distance, reason: "agrees".into() }
    }

    /// For an ordinary board: the readings of one or more photos of it
    /// (each `squares` from the board reader: 64 arrays of 13), matched
    /// against the position now and every legal move from it, the evidence
    /// of the photos added up. JSON `{candidates: [top five], sure}`.
    pub fn match_photos(&self, readings: Vec<String>) -> Result<String, MobileError> {
        let i = self.lock();
        let mut combined: HashMap<String, ScoredCandidate> = HashMap::new();
        let mut used = 0;
        for r in &readings {
            let Some(probs) = squares(r) else { continue };
            used += 1;
            for c in photo::match_after_move_probs(i.game.board(), &probs) {
                combined
                    .entry(c.uci.clone())
                    .and_modify(|e| {
                        e.log_likelihood += c.log_likelihood;
                        e.distance = e.distance.min(c.distance);
                    })
                    .or_insert(c);
            }
        }
        if used == 0 {
            return Err(MobileError::Vision {
                message: "no readable photo (squares are 64 arrays of 13)".into(),
            });
        }
        let mut all: Vec<ScoredCandidate> = combined.into_values().collect();
        all.sort_by(|a, b| b.log_likelihood.total_cmp(&a.log_likelihood));
        let top = all.first().map_or(0.0, |c| c.log_likelihood);
        let total: f64 = all.iter().map(|c| (c.log_likelihood - top).exp()).sum();
        for c in &mut all {
            c.probability = (c.log_likelihood - top).exp() / total;
        }
        all.truncate(5);
        let sure = all.first().is_some_and(|c| c.probability >= 0.9 && c.distance <= 1);
        Ok(json(&Matched { candidates: all, sure }))
    }
}

impl Capture {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn placement_of(game: &Game) -> String {
    game.fen().split_whitespace().next().unwrap_or_default().to_owned()
}

fn squares(reading: &str) -> Option<SquareProbs> {
    let v: serde_json::Value = serde_json::from_str(reading).ok()?;
    // A whole reading ({"squares": …}) or just its squares.
    let sq = if v.is_object() { v.get("squares")?.clone() } else { v };
    let rows: Vec<[f32; 13]> = serde_json::from_value(sq).ok()?;
    SquareProbs::try_from(rows.as_slice()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";
    const E4: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR";

    fn cap() -> Capture {
        Capture::new(None, String::new(), CapturePolicy { agreeing_every: 2, ..CapturePolicy::default() })
            .unwrap()
    }

    #[test]
    fn frames_get_the_boards_label_and_unsettled_ones_none() {
        let c = cap();
        assert_eq!(c.offer_frame(500, 5_000, 0, None).reason, "unknown");
        c.board_state(START.into(), true, 0);
        c.board_state(START.into(), false, 10_000); // a hand comes in
        c.board_state(E4.into(), true, 10_500);
        let d = c.offer_frame(2_000, 20_000, 0, Some(START.into()));
        assert_eq!((d.keep, d.label.as_deref(), d.reason.as_str()), (true, Some(START), "agrees"));
        assert_eq!(c.offer_frame(9_700, 20_000, 0, None).reason, "unsettled", "just before the change");
        assert_eq!(c.offer_frame(10_200, 20_000, 0, None).reason, "unsettled", "during it");
        assert_eq!(c.offer_frame(12_000, 12_100, 0, None).reason, "too soon");
        let d = c.offer_frame(12_000, 20_000, 0, Some(START.into()));
        assert_eq!(
            (d.keep, d.label.as_deref(), d.distance),
            (true, Some(E4), Some(2)),
            "the reader missed e4"
        );
    }

    #[test]
    fn keeps_mistakes_and_a_sample_of_the_rest() {
        let c = cap();
        c.board_state(START.into(), true, 0);
        let kept: Vec<bool> =
            (0..10).map(|n| c.offer_frame(1_000 + n, 9_000, 0, Some(START.into())).keep).collect();
        assert_eq!(kept.iter().filter(|k| **k).count(), 3, "max_agreeing, every second: {kept:?}");
        assert!(c.offer_frame(1_000, 9_000, 1, Some(START.into())).keep, "another angle starts again");
        let far = "8/8/8/8/8/8/8/8";
        assert_eq!(c.offer_frame(1_000, 9_000, 0, Some(far.into())).reason, "not the board");
        // Reversing a FEN placement turns the board through 180°.
        let flipped: String = START.chars().rev().collect();
        let d = c.offer_frame(1_000, 9_000, 0, Some(flipped));
        assert_ne!(d.reason, "not the board", "a board seen from Black's side is still the board: {d:?}");
    }

    #[test]
    fn photos_of_an_ordinary_board_find_the_move() {
        let c = cap();
        // A reading that is sure of the position after 1.e4.
        let grid = photo::parse_placement(E4).unwrap();
        let probs: Vec<Vec<f32>> = grid
            .iter()
            .map(|cell| {
                let k = photo::class_of(*cell);
                (0..13).map(|j| if j == k { 0.97 } else { 0.0025 }).collect()
            })
            .collect();
        let reading = serde_json::json!({ "squares": probs }).to_string();
        let m: serde_json::Value =
            serde_json::from_str(&c.match_photos(vec![reading.clone(), reading]).unwrap()).unwrap();
        assert_eq!(m["candidates"][0]["uci"], "e2e4");
        assert_eq!(m["sure"], true);
        assert!(c.match_photos(vec!["nonsense".into()]).is_err());
        assert_eq!(c.move_played("e2e4".into(), 3_000, false).unwrap(), "e4");
        assert_eq!(c.placement_now(), E4);
        let d = c.offer_frame(4_000, 9_000, 0, Some(E4.into()));
        assert_eq!(d.label.as_deref(), Some(E4), "a confirmed move labels what follows");
    }
}
