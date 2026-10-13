//! Playing online from the board: a game on lichess.org or an open-chess
//! server, through the Board API, with no browser.
//!
//! [`OnlinePlay`] is the game logic without I/O: the server's moves are
//! played on the board (shown on the clock, lit on boards with lights) and
//! wait to be made there; the player's moves on the board are sent; the
//! clock shows the server's times. [`Connection`] is the network side, on a
//! thread of its own: the event stream, each game's stream, and sending
//! moves.
//!
//! Games start elsewhere (a seek or challenge accepted on the site or the
//! app) or from the device with `--challenge`; the device plays whichever
//! starts. A takeback on the board can't take a move back online: the
//! board then waits for the game's position to be set up again.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use chess_core::{Side, Status};
use chess_dgt::{ClockButton, Command, Event, Observation, RunMode, Square};
use chess_proto::{GameFull, GameState};

use crate::board::Board;

/// What to ask the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Move { game: String, uci: String },
}

/// The game being played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub id: String,
    pub side: Side,
    pub opponent: String,
    pub initial_fen: Option<String>,
    /// The moves as the server has them.
    pub moves: Vec<String>,
    pub status: Status,
    pub winner: Option<Side>,
    /// Both clocks (ms) when the server last said, and when that was.
    pub clocks: Option<([i64; 2], Instant)>,
    pub increment_ms: i64,
    pub limit_ms: Option<i64>,
    /// A move of ours sent and not yet in the server's moves.
    sent: Option<String>,
}

impl Live {
    pub fn started(&self) -> bool {
        self.status == Status::Started
    }

    /// The side to move in the server's game.
    pub fn turn(&self) -> Side {
        let first = if self.initial_fen.as_deref().is_some_and(|f| f.split(' ').nth(1) == Some("b")) {
            Side::Black
        } else {
            Side::White
        };
        Side::at_ply(first, self.moves.len())
    }

    /// The side whose clock is running (Lichess starts clocks once both
    /// sides have moved).
    pub fn running(&self) -> Option<Side> {
        (self.started() && self.clocks.is_some() && self.moves.len() >= 2).then(|| self.turn())
    }

    /// Both clocks (ms) at `now`.
    pub fn clocks_at(&self, now: Instant) -> Option<[i64; 2]> {
        let (mut c, at) = self.clocks?;
        if let Some(side) = self.running() {
            let used = i64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(i64::MAX);
            c[side.index()] = (c[side.index()] - used).max(0);
        }
        Some(c)
    }
}

/// An online game on the board.
#[derive(Debug)]
pub struct OnlinePlay {
    me: String,
    game: Option<Live>,
    requests: Vec<Request>,
    last_opponent_san: Option<String>,
    white_on_left: bool,
    games: u32,
}

/// What Lichess sends for an untimed side.
const NO_CLOCK: i64 = 2_147_483_647;

impl OnlinePlay {
    /// `me` is the account's id; `white_on_left` says which side of the
    /// clock is White's.
    pub fn new(me: &str, white_on_left: bool) -> OnlinePlay {
        OnlinePlay {
            me: me.to_lowercase(),
            game: None,
            requests: Vec::new(),
            last_opponent_san: None,
            white_on_left,
            games: 0,
        }
    }

    pub fn game(&self) -> Option<&Live> {
        self.game.as_ref()
    }

