//! The game as the device's web API shows it, in Lichess's JSON.
//!
//! The board loop takes a [`View`] of the game after every step and
//! publishes it when something a viewer would see has changed (a move, the
//! result, a setting, the clock pausing); the clocks are worked out at the
//! moment they're sent. The JSON is what lichess.org and the open-chess
//! server send for the same things, so the Chess lab app (and any Board API
//! client) shows a game on the board as it would an online game: the
//! person on the board is the account `board`, the engine their opponent.

use std::time::Instant;

use chess_core::{Side, Speed, Status};
use chess_proto as p;
use serde_json::{Value, json};

use crate::board::Board;
use crate::controller::{Controller, TimeControl};

/// The account the person at the board is.
pub const PLAYER_ID: &str = "board";
const ENGINE_ID: &str = "engine";
/// What Lichess sends as the time of an untimed side.
pub const NO_CLOCK_MS: i64 = 2_147_483_647;

/// Who the engine is, for the app's labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    /// A UCI engine's name; `None` for open-chess's engine, which is named
    /// by its level.
    pub name: Option<String>,
}

/// A game on the board, as of one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub game_id: String,
    pub created_ms: i64,
    pub initial_fen: Option<String>,
    pub moves: Vec<String>,
    pub fen: String,
    pub turn: Side,
    pub status: Status,
    pub winner: Option<Side>,
    pub player: Side,
    pub level: u8,
    pub engine: EngineInfo,
    pub thinking: bool,
    /// The engine's move is waiting to be made on the board.
    pub awaiting_board: bool,
    pub time: Option<TimeControl>,
    /// Both clocks (ms) as of `at`, when the game has a clock.
    pub clocks: Option<[i64; 2]>,
    pub running: Option<Side>,
    pub paused: bool,
    pub at: Instant,
}

impl View {
    pub fn new(
        controller: &Controller,
        board: &impl Board,
        engine: &EngineInfo,
        game_id: String,
        created_ms: i64,
        now: Instant,
    ) -> View {
        let c = controller;
        let g = board.game();
        let (status, winner) = match (c.flagged(), g.outcome()) {
            (Some(side), _) => (Status::Outoftime, Some(side.opposite())),
            (None, Some(o)) => (o.status, o.winner),
            (None, None) => (Status::Started, None),
        };
        let clocks = match (c.remaining(Side::White, now), c.remaining(Side::Black, now)) {
            (Some(white), Some(black)) => Some([white.max(0), black.max(0)]),
            _ => None,
        };
        View {
            game_id,
            created_ms,
            initial_fen: g.initial_fen().map(str::to_owned),
            moves: g.uci_moves(),
            fen: g.fen(),
            turn: g.turn(),
            status,
            winner,
            player: c.player(),
            level: c.level(),
            engine: engine.clone(),
            thinking: c.thinking(),
            awaiting_board: board.awaiting_board(),
            time: c.time_control(),
            clocks,
            running: c.running(),
            paused: c.paused(),
            at: now,
        }
    }

    /// An online game on the board (or none yet, under `waiting_id`).
    pub fn online(
        online: &crate::online::OnlinePlay,
        board: &impl Board,
        waiting_id: &str,
        created_ms: i64,
        now: Instant,
    ) -> View {
        let g = board.game();
        let live = online.game();
        View {
            game_id: live.map_or_else(|| waiting_id.to_owned(), |l| l.id.clone()),
            created_ms,
            initial_fen: g.initial_fen().map(str::to_owned),
            moves: g.uci_moves(),
            fen: g.fen(),
            turn: g.turn(),
            status: live.map_or(Status::Created, |l| l.status),
            winner: live.and_then(|l| l.winner),
            player: live.map_or(Side::White, |l| l.side),
            level: 0,
            engine: EngineInfo {
                name: Some(live.map_or_else(|| "waiting for a game".into(), |l| l.opponent.clone())),
            },
            thinking: false,
            awaiting_board: board.awaiting_board(),
            time: live.and_then(|l| {
                l.limit_ms.map(|limit| TimeControl {
                    minutes: u32::try_from(limit / 60_000).unwrap_or(0),
                    increment_secs: u32::try_from(l.increment_ms / 1000).unwrap_or(0),
                })
            }),
            clocks: live.and_then(|l| l.clocks_at(now)),
            running: live.and_then(crate::online::Live::running),
            paused: false,
            at: now,
        }
    }

