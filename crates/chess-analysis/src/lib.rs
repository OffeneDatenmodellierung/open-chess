//! Chess search and training analysis.
//!
//! Everything here is pure Rust on top of [`chess_core`] and cozy-chess:
//! no I/O, no threads and no wall clock, so the same code runs in the
//! chess server, in a bot, in offline training jobs and in the browser
//! (through `chess-wasm`). Searches are bounded by depth and a node
//! budget rather than time, which also makes them reproducible: the same
//! position, limits and seed always give the same answer.
//!
//! - [`search`]: iterative-deepening alpha-beta with quiescence, a
//!   transposition table and multi-line (multi-PV) output.
//! - [`wdl`]: turning a search score into win / draw / loss odds.
//! - [`explain`]: notation decoded into plain English ("Nxe5+" →
//!   "Knight from f3 takes the pawn on e5, giving check").
//! - [`patterns`]: tactical and strategic motifs a move creates (fork,
//!   pin, skewer, discovered check, back-rank mate…).
//! - [`openings`]: named openings by move order.
//! - [`analysis`]: all of the above for a [`chess_core::Game`], in
//!   serialisable form for a UI.
//! - [`bot`]: a playing bot with strength levels.
//!
//! ## About the probabilities
//!
//! Chess is finite but far too large to enumerate (around 10^120 games),
//! so no program can look at every future move. The win/draw/loss figures
//! come from a depth-limited search scored by a heuristic evaluation,
//! mapped through a logistic model ([`wdl::Wdl::from_score`]). They are
//! estimates, they sharpen as material comes off and the search sees to
//! the end of the game, and they become exact once a forced mate is found
//! or the game is over. The model's parameters are deliberately simple so
//! they can be fitted to real game outcomes later.

pub mod analysis;
pub mod book;
pub mod bot;
pub mod dictionary;
pub mod eval;
pub mod explain;
pub mod nnue;
pub mod openings;
pub mod params;
pub mod patterns;
pub mod photo;
pub mod search;
pub mod tablebase;
pub mod wdl;

mod util;

/// Bumped whenever the search or evaluation changes behaviour for the same
/// parameters, so distributed training never mixes results from different
/// engines (see `chess-train`).
pub const ENGINE_REVISION: u32 = 2;

/// This engine's identity for training: crate version and revision.
pub fn engine_id() -> String {
    format!("{}+r{}", env!("CARGO_PKG_VERSION"), ENGINE_REVISION)
}

pub use analysis::{
    AnalyseOptions, Analysis, Line, LineMove, MoveInfo, MoveScore, analyse, analyse_with, legal_moves,
    score_moves,
};
pub use bot::{BotMove, Level, best_move, bot_move};
pub use explain::{MoveExplanation, explain_move, explain_san, glossary};
pub use openings::{Opening, opening};
pub use params::{SearchParams, TUNABLES, Tunable};
pub use patterns::{Motif, Pattern, detect};
pub use search::{IterationCallback, Limits, Score, SearchResult, Searcher, see};
pub use tablebase::{Probe, Tablebases};
pub use util::{history_hashes, parse_uci, std_uci};
pub use wdl::Wdl;
