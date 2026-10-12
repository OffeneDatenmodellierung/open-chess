//! WebAssembly bindings for the chess PWA.
//!
//! One [`Session`] holds a game, a searcher for analysis and one for the
//! bot. Structured results cross the boundary as JSON strings (parsed with
//! `JSON.parse` on the JS side), which keeps the binding small and the
//! shapes identical to the Rust types' serde output.
//!
//! Build: `wasm-pack build crates/chess-wasm --target web --release`
//! (the `chess-pwa` CI job does this and copies the output into
//! `web/chess/pkg`).

use chess_analysis::search::Searcher;
use chess_analysis::{
    AnalyseOptions, Level, analyse_with, best_move, bot_move, explain_san, glossary, legal_moves, openings,
};
use chess_core::{Game, PgnGame, PgnPlayer, STARTPOS_FEN, Side, Status};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Date, js_name = now)]
    fn date_now() -> f64;
}

/// Milliseconds from the browser's clock, for time-limited searches
/// (always positive and far below 2^53).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn now_ms() -> u64 {
    date_now() as u64
}

fn searcher(entries: usize) -> Searcher {
    let mut s = Searcher::new(entries);
    if cfg!(target_arch = "wasm32") {
        s.set_clock(now_ms);
    }
    s
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn err(e: impl std::fmt::Debug) -> JsError {
    JsError::new(&format!("{e:?}"))
}

#[derive(Serialize)]
struct State<'a> {
    fen: String,
    initial_fen: String,
    turn: &'static str,
    ply: usize,
    uci: Vec<String>,
    san: &'a [String],
    status: &'static str,
    winner: Option<&'static str>,
    check: bool,
    last: Option<String>,
    opening: Option<openings::Opening>,
}

/// A game plus the engines that work on it.
#[wasm_bindgen]
pub struct Session {
    game: Game,
    analysis: Searcher,
    bot: Searcher,
}

#[wasm_bindgen]
impl Session {
    /// A new game from the start, or from a FEN.
    #[wasm_bindgen(constructor)]
    pub fn new(fen: Option<String>) -> Result<Session, JsError> {
        let fen = fen.filter(|f| !f.trim().is_empty() && f.trim() != STARTPOS_FEN);
        Ok(Session {
            game: Game::new(fen.as_deref()).map_err(err)?,
            analysis: searcher(1 << 18),
            bot: searcher(1 << 18),
        })
    }

    /// Replace the game with one replayed from moves (space-separated UCI).
    #[wasm_bindgen(js_name = loadMoves)]
    pub fn load_moves(&mut self, fen: Option<String>, moves: &str) -> Result<(), JsError> {
        let fen = fen.filter(|f| !f.trim().is_empty() && f.trim() != STARTPOS_FEN);
        self.game = Game::from_moves(fen.as_deref(), moves).map_err(err)?;
        Ok(())
    }

    /// The game state as JSON: fen, turn, moves, status, winner…
    pub fn state(&self) -> String {
        let g = &self.game;
        let outcome = g.outcome();
        json(&State {
            fen: g.fen(),
            initial_fen: g.initial_fen_or_start(),
            turn: g.turn().name(),
            ply: g.ply(),
            uci: g.uci_moves(),
            san: g.sans(),
            status: outcome.as_ref().map_or(Status::Started, |o| o.status).name(),
            winner: outcome.and_then(|o| o.winner).map(Side::name),
            check: !g.board().checkers().is_empty(),
            last: g.uci_moves().last().cloned(),
            opening: openings::opening(&g.uci_moves()),
        })
    }

    /// Play a move given as UCI ("e2e4") or SAN ("Nf3", "O-O"). Returns
    /// the move's explanation and patterns as JSON.
    pub fn play(&mut self, mv: &str) -> Result<String, JsError> {
        let board = self.game.board().clone();
        let info = legal_moves(&self.game)
            .into_iter()
            .find(|m| m.explanation.uci == mv.trim())
            .or_else(|| {
                let e = explain_san(&board, mv)?;
                legal_moves(&self.game).into_iter().find(|m| m.explanation.uci == e.uci)
            })
            .ok_or_else(|| JsError::new(&format!("illegal move: {mv}")))?;
        self.game.play_uci(&info.explanation.uci).map_err(err)?;
        Ok(json(&info))
    }

    /// Take back `n` plies.
    pub fn undo(&mut self, n: usize) {
        self.game.undo(n);
    }