    /// How many games have started.
    pub fn games(&self) -> u32 {
        self.games
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// A game started (its stream's first line).
    pub fn on_full(&mut self, full: &GameFull, board: &mut impl Board, now: Instant) {
        let white = full.white.id.to_lowercase() == self.me;
        let (side, opponent) = if white { (Side::White, &full.black) } else { (Side::Black, &full.white) };
        let opponent = if opponent.name.is_empty() {
            opponent.ai_level.map_or_else(|| "opponent".into(), |l| format!("AI level {l}"))
        } else {
            opponent.name.clone()
        };
        let fresh = self.game.as_ref().is_none_or(|g| g.id != full.id);
        if fresh {
            self.games += 1;
            self.last_opponent_san = None;
        }
        self.game = Some(Live {
            id: full.id.clone(),
            side,
            opponent,
            initial_fen: (full.initial_fen != "startpos").then(|| full.initial_fen.clone()),
            moves: Vec::new(),
            status: Status::Started,
            winner: None,
            clocks: None,
            increment_ms: full.clock.map_or(0, |c| c.increment),
            limit_ms: full.clock.map(|c| c.initial),
            sent: None,
        });
        if fresh {
            board.send(Command::LightsOff);
            say(board, if side == Side::White { "you: wht" } else { "you: blk" }, 2);
        }
        self.on_state(&full.state, board, now);
    }

    /// The game moved on (or ended) on the server.
    pub fn on_state(&mut self, state: &GameState, board: &mut impl Board, now: Instant) {
        let Some(g) = self.game.as_mut() else { return };
        let was_started = g.started();
        g.moves = state.moves.split_whitespace().map(str::to_owned).collect();
        g.status = state.status;
        g.winner = state.winner;
        g.clocks = (state.wtime < NO_CLOCK).then_some(([state.wtime, state.btime], now));
        if g.sent.as_ref().is_some_and(|m| g.moves.last() == Some(m)) {
            g.sent = None;
        }
        self.reconcile(board);
        self.show_clock(board, now);
        let g = self.game.as_ref().expect("a game");
        if was_started && !g.started() {
            board.send(Command::LightsOff);
            say(board, &result_text(g.status, g.winner), 4);
        }
    }

    /// The server refused a move (or something else went wrong).
    pub fn on_refused(&mut self, board: &mut impl Board) {
        if let Some(g) = self.game.as_mut() {
            g.sent = None;
        }
        say(board, "refused", 4);
    }

    /// Something happened on the board.
    pub fn on_event(&mut self, e: &Event, board: &mut impl Board) {
        if let Event::Game(o) = e {
            match o {
                Observation::Move { .. }
                | Observation::Takeback { .. }
                | Observation::NewGame
                | Observation::Caught => {
                    if matches!(o, Observation::Caught) {
                        board.send(Command::LightsOff);
                    }
                    self.reconcile(board);
                }
                Observation::Unchanged | Observation::Waiting => {}
            }
        }
    }

    /// A clock button: the first shows the opponent's last move again.
    pub fn on_button(&mut self, b: ClockButton, board: &mut impl Board) {
        if b == ClockButton::Button(0) {
            let text = self.last_opponent_san.clone().unwrap_or_else(|| "no move".into());
            say(board, &text, 0);
        }
    }

    /// Bring the board and the server's game together: play the server's
    /// new moves on the board, send the player's new move.
    fn reconcile(&mut self, board: &mut impl Board) {
        let Some(g) = self.game.as_mut() else { return };
        let local = board.game().uci_moves();
        let server = &g.moves;
        if local == *server {
            return;
        }
        if server.len() > local.len() && server.starts_with(&local) {
            // The server is ahead: its moves, to be made on the board.
            let missing: Vec<String> = server[local.len()..].to_vec();
            for (i, m) in missing.iter().enumerate() {
                let Ok(played) = board.play(m) else {
                    say(board, "set up", 4);
                    return;
                };
                if i + 1 == missing.len() {
                    let side = board.game().turn().opposite();
                    if side != g.side {
                        self.last_opponent_san = Some(played.san.clone());
                    }
                    say(board, &played.san, 2);
                    if let (Some(from), Some(to)) = (Square::from_name(&m[0..2]), Square::from_name(&m[2..4]))
                    {
                        board.send(Command::Lights { from, to });
                    }
                }
            }
            return;
        }
        if local.len() == server.len() + 1 && local.starts_with(server) && g.started() {
            // The player's move, made on the board.
            let m = local.last().expect("one more").clone();
            if g.turn() == g.side && board.game().turn() != g.side {
                if g.sent.as_ref() != Some(&m) {
                    g.sent = Some(m.clone());
                    self.requests.push(Request::Move { game: g.id.clone(), uci: m });
                }
            } else {
                say(board, "not now", 4);
            }
            return;
        }
        // Taken back on the board, or a different position: the game
        // online goes on, so its position has to be set up again.
        say(board, "set back", 4);
    }

    fn show_clock(&self, board: &mut impl Board, now: Instant) {
        let Some(g) = &self.game else { return };
        let Some([w, b]) = g.clocks_at(now) else { return };
        let (left, right) = if self.white_on_left { (w, b) } else { (b, w) };
        let mode = match g.running() {
            Some(side) if (side == Side::White) == self.white_on_left => RunMode::LeftRunning,
            Some(_) => RunMode::RightRunning,
            None => RunMode::Paused,
        };
        board.send(Command::ClockSetAndRun {
            left: hms(left),
            right: hms(right),
            mode,
            lever_toggles: false,
        });
    }
}

fn hms(ms: i64) -> (u8, u8, u8) {
    let secs = u64::try_from((ms.max(0) + 999) / 1000).unwrap_or(0);
    ((secs / 3600).min(9) as u8, ((secs / 60) % 60) as u8, (secs % 60) as u8)
}

fn result_text(status: Status, winner: Option<Side>) -> String {
    let score = match winner {
        Some(Side::White) => "1-0",
        Some(Side::Black) => "0-1",
        None => "1/2",
    };
    match status {
        Status::Mate => format!("mate {score}"),
        Status::Resign => format!("resn {score}"),
        Status::Outoftime => format!("time {score}"),
        Status::Aborted => "aborted".into(),
        _ => format!("end {score}"),
    }
}

fn say(board: &mut impl Board, text: &str, beep: u8) {
    board.send(Command::ClockText { text: text.chars().take(8).collect(), beep });
}

// ── the network side ──

/// What the server said.
#[derive(Debug, Clone)]
pub enum FromServer {
    /// Signed in as this account (id).
    Account(String),
    Full(Box<GameFull>),
    State(GameState),
    /// A move wasn't accepted.
    Refused(String),
    /// Something went wrong (the device keeps trying).
    Problem(String),
}

/// A challenge to send when starting.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub opponent: String,
    pub request: chess_client::ChallengeRequest,
}

