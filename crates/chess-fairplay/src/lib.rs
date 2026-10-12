//! Post-game fair-play screening.
//!
//! An engine evaluates every position of a finished game; from that come,
//! per player, the signals cheat detection research (and Lichess's own
//! accuracy figures) rely on:
//!
//! * **accuracy / ACPL** — how close each move came to the engine's best,
//!   on Lichess's win-percentage scale;
//! * **engine-match rate** — how often the move played was the engine's
//!   first (or top three) choice, leaving out the opening, forced moves
//!   and positions already decided;
//! * **move-time patterns** — engine users tend to spend about the same
//!   time on every move, and the same time whether the position is quiet
//!   or critical; people think longer when it matters.
//!
//! * **app-engine fingerprint** — where Stockfish and the training app's
//!   own engine disagree on the best move, which one did the player pick?
//!   A person playing their own moves picks either, or neither; someone
//!   copying the app's suggestions picks the app's move almost every time.
//!   Because the app's engine is weaker and different, this catches
//!   assistance that plain engine-match rates miss ([`app_suggestions`],
//!   [`app_match`]).
//!
//! [`assess`] folds these into a 0–100 screening score. It is a prompt for
//! a person to look at the game, never a verdict: strong players have
//! accurate games, and the thresholds must be tuned on your own players.
//! Bot accounts aren't screened (they are engines, openly).

use std::fmt;

use chess_core::{Game, Side};
use chess_engine::{Engine, Limit, Score};
use serde::{Deserialize, Serialize};

const MATE_CP: i32 = 1000;

#[derive(Debug)]
pub enum Error {
    /// The moves don't replay (shouldn't happen for a stored game).
    Game(String),
    Engine(chess_engine::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Game(e) => write!(f, "replaying the game: {e}"),
            Error::Engine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<chess_engine::Error> for Error {
    fn from(e: chess_engine::Error) -> Self {
        Error::Engine(e)
    }
}

/// Lichess's win percentage for a centipawn score (side to move's view).
pub fn win_percent(cp: i32) -> f64 {
    let cp = f64::from(cp.clamp(-MATE_CP, MATE_CP));
    50.0 + 50.0 * (2.0 / (1.0 + (-0.003_682_08 * cp).exp()) - 1.0)
}

/// Lichess's per-move accuracy from the mover's win percentage before and
/// after the move.
pub fn move_accuracy(win_before: f64, win_after: f64) -> f64 {
    let drop = (win_before - win_after).max(0.0);
    (103.166_810_071_164_9 * (-0.043_544_153_867_539_51 * drop).exp() - 3.166_924_740_191_411 + 1.0)
        .clamp(0.0, 100.0)
}

/// How to analyse.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisConfig {
    pub limit: Limit,
    /// Lines per position (3: the engine-match top-3 rate needs them).
    pub multipv: u32,
    /// Plies at the start that aren't screened (opening theory).
    pub skip_plies: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        AnalysisConfig { limit: Limit::Depth(14), multipv: 3, skip_plies: 16 }
    }
}

/// One move, evaluated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveEval {
    pub ply: usize,
    pub side: Side,
    pub uci: String,
    /// The engine's best move here (absent in reports stored before it
    /// was recorded).
    #[serde(default)]
    pub best: Option<String>,
    /// The engine's best, mover's view, centipawns (mates ±1000).
    pub best_cp: i32,
    /// What the move played leads to, mover's view.
    pub played_cp: i32,
    pub cp_loss: i32,
    pub win_before: f64,
    pub win_after: f64,
    pub accuracy: f64,
    /// Where the move played ranked among the engine's lines (0 = best),
    /// if it was one of them.
    pub rank: Option<usize>,
    /// The only legal move.
    pub forced: bool,
    /// Win-percentage gap between the engine's best and second-best
    /// moves: high when only one move holds the position.
    pub criticality: f64,
    pub time_ms: Option<i64>,
}