    /// Every legal move, explained, with patterns (JSON array).
    #[wasm_bindgen(js_name = legalMoves)]
    pub fn legal_moves(&self) -> String {
        json(&legal_moves(&self.game))
    }

    /// Win/draw/loss odds and the best `lines` lines (JSON). `time_ms` 0
    /// means no time limit (depth and nodes only).
    pub fn analyse(&mut self, depth: u8, nodes: u32, lines: u8, plies: u8, time_ms: u32) -> String {
        let opts = AnalyseOptions {
            depth,
            nodes: u64::from(nodes),
            lines: usize::from(lines),
            line_plies: usize::from(plies),
            time_ms: (time_ms > 0).then_some(u64::from(time_ms)),
        };
        json(&analyse_with(&mut self.analysis, &self.game, opts))
    }

    /// Every legal move's value (JSON array, best first): one search with
    /// a line per move, shallower than `analyse`'s top lines.
    #[wasm_bindgen(js_name = scoreMoves)]
    pub fn score_moves(&mut self, depth: u8, nodes: u32, time_ms: u32) -> String {
        json(&chess_analysis::score_moves(
            &mut self.analysis,
            &self.game,
            depth,
            u64::from(nodes),
            (time_ms > 0).then_some(u64::from(time_ms)),
        ))
    }

    /// Full-strength move found in `time_ms` (JSON `{uci, san}` or `null`).
    #[wasm_bindgen(js_name = bestMove)]
    pub fn best_move(&mut self, time_ms: u32) -> String {
        json(&best_move(&mut self.bot, &self.game, u64::from(time_ms), 64))
    }

    /// The exact tablebase result for the side to move when three pieces
    /// are left (JSON, or `null`).
    pub fn tablebase(&mut self) -> String {
        let board = self.game.board().clone();
        json(&self.analysis.probe(&board))
    }

    /// The bot's move at `level` 1–8 (JSON `{uci, san}` or `null`). Does
    /// not play it.
    #[wasm_bindgen(js_name = botMove)]
    pub fn bot_move(&mut self, level: u8, seed: u32) -> String {
        json(&bot_move(&mut self.bot, &self.game, Level::new(level), u64::from(seed)))
    }

    /// Explain a SAN or UCI move in the current position without playing it.
    pub fn explain(&self, mv: &str) -> String {
        json(&explain_san(self.game.board(), mv))
    }

    /// The game as PGN.
    pub fn pgn(&self, white: &str, black: &str, event: &str) -> String {
        let outcome = self.game.outcome();
        let first_fullmove = self
            .game
            .initial_fen()
            .and_then(|f| f.split_whitespace().nth(5))
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        chess_core::pgn(&PgnGame {
            event,
            site: "open-chess".into(),
            game_id: "local",
            created_at_ms: 0,
            white: PgnPlayer { name: white.into(), rating: None, rating_diff: None, title: None },
            black: PgnPlayer { name: black.into(), rating: None, rating_diff: None, title: None },
            status: outcome.as_ref().map_or(Status::Started, |o| o.status),
            winner: outcome.and_then(|o| o.winner),
            clock: None,
            initial_fen: self.game.initial_fen(),
            sans: self.game.sans(),
            first_to_move: self.game.first_to_move(),
            first_fullmove,
            clocks_ms: &[],
        })
    }

    /// Match a board read from a photo (FEN placement) against the current
    /// position and every position one legal move away: JSON list of
    /// `{uci, san, fen, distance}`, best first (top five). `null` if the
    /// placement can't be parsed.
    #[wasm_bindgen(js_name = matchPhoto)]
    pub fn match_photo(&self, placement: &str) -> String {
        let Some(grid) = chess_analysis::photo::parse_placement(placement) else {
            return "null".into();
        };
        let mut c = chess_analysis::photo::match_after_move(self.game.board(), &grid);
        c.truncate(5);
        json(&c)
    }

    /// Match a board reader's per-square probabilities (JSON: 64 arrays of
    /// 13, as in a reading's `squares`) against the current position and
    /// every position one legal move away: JSON list of `{uci, san, fen,
    /// distance, log_likelihood, probability}`, most likely first (top
    /// five). `null` if the probabilities aren't 64 × 13.
    #[wasm_bindgen(js_name = matchPhotoProbs)]
    pub fn match_photo_probs(&self, squares: &str) -> String {
        let Ok(rows) = serde_json::from_str::<Vec<[f32; 13]>>(squares) else {
            return "null".into();
        };
        let Ok(probs) = <chess_analysis::photo::SquareProbs>::try_from(rows.as_slice()) else {
            return "null".into();
        };
        let mut c = chess_analysis::photo::match_after_move_probs(self.game.board(), &probs);
        c.truncate(5);
        json(&c)
    }

