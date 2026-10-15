//! The open-chess core for the iOS app, as Swift bindings through UniFFI.
//!
//! Everything that decides something lives here, in the same crates the
//! server and the Chess lab use; the app does the I/O (SwiftUI, the camera,
//! CoreBluetooth, the network) and calls in. See
//! `docs/adr/0001-native-ios-app-on-the-open-chess-core.md` and
//! `docs/ios.md`.
//!
//! - [`GameSession`]: a game with the bot and the training analysis, what
//!   help was used, and the record a finished offline game is uploaded as.
//! - [`ratings`]: Glicko-2 as the server computes it, for the provisional
//!   ratings the app shows between syncs.
//! - [`ChessnutBoard`]: the Chessnut boards' protocol and session; the app
//!   moves the bytes over CoreBluetooth.
//! - [`BoardReader`] (feature `vision`): a position from a photo.
//! - [`Store`]: the games, the outbox waiting to be sent and the cached
//!   ratings, in SQLite.
//!
//! Large analysis results cross as JSON (the shapes the Chess lab already
//! reads, decoded in Swift with `Codable`); everything else is typed.
//!
//! Build the Swift package with `scripts/build-xcframework.sh` (on a Mac).

mod board;
mod capture;
mod game;
pub mod ratings;
mod store;
#[cfg(feature = "vision")]
mod vision;

pub use board::{
    BoardEvent, BoardNow, ChessnutBoard, chessnut_read_characteristic, chessnut_write_characteristic,
};
pub use capture::{Capture, CapturePolicy, FrameDecision, default_capture_policy};
pub use game::{
    Assistance, BotMove, GameMode, GameSession, GameState, PlayedMove, check_fen, complete_fen,
    describe_moves,
};
pub use ratings::RatingValue;
pub use store::{OutboxItem, PendingTotals, SpeedRating, Store};
#[cfg(feature = "vision")]
pub use vision::BoardReader;

uniffi::setup_scaffolding!();

/// What went wrong, for Swift's `throws`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// A move that isn't legal here, or isn't a move at all.
    #[error("illegal move: {message}")]
    IllegalMove { message: String },
    /// A FEN that isn't a legal position.
    #[error("invalid position: {message}")]
    InvalidPosition { message: String },
    /// Help that this game doesn't allow (a rated online game).
    #[error("not allowed in this game: {message}")]
    NotAllowed { message: String },
    /// The local database.
    #[error("store: {message}")]
    Store { message: String },
    /// The board reader or its weights.
    #[error("board reader: {message}")]
    Vision { message: String },
}

impl From<rusqlite::Error> for MobileError {
    fn from(e: rusqlite::Error) -> Self {
        MobileError::Store { message: e.to_string() }
    }
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}
