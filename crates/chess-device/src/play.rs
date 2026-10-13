//! Playing on a board: against the engine, online, or as the board for
//! Lichess's DGT page. Each loop drives a [`Link`], publishes the game as
//! a [`View`] (for the app, PicoChess's tools and the terminal screen) and
//! takes settings from the web.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chess_dgt::{ClockButton, Command, Event, Observation};
use tokio::sync::watch;

use crate::board::Board;
use crate::controller::{Controller, describe};
use crate::links::{Input, Link};
use crate::livechess::{self, LiveChess};
use crate::log::{self, Source};
use crate::online::{Connection, FromServer, OnlinePlay};
use crate::opponent::Opponent;
use crate::view::{EngineInfo, View, game_id};
use crate::web::{self, Change, Request, Web};

/// What every mode needs.
#[derive(Debug, Clone)]
pub struct Options {
    /// White's time is on the clock's left.
    pub white_on_left: bool,
    /// Serve the app and the APIs here.
    pub listen: Option<String>,
    pub app: Option<PathBuf>,
    /// The device's name, shown in the app.
    pub name: String,
    /// For game ids.
    pub seed: u64,
    /// Set to stop playing (the loop returns).
    pub stop: Arc<AtomicBool>,
}

impl Options {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Who the board plays.
pub enum Mode {
    Engine { controller: Box<Controller>, opponent: Opponent, engine: EngineInfo },
    Online { connection: Connection },
    LiveChess { addr: String },
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn publish(tx: &watch::Sender<View>, v: View) {
    tx.send_if_modified(|old| {
        if old.same(&v) {
            false
        } else {
            *old = v;
            true
        }
    });
}

fn serve_web(o: &Options, view: &watch::Sender<View>) -> io::Result<mpsc::Receiver<Request>> {
    let (request_tx, requests) = mpsc::channel::<Request>();
    if let Some(addr) = &o.listen {
        let at = web::spawn(addr, Web::new(view.subscribe(), request_tx, o.app.clone(), &o.name))
            .map_err(|e| io::Error::other(format!("listening on {addr}: {e}")))?;
        log::line(Source::Web, format!("the app and APIs on http://{at}/"));
    }
    Ok(requests)
}

fn note(e: &Event, board: &impl Board) {
    match e {
        Event::Connected { .. } => log::line(Source::Board, "connected"),
        Event::Game(Observation::NewGame) => log::line(Source::Game, "new game"),
        Event::Game(Observation::Move { san, .. }) => {
            log::line(Source::Game, format!("{san}: {}", describe(board.game())));
        }
        Event::Game(Observation::Takeback { plies }) => {
            log::line(Source::Game, format!("took back {plies}: {}", describe(board.game())));
        }
        Event::Game(Observation::Caught) => log::line(Source::Game, "the move is on the board"),
        _ => {}
    }
}

/// Play `mode` on a board until the board fails, publishing on `view`.
pub fn drive<L: Link>(link: &mut L, mode: Mode, o: &Options, view: &watch::Sender<View>) -> io::Result<()> {
    match mode {
        Mode::Engine { controller, opponent, engine } => {
            play_engine(link, *controller, &opponent, &engine, o, view)
        }
        Mode::Online { connection } => play_online(link, &connection, o, view),
        Mode::LiveChess { addr } => play_livechess(link, &addr, o, view),
    }
}

fn play_engine<L: Link>(
    link: &mut L,
    mut controller: Controller,
    opponent: &Opponent,
    engine: &EngineInfo,
    o: &Options,
    view: &watch::Sender<View>,
) -> io::Result<()> {
    let mut game = (controller.games(), game_id(o.seed, 0), now_ms());
    publish(view, View::new(&controller, link, engine, game.1.clone(), game.2, Instant::now()));
    let requests = serve_web(o, view)?;
    loop {
        if o.stopped() {
            return Ok(());
        }
        let now = Instant::now();
        for input in link.exchange(now)? {
            let think = match input {
                Input::Event(e) => {
                    note(&e, link);
                    controller.on_event(&e, link, now)
                }
                Input::Button(b) => {
                    log::line(Source::Clock, format!("button {b:?}"));
                    controller.on_button(b, link, now)
                }
            };
            if let Some(t) = think {
                opponent.ask(t);
            }
        }
        while let Some(r) = opponent.poll() {
            if let Some(e) = &r.error {
                log::line(Source::Engine, e.clone());
            }
            if let Some(t) = controller.on_reply(r.ply, r.uci.as_deref(), link, Instant::now()) {
                opponent.ask(t);
            }
            if link.awaiting_board() {
                let san = link.game().sans().last().cloned().unwrap_or_default();
                log::line(Source::Engine, format!("plays {san} (make it on the board)"));
            }
        }
        let mut replies = Vec::new();
        while let Ok(r) = requests.try_recv() {
            log::line(Source::Web, format!("{:?}", r.change));
            let result = apply(r.change, &mut controller, link, opponent);
            replies.push((r.reply, result));
        }
        controller.tick(link, Instant::now());
        if controller.games() != game.0 {
            game = (controller.games(), game_id(o.seed, controller.games()), now_ms());
        }
        publish(view, View::new(&controller, link, engine, game.1.clone(), game.2, Instant::now()));
        // Answered once the change is in what the clients read.
        for (tx, result) in replies {
            let _ = tx.send(result);
        }
    }
}

/// A settings change from the app or PicoChess's tools.
fn apply(
    change: Change,
    controller: &mut Controller,
    board: &mut impl Board,
    opponent: &Opponent,
) -> Result<String, String> {
    let now = Instant::now();
    match change {
        Change::Level(level) => {
            controller.set_level(level, board);
            Ok(format!("The engine plays at level {}.", controller.level()))
        }
        Change::Player(side) => {
            if let Some(t) = controller.set_player(side, board, now)? {
                opponent.ask(t);
            }
            Ok(format!("You play {}.", side.name()))
        }
        Change::Button(b) => {
            if let Some(t) = controller.on_button(ClockButton::Button(b), board, now) {
                opponent.ask(t);
            }
            Ok(format!("button {b}"))
        }
        Change::Time(time) => {
            let label = time.map_or_else(|| "untimed".to_owned(), |t| t.to_string());
            Ok(if controller.set_time(time, board, now) {
                format!("This game is {label}.")
            } else {
                format!("The next game is {label}.")
            })
        }
    }
}

fn play_online<L: Link>(
    link: &mut L,
    connection: &Connection,
    o: &Options,
    view: &watch::Sender<View>,
) -> io::Result<()> {
    let waiting = game_id(o.seed, 0);
    let created = now_ms();
    let mut online = OnlinePlay::new("", o.white_on_left);
    let mut signed_in = false;
    publish(view, View::online(&online, link, &waiting, created, Instant::now()));
    let requests = serve_web(o, view)?;
    loop {
        if o.stopped() {
            return Ok(());
        }
        let now = Instant::now();
        for input in link.exchange(now)? {
            match input {
                Input::Event(e) => {
                    note(&e, link);
                    online.on_event(&e, link);
                }
                Input::Button(b) => online.on_button(b, link),
            }
        }
        for m in connection.messages() {
            match m {
                FromServer::Account(id) => {
                    log::line(
                        Source::Online,
                        format!("signed in as {id}; start a game on the site or the app"),
                    );
                    online = OnlinePlay::new(&id, o.white_on_left);
                    signed_in = true;
                }
                FromServer::Full(f) if signed_in => {
                    online.on_full(&f, link, now);
                    if let Some(g) = online.game() {
                        log::line(
                            Source::Online,
                            format!("game {} against {}, you play {}", g.id, g.opponent, g.side.name()),
                        );
                    }
                }
                FromServer::State(st) if signed_in => {
                    log::line(
                        Source::Online,
                        format!("{} {}", st.status.name(), st.moves.split(' ').next_back().unwrap_or("")),
                    );
                    online.on_state(&st, link, now);
                }
                FromServer::Refused(why) => {
                    log::line(Source::Online, format!("move refused: {why}"));
                    online.on_refused(link);
                }
                FromServer::Problem(why) => log::line(Source::Online, why),
                FromServer::Full(_) | FromServer::State(_) => {}
            }
        }
        for r in online.take_requests() {
            log::line(Source::Online, format!("{r:?}"));
            connection.send(r);
        }
        while let Ok(r) = requests.try_recv() {
            let reply = match r.change {
                Change::Button(b) => {
                    online.on_button(ClockButton::Button(b), link);
                    Ok(format!("button {b}"))
                }
                _ => Err("playing online: set the game up on the site".into()),
            };
            let _ = r.reply.send(reply);
        }
        publish(view, View::online(&online, link, &waiting, created, Instant::now()));
    }
}

fn play_livechess<L: Link>(
    link: &mut L,
    addr: &str,
    o: &Options,
    view: &watch::Sender<View>,
) -> io::Result<()> {
    let mut lc = LiveChess::new();
    let (feed_tx, feed_rx) = watch::channel(lc.feed(link));
    let (setup_tx, setups) = mpsc::channel::<String>();
    let (log_tx, lines) = mpsc::channel::<String>();
    let at = livechess::serve(addr, feed_rx, setup_tx, log_tx)
        .map_err(|e| io::Error::other(format!("listening on {addr}: {e}")))?;
    log::line(
        Source::LiveChess,
        format!("ws://{at}/api/v1.0: open lichess.org/dgt in a browser on this computer"),
    );
    let waiting = game_id(o.seed, 0);
    let created = now_ms();
    let title = "Lichess (DGT page)";
    publish(view, View::board_only(link, &waiting, title, created, Instant::now()));
    let requests = serve_web(o, view)?;
    loop {
        if o.stopped() {
            return Ok(());
        }
        let now = Instant::now();
        for input in link.exchange(now)? {
            if let Input::Event(e) = input {
                note(&e, link);
                if matches!(e, Event::Game(Observation::Caught)) {
                    link.send(Command::LightsOff);
                }
            }
        }
        for fen in setups.try_iter() {
            log::line(Source::LiveChess, format!("setup {fen}"));
            lc.setup(&fen, link);
        }
        for line in lines.try_iter() {
            log::line(Source::LiveChess, line);
        }
        while let Ok(r) = requests.try_recv() {
            let _ = r.reply.send(Err("the Lichess DGT page runs this game".into()));
        }
        let f = lc.feed(link);
        feed_tx.send_if_modified(|old| {
            if *old == f {
                false
            } else {
                *old = f;
                true
            }
        });
        publish(view, View::board_only(link, &waiting, title, created, Instant::now()));
    }
}