    /// Use an NNUE weight file for analysis and the bot (instead of the
    /// hand-written evaluation).
    #[wasm_bindgen(js_name = loadNetwork)]
    pub fn load_network(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        let net = std::sync::Arc::new(chess_analysis::nnue::Network::from_bytes(bytes).map_err(err)?);
        self.analysis.set_network(Some(net.clone()));
        self.bot.set_network(Some(net));
        Ok(())
    }

    /// Start a fresh analysis table (new game).
    pub fn reset_engines(&mut self) {
        self.analysis.clear();
        self.bot.clear();
    }
}

/// Check a FEN typed or recognised from a photo: `null` if it's a legal
/// position, otherwise what's wrong with it.
#[wasm_bindgen(js_name = checkFen)]
pub fn check_fen(fen: &str) -> Option<String> {
    Game::new(Some(fen.trim())).err().map(|e| format!("{e:?}"))
}

/// A full FEN for a placement read from a photo (castling inferred), or
/// `null` if the placement can't be parsed.
#[wasm_bindgen(js_name = completeFen)]
pub fn complete_fen(placement: &str, white_to_move: bool) -> Option<String> {
    let grid = chess_analysis::photo::parse_placement(placement)?;
    Some(chess_analysis::photo::complete_fen(&grid, white_to_move))
}

/// The notation dictionary as JSON `[[symbol, meaning], …]`.
#[wasm_bindgen(js_name = glossary)]
pub fn glossary_json() -> String {
    json(&glossary())
}

/// Every pattern the trainer knows, as JSON `[{name, definition}]`.
#[wasm_bindgen(js_name = motifs)]
pub fn motifs_json() -> String {
    use chess_analysis::Motif as M;
    let all = [
        M::Checkmate,
        M::BackRankMate,
        M::SmotheredMate,
        M::Check,
        M::DoubleCheck,
        M::DiscoveredCheck,
        M::DiscoveredAttack,
        M::Fork,
        M::Pin,
        M::Skewer,
        M::WinsHangingPiece,
        M::Trade,
        M::HangsPiece,
        M::Castling,
        M::Promotion,
        M::Underpromotion,
        M::EnPassant,
        M::CentralPawn,
        M::Development,
        M::Fianchetto,
        M::EarlyQueen,
    ];
    let v: Vec<_> = all
        .iter()
        .map(|m| serde_json::json!({ "motif": m, "name": m.name(), "definition": m.definition() }))
        .collect();
    json(&v)
}

/// Every named opening in the book (JSON).
#[wasm_bindgen(js_name = openingBook)]
pub fn opening_book() -> String {
    json(&openings::all())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip() {
        let mut s = Session::new(None).unwrap();
        let played: serde_json::Value = serde_json::from_str(&s.play("Nf3").unwrap()).unwrap();
        assert_eq!(played["uci"], "g1f3");
        s.play("e7e5").unwrap();
        let st: serde_json::Value = serde_json::from_str(&s.state()).unwrap();
        assert_eq!(st["ply"], 2);
        assert_eq!(st["turn"], "white");
        assert_eq!(st["san"][1], "e5");
        let a: serde_json::Value = serde_json::from_str(&s.analyse(3, 100_000, 5, 5, 0)).unwrap();
        assert_eq!(a["lines"].as_array().unwrap().len(), 5);
        let b: serde_json::Value = serde_json::from_str(&s.bot_move(3, 1)).unwrap();
        assert!(b["uci"].is_string());
        s.undo(1);
        assert!(s.pgn("me", "bot", "Training").contains("1. Nf3"));
        assert!(serde_json::from_str::<serde_json::Value>(&glossary_json()).unwrap().is_array());
        assert!(motifs_json().contains("Skewer"));
        assert!(check_fen("8/8/8/8/8/8/8/8 w - - 0 1").is_some());
        assert!(check_fen(STARTPOS_FEN).is_none());
        let mut k = Session::new(Some("8/8/8/4k3/8/8/8/4K2R w - - 0 1".into())).unwrap();
        assert!(k.tablebase().contains("win"));
        assert!(k.best_move(50).contains("uci"));
    }
}
