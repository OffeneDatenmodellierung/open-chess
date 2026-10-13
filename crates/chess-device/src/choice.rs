//! What kind of game to play on the board, as chosen on the terminal
//! screen or given on the command line.

use chess_core::Side;

use crate::controller::TimeControl;

/// Where an online game is played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    Lichess,
    /// An open-chess server at this address.
    Server(String),
}

impl Site {
    pub fn base(&self) -> &str {
        match self {
            Site::Lichess => "https://lichess.org",
            Site::Server(url) => url,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Site::Lichess => "Lichess",
            Site::Server(url) => url,
        }
    }
}

/// How an online game starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Wait for one started on the site or the app.
    Wait,
    /// Challenge this player.
    Challenge(String),
    /// Play the site's computer at this level (1–8).
    Computer(u8),
    /// Go back to this game of ours, still in progress.
    Resume(String),
    /// Accept this challenge to us.
    Accept(String),
}

/// A game to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameChoice {
    /// The engine (open-chess's at `level`, or the UCI engine the command
    /// line or PicoChess's settings name).
    Computer { name: String, level: u8, play: Side, time: Option<TimeControl> },
    /// Two people on one board.
    TwoPlayers { white: String, black: String, time: Option<TimeControl> },
    Online {
        site: Site,
        token: String,
        start: Start,
        /// `None`: random.
        play: Option<Side>,
        time: Option<TimeControl>,
        rated: bool,
    },
    /// The board for Lichess's DGT page, answering DGT LiveChess here.
    DgtPage { addr: String },
}

impl GameChoice {
    pub fn describe(&self) -> String {
        match self {
            GameChoice::Computer { level, play, .. } => {
                format!("the computer at level {level}, you play {}", play.name())
            }
            GameChoice::TwoPlayers { white, black, .. } => format!("{white} (White) v {black} (Black)"),
            GameChoice::Online { site, start: Start::Wait, .. } => {
                format!("{}: waiting for a game started there", site.name())
            }
            GameChoice::Online { site, start: Start::Challenge(who), .. } => {
                format!("{}: challenging {who}", site.name())
            }
            GameChoice::Online { site, start: Start::Computer(level), .. } => {
                format!("{}: its computer at level {level}", site.name())
            }
            GameChoice::Online { site, start: Start::Resume(id), .. } => {
                format!("{}: back to game {id}", site.name())
            }
            GameChoice::Online { site, start: Start::Accept(id), .. } => {
                format!("{}: accepting challenge {id}", site.name())
            }
            GameChoice::DgtPage { addr } => format!("the board for lichess.org/dgt (ws://{addr})"),
        }
    }
}