/// Evaluate every move of a game. `move_times_ms` holds the time each
/// ply took (empty when unknown).
pub async fn evaluate_game<E: Engine>(
    engine: &mut E,
    initial_fen: Option<&str>,
    moves: &[String],
    move_times_ms: &[i64],
    cfg: &AnalysisConfig,
) -> Result<Vec<MoveEval>, Error> {
    let mut game = Game::new(initial_fen).map_err(|e| Error::Game(e.to_string()))?;
    let first = game.first_to_move();
    let fen = game.initial_fen_or_start();
    engine.new_game().await?;
    // One search per position: evals[i] is the position before ply i (and
    // evals[n] the final position), each from its side to move's view.
    let mut evals: Vec<(i32, Vec<(String, i32)>)> = Vec::with_capacity(moves.len() + 1);
    for i in 0..=moves.len() {
        if i > 0 {
            game.play_uci(&moves[i - 1]).map_err(|e| Error::Game(e.to_string()))?;
        }
        if let Some(o) = game.outcome() {
            // Terminal: mated (the side to move lost) or a draw.
            let cp = if o.winner.is_some() { -MATE_CP } else { 0 };
            evals.push((cp, Vec::new()));
            continue;
        }
        let a = engine.analyse(&fen, &moves[..i], cfg.limit, cfg.multipv).await?;
        let top: Vec<(String, i32)> =
            a.lines.iter().filter_map(|l| Some((l.pv.first()?.clone(), l.score.to_cp(MATE_CP)))).collect();
        let best = top.first().map_or(0, |t| t.1);
        evals.push((best, top));
    }
    let mut out = Vec::with_capacity(moves.len());
    for (i, uci) in moves.iter().enumerate() {
        let (best, top) = &evals[i];
        let played = Score::Cp(evals[i + 1].0).negate().to_cp(MATE_CP);
        let win_before = win_percent(*best);
        let win_after = win_percent(played);
        let criticality = match (top.first(), top.get(1)) {
            (Some(a), Some(b)) => (win_percent(a.1) - win_percent(b.1)).max(0.0),
            _ => 0.0,
        };
        out.push(MoveEval {
            ply: i,
            side: Side::at_ply(first, i),
            uci: uci.clone(),
            best: top.first().map(|t| t.0.clone()),
            best_cp: *best,
            played_cp: played,
            cp_loss: (best - played).clamp(0, MATE_CP),
            win_before,
            win_after,
            accuracy: move_accuracy(win_before, win_after),
            rank: top.iter().position(|(m, _)| m == uci),
            forced: top.len() == 1 && cfg.multipv > 1,
            criticality,
            time_ms: move_times_ms.get(i).copied(),
        });
    }
    Ok(out)
}

/// One player's numbers for one game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Features {
    pub side: Side,
    /// Moves screened (after the opening, not forced, not already decided).
    pub moves: usize,
    pub acpl: f64,
    /// Lichess-style game accuracy, 0–100, over every move of the side.
    pub accuracy: f64,
    pub top1: f64,
    pub top3: f64,
    pub inaccuracies: usize,
    pub mistakes: usize,
    pub blunders: usize,
    pub mean_time_ms: Option<f64>,
    /// Spread of move times relative to their mean (low: metronomic).
    pub time_cv: Option<f64>,
    /// Correlation of move time with how critical the position was (people
    /// think longer when it matters; a value near zero or below is odd).
    pub time_criticality_corr: Option<f64>,
}

/// A position where any reasonable move wins (or loses): not informative.
fn decided(e: &MoveEval) -> bool {
    e.best_cp.abs() >= 600 && e.played_cp.abs() >= 600 && e.best_cp.signum() == e.played_cp.signum()
}

pub fn features(evals: &[MoveEval], side: Side, skip_plies: usize) -> Features {
    let all: Vec<&MoveEval> = evals.iter().filter(|e| e.side == side).collect();
    let screened: Vec<&MoveEval> =
        all.iter().copied().filter(|e| e.ply >= skip_plies && !e.forced && !decided(e)).collect();
    let n = screened.len();
    let frac = |pred: &dyn Fn(&&MoveEval) -> bool| {
        if n == 0 { 0.0 } else { screened.iter().filter(|e| pred(e)).count() as f64 / n as f64 }
    };
    let drop = |e: &&MoveEval| e.win_before - e.win_after;
    let times: Vec<(f64, f64)> =
        screened.iter().filter_map(|e| e.time_ms.map(|t| (t as f64, e.criticality))).collect();
    let (mean_time, cv, corr) = if times.len() >= 10 {
        let ts: Vec<f64> = times.iter().map(|t| t.0).collect();
        let cs: Vec<f64> = times.iter().map(|t| t.1).collect();
        let m = mean(&ts);
        (Some(m), (m > 0.0).then(|| stdev(&ts) / m), pearson(&ts, &cs))
    } else {
        (None, None, None)
    };
    Features {
        side,
        moves: n,
        acpl: if n == 0 {
            0.0
        } else {
            screened.iter().map(|e| f64::from(e.cp_loss)).sum::<f64>() / n as f64
        },
        accuracy: game_accuracy(&all),
        top1: frac(&|e| e.rank == Some(0)),
        top3: frac(&|e| e.rank.is_some_and(|r| r < 3)),
        inaccuracies: all.iter().filter(|e| (5.0..10.0).contains(&drop(e))).count(),
        mistakes: all.iter().filter(|e| (10.0..15.0).contains(&drop(e))).count(),
        blunders: all.iter().filter(|e| drop(e) >= 15.0).count(),
        mean_time_ms: mean_time,
        time_cv: cv,
        time_criticality_corr: corr,
    }
}