    /// A board's game with no engine or online game behind it (the
    /// Lichess DGT page drives it), titled `title`.
    pub fn board_only(board: &impl Board, id: &str, title: &str, created_ms: i64, now: Instant) -> View {
        let g = board.game();
        let (status, winner) = g.outcome().map_or((Status::Started, None), |o| (o.status, o.winner));
        View {
            game_id: id.to_owned(),
            created_ms,
            initial_fen: g.initial_fen().map(str::to_owned),
            moves: g.uci_moves(),
            fen: g.fen(),
            turn: g.turn(),
            status,
            winner,
            player: Side::White,
            level: 0,
            engine: EngineInfo { name: Some(title.to_owned()) },
            thinking: false,
            awaiting_board: board.awaiting_board(),
            time: None,
            clocks: None,
            running: None,
            paused: false,
            at: now,
        }
    }

    /// Before a board is connected.
    pub fn waiting(title: &str) -> View {
        let g = chess_core::Game::new(None).expect("the starting position");
        View {
            game_id: "waiting0".into(),
            created_ms: 0,
            initial_fen: None,
            moves: Vec::new(),
            fen: g.fen(),
            turn: Side::White,
            status: Status::Created,
            winner: None,
            player: Side::White,
            level: 0,
            engine: EngineInfo { name: Some(title.to_owned()) },
            thinking: false,
            awaiting_board: false,
            time: None,
            clocks: None,
            running: None,
            paused: false,
            at: Instant::now(),
        }
    }

    /// The moves in SAN.
    pub fn sans(&self) -> Vec<String> {
        chess_core::Game::from_moves(self.initial_fen.as_deref(), &self.moves.join(" "))
            .map(|g| g.sans().to_vec())
            .unwrap_or_default()
    }

    /// The moves, numbered, the latest last.
    pub fn movetext(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.sans().iter().enumerate() {
            if i % 2 == 0 {
                out.push_str(&(i / 2 + 1).to_string());
                out.push_str(". ");
            }
            out.push_str(s);
            out.push(' ');
        }
        out.trim_end().to_owned()
    }

    /// What's going on, in a few words.
    pub fn status_line(&self) -> String {
        match self.status {
            Status::Created => "waiting for a board".to_owned(),
            Status::Started if self.awaiting_board => {
                format!("make {} on the board", self.sans().last().cloned().unwrap_or_default())
            }
            Status::Started if self.thinking => "the engine is thinking".to_owned(),
            Status::Started if self.turn == self.player => "your move".to_owned(),
            Status::Started => {
                format!("{} to move", if self.turn == Side::White { "White" } else { "Black" })
            }
            s => format!(
                "game over: {} ({})",
                match self.winner {
                    Some(Side::White) => "1-0",
                    Some(Side::Black) => "0-1",
                    None => "1/2-1/2",
                },
                s.name()
            ),
        }
    }

    /// The opponent's name.
    pub fn opponent(&self) -> String {
        self.engine.name.clone().unwrap_or_else(|| format!("open-chess level {}", self.level))
    }

    /// The same to a viewer, apart from the clocks counting down.
    pub fn same(&self, other: &View) -> bool {
        let strip = |v: &View| View { clocks: v.clocks.map(|_| [0, 0]), at: self.at, ..v.clone() };
        strip(self) == strip(other)
    }

    pub fn started(&self) -> bool {
        self.status == Status::Started
    }

    /// Both clocks (ms) at `now`, counting down the one that's running.
    pub fn clocks_at(&self, now: Instant) -> Option<[i64; 2]> {
        let mut c = self.clocks?;
        if let Some(side) = self.running {
            let used = i64::try_from(now.saturating_duration_since(self.at).as_millis()).unwrap_or(i64::MAX);
            c[side.index()] = (c[side.index()] - used).max(0);
        }
        Some(c)
    }

    fn engine_name(&self) -> String {
        self.engine.name.clone().unwrap_or_else(|| format!("open-chess level {}", self.level))
    }

    fn speed(&self) -> Speed {
        self.time.map_or(Speed::Correspondence, |t| Speed::from_clock(t.minutes * 60, t.increment_secs))
    }

    fn variant(&self) -> p::Variant {
        if self.initial_fen.is_some() { p::Variant::from_position() } else { p::Variant::standard() }
    }

    fn players(&self) -> (p::GameEventPlayer, p::GameEventPlayer) {
        let person = p::GameEventPlayer {
            id: PLAYER_ID.into(),
            name: "Board".into(),
            title: None,
            rating: None,
            rating_diff: None,
            provisional: None,
            ai_level: None,
        };
        let engine = p::GameEventPlayer {
            id: ENGINE_ID.into(),
            name: self.engine_name(),
            title: Some("BOT".into()),
            ai_level: self.engine.name.is_none().then_some(i32::from(self.level)),
            ..person.clone()
        };
        if self.player == Side::White { (person, engine) } else { (engine, person) }
    }