/// The connection to the server, on a thread of its own.
#[derive(Debug)]
pub struct Connection {
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
    rx: Receiver<FromServer>,
}

impl Connection {
    pub fn spawn(base: &str, token: &str, challenge: Option<Challenge>) -> anyhow::Result<Connection> {
        let client = chess_client::Client::new(base, Some(token))?;
        let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let (out, rx) = mpsc::channel::<FromServer>();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        thread::Builder::new().name("online".into()).spawn(move || {
            rt.block_on(async move {
                let me = loop {
                    match client.account().await {
                        Ok(u) => break u.id,
                        Err(e) => {
                            let _ = out.send(FromServer::Problem(format!("signing in: {e}")));
                            tokio::time::sleep(Duration::from_secs(10)).await;
                        }
                    }
                };
                let _ = out.send(FromServer::Account(me));
                if let Some(c) = challenge
                    && let Err(e) = client.challenge(&c.opponent, &c.request).await
                {
                    let _ = out.send(FromServer::Problem(format!("challenging {}: {e}", c.opponent)));
                }
                let sender = client.clone();
                let moves_out = out.clone();
                tokio::spawn(async move {
                    while let Some(r) = requests.recv().await {
                        let Request::Move { game, uci } = r;
                        if let Err(e) = sender.make_move(chess_client::Api::Board, &game, &uci, false).await {
                            let _ = moves_out.send(FromServer::Refused(format!("{uci}: {e}")));
                        }
                    }
                });
                events(&client, &out).await;
            });
        })?;
        Ok(Connection { tx, rx })
    }

    pub fn send(&self, r: Request) {
        let _ = self.tx.send(r);
    }