/// Lichess's game accuracy: the mean of a volatility-weighted mean and the
/// harmonic mean of move accuracies.
fn game_accuracy(moves: &[&MoveEval]) -> f64 {
    if moves.is_empty() {
        return 0.0;
    }
    let acc: Vec<f64> = moves.iter().map(|e| e.accuracy).collect();
    let wins: Vec<f64> = moves.iter().map(|e| e.win_before).collect();
    let window = (moves.len() / 10).clamp(2, 8);
    let weights: Vec<f64> = (0..moves.len())
        .map(|i| {
            let lo = i.saturating_sub(window / 2);
            let hi = (lo + window).min(wins.len());
            stdev(&wins[lo..hi]).clamp(0.5, 12.0)
        })
        .collect();
    let wsum: f64 = weights.iter().sum();
    let weighted = acc.iter().zip(&weights).map(|(a, w)| a * w).sum::<f64>() / wsum;
    let harmonic = acc.len() as f64 / acc.iter().map(|a| 1.0 / a.max(1.0)).sum::<f64>();
    f64::midpoint(weighted, harmonic)
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() { 0.0 } else { xs.iter().sum::<f64>() / xs.len() as f64 }
}

fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64).sqrt()
}

fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let (ma, mb) = (mean(a), mean(b));
    let (sa, sb) = (stdev(a), stdev(b));
    if sa == 0.0 || sb == 0.0 {
        return None;
    }
    let cov = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum::<f64>() / a.len() as f64;
    Some(cov / (sa * sb))
}

/// How one signal feeds the score: a logistic curve that reaches 0.5 at
/// `centre`, with `scale` setting how sharp the step is, weighted into the
/// total. Which side of the centre counts as engine-like is fixed per
/// signal (high accuracy, low centipawn loss, …).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub centre: f64,
    pub scale: f64,
    pub weight: f64,
}

impl Signal {
    pub const fn new(centre: f64, scale: f64, weight: f64) -> Self {
        Signal { centre, scale, weight }
    }

    /// Engine-like when the value is above the centre.
    fn high(&self, x: f64) -> f64 {
        sigmoid((x - self.centre) / self.scale.max(f64::EPSILON))
    }

    /// Engine-like when the value is below the centre.
    fn low(&self, x: f64) -> f64 {
        sigmoid((self.centre - x) / self.scale.max(f64::EPSILON))
    }
}

/// How the app-engine fingerprint ([`AppMatch`]) feeds the score.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Fewer decided disagreements than this: the fingerprint is ignored.
    pub min_decided: usize,
    /// The share of disagreements going the app's way that scores 0.5 …
    pub centre: f64,
    /// … and how sharp that step is.
    pub scale: f64,
    /// How much of the final score the fingerprint makes up (0–1).
    pub blend: f64,
    /// A game is flagged outright when the app's share is at least this …
    pub flag_rate: f64,
    /// … over at least this many picks of the app's move.
    pub flag_min_picked: usize,
}