    /// A `gameState` line of the board game stream.
    pub fn game_state(&self, now: Instant) -> p::GameState {
        let [wtime, btime] = self.clocks_at(now).unwrap_or([NO_CLOCK_MS; 2]);
        let inc = self.time.map_or(0, |t| i64::from(t.increment_secs) * 1000);
        p::GameState {
            kind: p::GameState::TYPE.into(),
            moves: self.moves.join(" "),
            wtime,
            btime,
            winc: inc,
            binc: inc,
            status: self.status,
            winner: self.winner,
            wdraw: false,
            bdraw: false,
            wtakeback: false,
            btakeback: false,
            expiration: None,
        }
    }

    /// The `gameFull` line that opens the board game stream.
    pub fn game_full(&self, now: Instant) -> p::GameFull {
        let (white, black) = self.players();
        let speed = self.speed();
        p::GameFull {
            kind: p::GameFull::TYPE.into(),
            id: self.game_id.clone(),
            variant: self.variant(),
            clock: self.time.map(|t| p::GameClock {
                initial: i64::from(t.minutes) * 60_000,
                increment: i64::from(t.increment_secs) * 1000,
            }),
            speed,
            perf: p::PerfName { name: speed.display().into() },
            rated: false,
            created_at: self.created_ms,
            white,
            black,
            initial_fen: self.initial_fen.clone().unwrap_or_else(|| "startpos".into()),
            state: self.game_state(now),
            days_per_turn: None,
            tournament_id: None,
        }
    }

    /// A `gameStart` (or `gameFinish`) line of the event stream.
    pub fn game_event(&self, kind: &str, now: Instant) -> p::GameEvent {
        let speed = self.speed();
        p::GameEvent {
            kind: kind.into(),
            game: p::GameEventInfo {
                full_id: format!("{}0000", self.game_id),
                game_id: self.game_id.clone(),
                fen: self.fen.clone(),
                color: self.player,
                last_move: self.moves.last().cloned().unwrap_or_default(),
                source: "ai".into(),
                status: self.status.into(),
                variant: self.variant().brief(),
                speed,
                perf: speed.key().into(),
                rated: false,
                has_moved: !self.moves.is_empty(),
                opponent: p::GameEventOpponent {
                    id: ENGINE_ID.into(),
                    username: self.engine_name(),
                    rating: 0,
                    rating_diff: None,
                },
                is_my_turn: self.started() && self.turn == self.player,
                seconds_left: self.clocks_at(now).map(|c| c[self.player.index()] / 1000),
                winner: self.winner,
                rating: None,
                rating_diff: None,
                compat: p::Compat { bot: false, board: true },
                id: self.game_id.clone(),
            },
        }
    }

    /// The first line of the live feed (`/api/stream/game/{id}`).
    pub fn feed_description(&self, now: Instant) -> Value {
        let (white, black) = self.players();
        let player = |pl: &p::GameEventPlayer| json!({"user": {"name": pl.name, "id": pl.id}});
        let mut v = json!({
            "id": self.game_id,
            "variant": self.variant().brief(),
            "speed": self.speed(),
            "perf": self.speed().key(),
            "rated": false,
            "initialFen": self.initial_fen.clone().unwrap_or_else(|| chess_core::STARTPOS_FEN.into()),
            "fen": self.fen,
            "player": self.turn,
            "turns": self.moves.len(),
            "source": "ai",
            "status": p::StatusRef::from(self.status),
            "createdAt": self.created_ms,
            "players": {"white": player(&white), "black": player(&black)},
        });
        if let Some(w) = self.winner {
            v["winner"] = json!(w);
        }
        if let Some(lm) = self.moves.last() {
            v["lastMove"] = json!(lm);
        }
        if let Some(t) = self.time {
            v["clock"] = json!({
                "initial": t.minutes * 60,
                "increment": t.increment_secs,
                "totalTime": t.minutes * 60 + 40 * t.increment_secs,
            });
        }
        if let Some([w, b]) = self.clocks_at(now) {
            v["wc"] = json!(w / 1000);
            v["bc"] = json!(b / 1000);
        }
        v
    }

    /// A move line of the live feed: the position and both clocks (seconds).
    pub fn feed_move(&self, now: Instant) -> p::MoveLine {
        let [wc, bc] = self.clocks_at(now).map_or([0, 0], |c| [c[0] / 1000, c[1] / 1000]);
        p::MoveLine { fen: self.fen.clone(), lm: self.moves.last().cloned(), wc, bc }
    }