    /// What the server has said since last asked.
    pub fn messages(&self) -> impl Iterator<Item = FromServer> + '_ {
        self.rx.try_iter()
    }
}

/// Follow the event stream for good: each game that starts is streamed.
async fn events(client: &chess_client::Client, out: &mpsc::Sender<FromServer>) {
    let mut streaming: Option<(String, tokio::task::JoinHandle<()>)> = None;
    loop {
        match client.stream_events().await {
            Ok(mut s) => {
                while let Some(e) = s.next().await {
                    match e {
                        Ok(chess_proto::StreamEvent::GameStart(info)) => {
                            let id = info.game_id.clone();
                            if streaming.as_ref().is_some_and(|(g, h)| *g == id && !h.is_finished()) {
                                continue;
                            }
                            if let Some((_, h)) = streaming.take() {
                                h.abort();
                            }
                            let (c, o) = (client.clone(), out.clone());
                            let gid = id.clone();
                            streaming = Some((id, tokio::spawn(async move { game(&c, &gid, &o).await })));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let _ = out.send(FromServer::Problem(format!("event stream: {e}")));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = out.send(FromServer::Problem(format!("event stream: {e}")));
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn game(client: &chess_client::Client, id: &str, out: &mpsc::Sender<FromServer>) {
    match client.stream_game(chess_client::Api::Board, id).await {
        Ok(mut s) => {
            while let Some(e) = s.next().await {
                let m = match e {
                    Ok(chess_proto::BoardEvent::GameFull(f)) => FromServer::Full(f),
                    Ok(chess_proto::BoardEvent::GameState(st)) => FromServer::State(st),
                    Ok(_) => continue,
                    Err(e) => FromServer::Problem(format!("game {id}: {e}")),
                };
                if out.send(m).is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            let _ = out.send(FromServer::Problem(format!("game {id}: {e}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_dgt::{Follower, Placement, Session};
    use chess_proto as p;

    fn player(id: &str) -> p::GameEventPlayer {
        p::GameEventPlayer {
            id: id.into(),
            name: id.into(),
            title: None,
            rating: Some(1500),
            rating_diff: None,
            provisional: None,
            ai_level: None,
        }
    }

    fn state(moves: &str, status: Status) -> GameState {
        GameState {
            kind: GameState::TYPE.into(),
            moves: moves.into(),
            wtime: 600_000,
            btime: 600_000,
            winc: 5000,
            binc: 5000,
            status,
            winner: None,
            wdraw: false,
            bdraw: false,
            wtakeback: false,
            btakeback: false,
            expiration: None,
        }
    }

    fn full(white: &str, black: &str, moves: &str) -> GameFull {
        GameFull {
            kind: GameFull::TYPE.into(),
            id: "abcd1234".into(),
            variant: p::Variant::standard(),
            clock: Some(p::GameClock { initial: 600_000, increment: 5000 }),
            speed: chess_core::Speed::Rapid,
            perf: p::PerfName { name: "Rapid".into() },
            rated: false,
            created_at: 0,
            white: player(white),
            black: player(black),
            initial_fen: "startpos".into(),
            state: state(moves, Status::Started),
            days_per_turn: None,
            tournament_id: None,
        }
    }

    fn frame(id: u8, data: &[u8]) -> Vec<u8> {
        let len = data.len() + 3;
        let mut v = vec![id | 0x80, (len >> 7) as u8, (len & 0x7f) as u8];
        v.extend_from_slice(data);
        v
    }

    /// The board shows the position after these moves (a board dump).
    fn show(s: &mut Session, o: &mut OnlinePlay, moves: &str) {
        let g = chess_core::Game::from_moves(None, moves).unwrap();
        let p = Placement::from_fen(&g.fen()).unwrap();
        let mut codes = [0u8; 64];
        for (i, code) in codes.iter_mut().enumerate() {
            let sq = (7 - i / 8) * 8 + i % 8;
            *code = p.0[sq].map_or(0, |piece| {
                let base = match piece.kind {
                    chess_dgt::PieceKind::Pawn => 1,
                    chess_dgt::PieceKind::Rook => 2,
                    chess_dgt::PieceKind::Knight => 3,
                    chess_dgt::PieceKind::Bishop => 4,
                    chess_dgt::PieceKind::King => 5,
                    chess_dgt::PieceKind::Queen => 6,
                };
                if piece.white { base } else { base + 6 }
            });
        }
        for e in s.receive(&frame(0x06, &codes), Instant::now()) {
            o.on_event(&e, s);
        }
    }

    fn texts(s: &mut Session) -> Vec<String> {
        s.take_commands()
            .into_iter()
            .filter_map(|c| match c {
                Command::ClockText { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plays_an_online_game_from_the_board() {
        let now = Instant::now();
        let mut s = Session::new(Follower::new());
        let mut o = OnlinePlay::new("Mark", true);
        show(&mut s, &mut o, "");
        o.on_full(&full("mark", "magnus", ""), &mut s, now);
        assert_eq!(texts(&mut s), ["you: wht"]);
        assert_eq!(o.game().unwrap().opponent, "magnus");

        // My move on the board goes to the server, once.
        show(&mut s, &mut o, "e2e4");
        assert_eq!(o.take_requests(), [Request::Move { game: "abcd1234".into(), uci: "e2e4".into() }]);
        show(&mut s, &mut o, "e2e4");
        assert!(o.take_requests().is_empty());

        // The server echoes it, then the opponent replies: shown, lit, and
        // waiting for the board.
        o.on_state(&state("e2e4", Status::Started), &mut s, now);
        s.take_commands();
        o.on_state(&state("e2e4 c7c5", Status::Started), &mut s, now);
        let cmds = s.take_commands();
        assert!(cmds.contains(&Command::ClockText { text: "c5".into(), beep: 2 }), "{cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, Command::Lights { .. })));
        assert!(cmds.iter().any(|c| matches!(c, Command::ClockSetAndRun { mode: RunMode::LeftRunning, .. })));
        assert!(s.follower().awaiting_board());
        show(&mut s, &mut o, "e2e4 c7c5");
        assert!(!s.follower().awaiting_board());

        // The game ends online.
        let mut end = state("e2e4 c7c5", Status::Resign);
        end.winner = Some(Side::White);
        o.on_state(&end, &mut s, now);
        assert!(texts(&mut s).contains(&"resn 1-0".to_string()));
    }

    #[test]
    fn a_game_in_progress_is_set_up_on_the_board() {
        let now = Instant::now();
        let mut s = Session::new(Follower::new());
        let mut o = OnlinePlay::new("mark", true);
        show(&mut s, &mut o, "");
        o.on_full(&full("magnus", "mark", "d2d4 g8f6 c2c4"), &mut s, now);
        assert_eq!(s.follower().game().uci_moves(), ["d2d4", "g8f6", "c2c4"]);
        assert!(s.follower().awaiting_board());
        show(&mut s, &mut o, "d2d4 g8f6 c2c4");
        assert!(!s.follower().awaiting_board());
        // Black to move: my move goes off.
        show(&mut s, &mut o, "d2d4 g8f6 c2c4 e7e6");
        assert_eq!(o.take_requests().len(), 1);
    }

    #[test]
    fn a_refused_move_can_be_sent_again() {
        let now = Instant::now();
        let mut s = Session::new(Follower::new());
        let mut o = OnlinePlay::new("mark", true);
        show(&mut s, &mut o, "");
        o.on_full(&full("mark", "magnus", ""), &mut s, now);
        show(&mut s, &mut o, "e2e4");
        assert_eq!(o.take_requests().len(), 1);
        o.on_refused(&mut s);
        assert!(texts(&mut s).contains(&"refused".to_string()));
        // Taken back on the board and played again: sent again.
        show(&mut s, &mut o, "");
        show(&mut s, &mut o, "e2e4");
        assert_eq!(o.take_requests().len(), 1);
    }
}