/// Everything that turns a player's [`Features`] into a screening score
/// and a flag: the mechanism is here, the numbers are data.
///
/// The numbers are what someone dodging screening would want to know, and
/// they only mean anything once tuned on your own players, so a
/// deployment should keep its tuned model private and load it at run time
/// (the server reads `CHESS_FAIRPLAY_MODEL`). [`Model::default`] is an
/// untuned example for development and tests, not for deciding who to
/// look at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Fewer screened moves than this: never flagged (too little evidence).
    pub min_moves: usize,
    /// Score (0–100) at or above which the game is flagged for review.
    pub flag_score: f64,
    /// Average move accuracy, %: engine-like when high.
    pub accuracy: Signal,
    /// Share of moves that were the engine's first choice: engine-like when high.
    pub top1: Signal,
    /// Average centipawn loss: engine-like when low.
    pub acpl: Signal,
    /// Variation of move times (coefficient of variation): engine-like when low.
    pub time_cv: Signal,
    /// Correlation of move time with how critical the position was:
    /// engine-like when low (people think longer when it matters).
    pub time_criticality: Signal,
    pub fingerprint: Fingerprint,
}

impl Default for Model {
    /// An untuned example. Deployments load their own (see [`Model`]).
    fn default() -> Self {
        Model {
            min_moves: 20,
            flag_score: 80.0,
            accuracy: Signal::new(90.0, 4.0, 0.25),
            top1: Signal::new(0.6, 0.1, 0.25),
            acpl: Signal::new(20.0, 5.0, 0.2),
            time_cv: Signal::new(0.5, 0.15, 0.15),
            time_criticality: Signal::new(0.0, 0.15, 0.15),
            fingerprint: Fingerprint {
                min_decided: 8,
                centre: 0.8,
                scale: 0.1,
                blend: 0.2,
                flag_rate: 0.95,
                flag_min_picked: 10,
            },
        }
    }
}

