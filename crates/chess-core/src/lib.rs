//! Chess rules, shared by the chess server, the fair-play
//! analyser and any bot built on top of them.
//!
//! Move generation is [`cozy_chess`] (MIT). On top of it this crate adds
//! what a game server needs and cozy-chess leaves out: the move history
//! with repetition and draw rules, SAN, UCI in the standard (non-960)
//! castling notation Lichess uses, Fischer clocks, Lichess's game status
//! codes and speed classes, PGN export and Glicko-2 ratings.
//!
//! Nothing here does I/O, so every rule is unit-tested in isolation.

mod clock;
mod game;
mod glicko;
mod pgn;
mod san;
mod speed;
mod status;

pub use clock::{Clock, MoveClock};
pub use game::{Game, GameError, Outcome, Played, Side};
pub use glicko::{GameScore, Rating};
pub use pgn::{PgnGame, PgnPlayer, pgn};
pub use san::san;
pub use speed::Speed;
pub use status::Status;

/// The FEN of the standard starting position.
pub const STARTPOS_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