    /// The device's settings and what it's doing (`GET /api/device`).
    pub fn device(&self) -> Value {
        json!({
            "gameId": self.game_id,
            "level": self.level,
            "play": self.player,
            "time": self.time.map(|t| t.to_string()),
            "engine": self.engine_name(),
            "builtin": self.engine.name.is_none(),
            "thinking": self.thinking,
            "awaitingBoard": self.awaiting_board,
            "paused": self.paused,
            "status": self.status,
        })
    }
}

/// The account the app signs in as (`GET /api/account`).
pub fn account() -> Value {
    json!({
        "id": PLAYER_ID,
        "username": "Board",
        "perfs": {},
        "createdAt": 0,
        "seenAt": 0,
        "playTime": {"total": 0, "tv": 0},
    })
}

/// A Lichess-style game id (eight letters and digits), different for each
/// game and each run of the device.
pub fn game_id(seed: u64, game: u32) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut x = seed ^ u64::from(game).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // splitmix64
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (0..8)
        .map(|_| {
            let c = ALPHABET[(x % 62) as usize];
            x /= 62;
            char::from(c)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Setup;
    use chess_dgt::{Follower, Session};
    use std::time::Duration;

    fn view(setup: Setup) -> View {
        let c = Controller::new(setup);
        let s = Session::new(Follower::new());
        View::new(&c, &s, &EngineInfo { name: None }, game_id(7, 1), 1_700_000_000_000, Instant::now())
    }

    #[test]
    fn game_ids_look_like_lichess_ids() {
        let a = game_id(7, 1);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, game_id(7, 2));
        assert_ne!(a, game_id(8, 1));
    }

    #[test]
    fn an_untimed_game_reads_as_a_board_api_game() {
        let v = view(Setup::default());
        let full = serde_json::to_value(v.game_full(v.at)).unwrap();
        assert_eq!(full["type"], "gameFull");
        assert_eq!(full["white"]["id"], PLAYER_ID);
        assert_eq!(full["black"]["name"], "open-chess level 4");
        assert_eq!(full["black"]["aiLevel"], 4);
        assert_eq!(full["initialFen"], "startpos");
        assert_eq!(full["speed"], "correspondence");
        assert_eq!(full["state"]["status"], "started");
        assert_eq!(full["state"]["wtime"], NO_CLOCK_MS);
        assert!(full.get("clock").is_none());
        // And reads back with the client's types.
        let line = serde_json::to_vec(&full).unwrap();
        assert!(matches!(p::BoardEvent::parse(&line).unwrap(), p::BoardEvent::GameFull(_)));

        let start = serde_json::to_vec(&v.game_event(p::GameEvent::START, v.at)).unwrap();
        let p::StreamEvent::GameStart(info) = p::StreamEvent::parse(&start).unwrap() else { panic!() };
        assert_eq!((info.color, info.is_my_turn), (Side::White, true));
    }

    #[test]
    fn a_timed_game_has_the_clock_counting_down() {
        let v = view(Setup { player: Side::Black, time: Some("5+3".parse().unwrap()), ..Setup::default() });
        let full = serde_json::to_value(v.game_full(v.at)).unwrap();
        assert_eq!(full["clock"]["initial"], 300_000);
        assert_eq!(full["clock"]["increment"], 3000);
        assert_eq!(full["speed"], "blitz");
        assert_eq!(full["black"]["id"], PLAYER_ID);
        assert_eq!(full["state"]["wtime"], 300_000);
        // Clocks start with the first move, so nothing runs yet.
        assert_eq!(v.clocks_at(v.at + Duration::from_secs(5)), Some([300_000, 300_000]));
        let running = View { running: Some(Side::White), ..v.clone() };
        assert_eq!(running.clocks_at(v.at + Duration::from_secs(5)), Some([295_000, 300_000]));
        assert_eq!(running.clocks_at(v.at + Duration::from_secs(400)).unwrap()[0], 0);
    }

    #[test]
    fn only_what_a_viewer_sees_counts_as_a_change() {
        let v = view(Setup { time: Some("5+0".parse().unwrap()), ..Setup::default() });
        let later = View { clocks: Some([299_000, 300_000]), at: v.at + Duration::from_secs(1), ..v.clone() };
        assert!(v.same(&later));
        assert!(!v.same(&View { level: 5, ..v.clone() }));
        assert!(!v.same(&View { moves: vec!["e2e4".into()], ..v.clone() }));
        assert!(!v.same(&View { paused: true, ..v.clone() }));
    }

    #[test]
    fn the_device_reports_its_settings() {
        let v = view(Setup { level: 6, time: Some("10+5".parse().unwrap()), ..Setup::default() });
        let d = v.device();
        assert_eq!(d["level"], 6);
        assert_eq!(d["play"], "white");
        assert_eq!(d["time"], "10+5");
        assert_eq!(d["builtin"], true);
    }
}