impl Model {
    /// Checks a loaded model is usable.
    pub fn validate(&self) -> Result<(), String> {
        let signals = [
            ("accuracy", self.accuracy),
            ("top1", self.top1),
            ("acpl", self.acpl),
            ("time_cv", self.time_cv),
            ("time_criticality", self.time_criticality),
        ];
        for (name, s) in signals {
            if !(s.scale > 0.0 && s.scale.is_finite()) {
                return Err(format!("{name}.scale must be positive"));
            }
            if !(s.weight >= 0.0 && s.weight.is_finite()) {
                return Err(format!("{name}.weight must be zero or more"));
            }
        }
        if signals.iter().map(|(_, s)| s.weight).sum::<f64>() <= 0.0 {
            return Err("at least one signal needs a weight".into());
        }
        let fp = &self.fingerprint;
        if !(fp.scale > 0.0 && fp.scale.is_finite()) {
            return Err("fingerprint.scale must be positive".into());
        }
        if !(0.0..=1.0).contains(&fp.blend) {
            return Err("fingerprint.blend must be between 0 and 1".into());
        }
        if !(0.0..=100.0).contains(&self.flag_score) {
            return Err("flag_score must be between 0 and 100".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    /// 0–100: how engine-like the play looks.
    pub score: f64,
    pub flagged: bool,
    /// What drove the score, in words for the reviewer.
    pub reasons: Vec<String>,
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// The move the in-app engine would have suggested before each ply
/// (standard UCI), with the settings the app's training view uses, so the
/// suggestions match what a player using the app would have seen.
/// Deterministic: node-limited, no clock. `net` is the network the app
/// ships, if it ships one.
pub fn app_suggestions(
    initial_fen: Option<&str>,
    moves: &[String],
    depth: u8,
    nodes: u64,
    net: Option<std::sync::Arc<chess_analysis::nnue::Network>>,
) -> Result<Vec<Option<String>>, Error> {
    use chess_analysis::search::{Limits, Searcher};
    let mut game = Game::new(initial_fen).map_err(|e| Error::Game(e.to_string()))?;
    let mut searcher = Searcher::new(1 << 18);
    searcher.set_network(net);
    let mut out = Vec::with_capacity(moves.len());
    for m in moves {
        let suggestion = if game.outcome().is_some() {
            None
        } else {
            let board = game.board().clone();
            let r = searcher.search(
                &board,
                &chess_analysis::history_hashes(&game),
                Limits { depth, nodes, multipv: 1, time_ms: None },
            );
            r.lines.first().and_then(|l| l.moves.first()).map(|&mv| chess_analysis::std_uci(&board, mv))
        };
        out.push(suggestion);
        game.play_uci(m).map_err(|e| Error::Game(e.to_string()))?;
    }
    Ok(out)
}

/// How a player chose where Stockfish and the app's engine disagreed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct AppMatch {
    /// Screened positions where the two engines' best moves differ.
    pub disagreements: usize,
    /// Of those, how often the player played the app's move …
    pub picked_app: usize,
    /// … and how often Stockfish's.
    pub picked_reference: usize,
}

impl AppMatch {
    /// Share of the decided disagreements that went the app's way.
    pub fn rate(&self) -> Option<f64> {
        let n = self.picked_app + self.picked_reference;
        (n > 0).then(|| self.picked_app as f64 / n as f64)
    }
}

/// Compare one side's moves with the app's suggestions (see
/// [`app_suggestions`]), on the same screened moves as [`features`].
pub fn app_match(evals: &[MoveEval], app: &[Option<String>], side: Side, skip_plies: usize) -> AppMatch {
    let mut m = AppMatch::default();
    for e in evals.iter().filter(|e| e.side == side && e.ply >= skip_plies && !e.forced && !decided(e)) {
        let (Some(a), Some(b)) = (app.get(e.ply).and_then(Option::as_ref), e.best.as_ref()) else { continue };
        if a == b {
            continue;
        }
        m.disagreements += 1;
        if &e.uci == a {
            m.picked_app += 1;
        } else if &e.uci == b {
            m.picked_reference += 1;
        }
    }
    m
}

/// Fold one player's features into a screening score.
pub fn assess(f: &Features, m: &Model) -> Assessment {
    assess_with_app(f, None, m)
}

/// [`assess`], also weighing the app-engine fingerprint when there is one.
pub fn assess_with_app(f: &Features, app: Option<&AppMatch>, m: &Model) -> Assessment {
    let accuracy = m.accuracy.high(f.accuracy);
    let top1 = m.top1.high(f.top1);
    let low_loss = m.acpl.low(f.acpl);
    // Without enough timed moves the time signals say nothing either way.
    let uniform = f.time_cv.map_or(0.5, |cv| m.time_cv.low(cv));
    let flat = f.time_criticality_corr.map_or(0.5, |c| m.time_criticality.low(c));
    let weights =
        m.accuracy.weight + m.top1.weight + m.acpl.weight + m.time_cv.weight + m.time_criticality.weight;
    let weighted = m.accuracy.weight * accuracy
        + m.top1.weight * top1
        + m.acpl.weight * low_loss
        + m.time_cv.weight * uniform
        + m.time_criticality.weight * flat;
    let score = if weights > 0.0 { 100.0 * weighted / weights } else { 0.0 };
    let mut reasons = Vec::new();
    if accuracy > 0.5 {
        reasons.push(format!("accuracy {:.0}%", f.accuracy));
    }
    if top1 > 0.5 {
        reasons.push(format!("engine's first choice {:.0}% of {} moves", f.top1 * 100.0, f.moves));
    }
    if low_loss > 0.5 {
        reasons.push(format!("average centipawn loss {:.0}", f.acpl));
    }
    if let Some(cv) = f.time_cv.filter(|_| uniform > 0.5) {
        reasons.push(format!("very even move times (variation {cv:.2})"));
    }
    if let Some(c) = f.time_criticality_corr.filter(|_| flat > 0.5) {
        reasons.push(format!("no more time on critical moves (correlation {c:.2})"));
    }
    // The fingerprint needs a handful of disagreements to mean anything.
    let mut score = score;
    let mut fingerprint = false;
    let fp = &m.fingerprint;
    if let Some(a) = app.filter(|a| a.picked_app + a.picked_reference >= fp.min_decided)
        && let Some(rate) = a.rate()
    {
        let s = sigmoid((rate - fp.centre) / fp.scale.max(f64::EPSILON));
        let blend = fp.blend.clamp(0.0, 1.0);
        score = (1.0 - blend) * score + 100.0 * blend * s;
        if s > 0.5 {
            reasons.push(format!(
                "chose the training app's suggestion over Stockfish's in {} of {} positions where they differed",
                a.picked_app,
                a.picked_app + a.picked_reference
            ));
        }
        fingerprint = rate >= fp.flag_rate && a.picked_app >= fp.flag_min_picked;
    }
    let flagged = f.moves >= m.min_moves && (score >= m.flag_score || fingerprint);
    if f.moves < m.min_moves {
        reasons.push(format!("only {} moves screened: not enough to judge", f.moves));
    }
    Assessment { score: (score * 10.0).round() / 10.0, flagged, reasons }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_engine::{Analysis, Line};

    #[test]
    fn app_fingerprint_flags_copying_the_app() {
        // 30 screened moves; in 12 the engines disagree.
        let mut evals = Vec::new();
        let mut app = Vec::new();
        for i in 0..60_usize {
            let side = if i.is_multiple_of(2) { Side::White } else { Side::Black };
            let mut e = eval(i, side, 10, Some(1), 5.0, Some(4000 + (i as i64 * 37) % 2000));
            e.best = Some("d2d4".into());
            let disagree = i % 5 == 0;
            app.push(Some(if disagree { "c2c4".to_string() } else { "d2d4".to_string() }));
            // White copies the app; Black plays its own moves.
            e.uci = match (side, disagree) {
                (Side::White, true) => "c2c4".into(),
                (Side::White, false) => "d2d4".into(),
                (_, true) if i % 10 == 0 => "d2d4".into(),
                _ => "g1f3".into(),
            };
            evals.push(e);
        }
        let w = app_match(&evals, &app, Side::White, 0);
        let b = app_match(&evals, &app, Side::Black, 0);
        assert_eq!(w.picked_reference, 0);
        assert!(w.picked_app >= 6 && w.rate() == Some(1.0), "{w:?}");
        assert_eq!(b.picked_app, 0, "{b:?}");
        let t = Model {
            min_moves: 5,
            fingerprint: Fingerprint { min_decided: 5, ..Model::default().fingerprint },
            ..Model::default()
        };
        let fw = features(&evals, Side::White, 0);
        let plain = assess(&fw, &t);
        let with_app = assess_with_app(&fw, Some(&w), &t);
        assert!(with_app.score > plain.score);
        assert!(with_app.reasons.iter().any(|r| r.contains("training app")));
    }

    #[test]
    fn app_suggestions_are_legal_and_reproducible() {
        let moves: Vec<String> = ["e2e4", "e7e5", "g1f3"].iter().map(|s| (*s).to_string()).collect();
        let a = app_suggestions(None, &moves, 3, 20_000, None).unwrap();
        let b = app_suggestions(None, &moves, 3, 20_000, None).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert!(a.iter().all(Option::is_some));
    }

    #[test]
    fn win_percent_matches_lichess() {
        assert!((win_percent(0) - 50.0).abs() < 1e-9);
        assert!((win_percent(100) - 59.1).abs() < 0.1);
        assert!(win_percent(5000) < 100.0 && win_percent(5000) > 95.0);
        assert!((win_percent(-100) + win_percent(100) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn move_accuracy_scale() {
        assert!((move_accuracy(60.0, 60.0) - 100.0).abs() < 1e-9, "a perfect move is 100");
        assert!(move_accuracy(60.0, 70.0) > 99.0, "gaining is perfect too");
        let a = move_accuracy(60.0, 40.0);
        assert!(a > 30.0 && a < 50.0, "{a}");
        assert!(move_accuracy(100.0, 0.0) < 1.0);
    }

    fn eval(ply: usize, side: Side, loss: i32, rank: Option<usize>, crit: f64, t: Option<i64>) -> MoveEval {
        let best = 50;
        let played = best - loss;
        let (wb, wa) = (win_percent(best), win_percent(played));
        MoveEval {
            ply,
            side,
            uci: "e2e4".into(),
            best: Some("e2e4".into()),
            best_cp: best,
            played_cp: played,
            cp_loss: loss,
            win_before: wb,
            win_after: wa,
            accuracy: move_accuracy(wb, wa),
            rank,
            forced: false,
            criticality: crit,
            time_ms: t,
        }
    }

    /// A metronomic perfect player against an erratic human.
    fn sample() -> Vec<MoveEval> {
        let mut v = Vec::new();
        for i in 0..80_usize {
            let side = if i.is_multiple_of(2) { Side::White } else { Side::Black };
            let crit = (i % 7) as f64 * 3.0;
            if side == Side::White {
                v.push(eval(i, side, 0, Some(0), crit, Some(3_000 + (i as i64 % 3) * 100)));
            } else {
                let loss = [0, 40, 10, 120, 25][i % 5];
                let rank = [Some(0), Some(2), Some(1), None, Some(0)][i % 5];
                v.push(eval(
                    i,
                    side,
                    loss,
                    rank,
                    crit,
                    Some(1_000 + (crit as i64) * 900 + (i as i64 % 4) * 700),
                ));
            }
        }
        v
    }

    #[test]
    fn features_separate_engine_like_and_human_play() {
        let evals = sample();
        let w = features(&evals, Side::White, 16);
        let b = features(&evals, Side::Black, 16);
        assert_eq!(w.moves, 32);
        assert!((w.top1 - 1.0).abs() < 1e-9 && w.acpl == 0.0);
        assert!(w.accuracy > 99.0, "{w:?}");
        assert!(w.time_cv.unwrap() < 0.1);
        assert!(b.top1 < 0.5 && b.acpl > 30.0);
        assert!(b.time_criticality_corr.unwrap() > 0.5, "{b:?}");
        assert!(b.blunders > 0 || b.mistakes > 0 || b.inaccuracies > 0);

        let t = Model::default();
        let aw = assess(&w, &t);
        let ab = assess(&b, &t);
        assert!(aw.flagged, "{aw:?}");
        assert!(aw.score > 85.0);
        assert!(!ab.flagged && ab.score < 50.0, "{ab:?}");
        assert!(aw.reasons.iter().any(|r| r.contains("first choice")));
    }

    #[test]
    fn short_games_are_never_flagged() {
        let evals: Vec<MoveEval> = sample().into_iter().take(30).collect();
        let w = features(&evals, Side::White, 16);
        let a = assess(&w, &Model::default());
        assert!(!a.flagged);
        assert!(a.reasons.iter().any(|r| r.contains("not enough")));
        assert_eq!(w.time_cv, None, "too few timed moves for time features");
    }

    /// A scripted engine: the same lines for every position.
    struct Scripted;

    impl Engine for Scripted {
        async fn analyse(
            &mut self,
            _fen: &str,
            moves: &[String],
            _limit: Limit,
            _multipv: u32,
        ) -> chess_engine::Result<Analysis> {
            // After 1.e4 the "engine" likes e7e5 and is level; otherwise e2e4.
            let (best, second) =
                if moves.len().is_multiple_of(2) { ("e2e4", "d2d4") } else { ("e7e5", "c7c5") };
            Ok(Analysis {
                best_move: Some(best.into()),
                lines: vec![
                    Line { multipv: 1, depth: 10, score: Score::Cp(20), pv: vec![best.into()] },
                    Line { multipv: 2, depth: 10, score: Score::Cp(-80), pv: vec![second.into()] },
                ],
            })
        }

        async fn new_game(&mut self) -> chess_engine::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn evaluate_game_ranks_moves_and_scores_mate() {
        let moves: Vec<String> = ["f2f3", "e7e5", "g2g4", "d8h4"].iter().map(|s| (*s).to_owned()).collect();
        let ev =
            evaluate_game(&mut Scripted, None, &moves, &[500, 400, 300, 200], &AnalysisConfig::default())
                .await
                .unwrap();
        assert_eq!(ev.len(), 4);
        assert_eq!(ev[0].rank, None, "f3 isn't among the lines");
        assert_eq!(ev[1].rank, Some(0), "e5 is the best line");
        assert_eq!(ev[1].side, Side::Black);
        assert_eq!(ev[3].played_cp, MATE_CP, "Qh4# leads to the opponent being mated");
        assert_eq!(ev[3].time_ms, Some(200));
        assert!(ev[0].criticality > 0.0);
        let json = serde_json::to_string(&ev[0]).unwrap();
        assert!(json.contains("\"side\":\"white\""));
    }

    #[tokio::test]
    async fn real_stockfish_if_available() {
        let Some(sf) = chess_engine::find_stockfish() else {
            return;
        };
        let mut e = chess_engine::UciEngine::spawn(&sf, &[("Threads", "1")]).await.unwrap();
        let moves: Vec<String> =
            ["e2e4", "e7e5", "g1f3", "b8c6", "f1c4", "g8f6"].iter().map(|s| (*s).to_owned()).collect();
        let cfg = AnalysisConfig { limit: Limit::Depth(8), multipv: 3, skip_plies: 0 };
        let ev = evaluate_game(&mut e, None, &moves, &[], &cfg).await.unwrap();
        assert_eq!(ev.len(), 6);
        assert!(ev.iter().all(|m| m.accuracy > 50.0), "book moves are fine: {ev:?}");
        e.quit().await;
    }
}
