//! Playing on a board: against the engine, online, or as the board for
//! Lichess's DGT page. Each loop drives a [`Link`], publishes the game as
//! a [`View`] (for the app, PicoChess's tools and the terminal screen) and
//! takes settings from the web.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use chess_analysis::book::Book;
use chess_dgt::{ClockButton, Command, Event, Observation};
use tokio::sync::watch;

use crate::board::Board;
use crate::controller::{Controller, Switch, describe};
use crate::links::{Input, Link};
use crate::livechess::{self, LiveChess};
use crate::log::{self, Source};
use crate::online::{Connection, FromServer, OnlinePlay};
use crate::opponent::{EngineSpec, Opponent};
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

/// An engine the clock's menu offers.
#[derive(Debug, Clone)]
pub struct EngineChoice {
    pub name: String,
    pub spec: EngineSpec,
}

impl EngineChoice {
    pub fn info(&self) -> EngineInfo {
        EngineInfo { name: matches!(self.spec, EngineSpec::Uci { .. }).then(|| self.name.clone()) }
    }
}

/// The books and engines behind the menu's choices ([`crate::menu::Choices`]).
#[derive(Debug, Clone, Default)]
pub struct Lists {
    pub books: Vec<PathBuf>,
    pub engines: Vec<EngineChoice>,
}

/// Read a Polyglot book.
pub fn load_book(path: &Path) -> anyhow::Result<Book> {
    let bytes = std::fs::read(path).with_context(|| format!("reading the book {}", path.display()))?;
    let book = Book::from_bytes(&bytes).with_context(|| path.display().to_string())?;
    log::line(Source::Engine, format!("book: {}, {} entries", path.display(), book.len()));
    Ok(book)
}

/// Who gets rated when a game against the engine, or between two people
/// on the board, ends ([`crate::ratings`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rating {
    /// Not rated (a UCI engine: its strength isn't known).
    None,
    /// This person against open-chess's engine (at the level of the time).
    Against(String),
    /// Two people: White's name and Black's.
    Pair(String, String),
}

/// Who the board plays.
pub enum Mode {
    Engine {
        controller: Box<Controller>,
        opponent: Opponent,
        engine: EngineInfo,
        lists: Lists,
        rating: Rating,
    },
    Online {
        connection: Connection,
    },
    LiveChess {
        addr: String,
    },
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

/// Serve the app and the APIs (with `listen`), for as long as the program
/// runs: the settings clients ask for come on the receiver, for whichever
/// game is being played.
pub fn serve_web(o: &Options, view: &watch::Sender<View>) -> io::Result<mpsc::Receiver<Request>> {
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

/// Play `mode` on a board until the board fails or `o.stop` is set,
/// publishing on `view`; the web's requests (from [`serve_web`]) come on
/// `requests`.
pub fn drive<L: Link>(
    link: &mut L,
    mode: Mode,
    o: &Options,
    view: &watch::Sender<View>,
    requests: &mpsc::Receiver<Request>,
) -> io::Result<()> {
    match mode {
        Mode::Engine { controller, opponent, engine, lists, rating } => {
            play_engine(link, *controller, opponent, engine, &lists, &rating, o, view, requests)
        }
        Mode::Online { connection } => play_online(link, &connection, o, view, requests),
        Mode::LiveChess { addr } => play_livechess(link, &addr, o, view, requests),
    }
}

/// The names to show for a game against the engine or between two people.
fn names(rating: &Rating, controller: &Controller, engine: &EngineInfo) -> Option<[String; 2]> {
    match rating {
        Rating::Pair(w, b) => Some([w.clone(), b.clone()]),
        Rating::Against(me) => {
            let them =
                engine.name.clone().unwrap_or_else(|| format!("open-chess level {}", controller.level()));
            Some(if controller.player() == chess_core::Side::White {
                [me.clone(), them]
            } else {
                [them, me.clone()]
            })
        }
        Rating::None => None,
    }
}

/// Rate a game that has just ended, if it counts.
fn rate(rating: &Rating, v: &View, level: u8) -> Option<[crate::ratings::PlayerResult; 2]> {
    use crate::ratings::{List, PlayerResult, Store, engine_rating, white_score};
    let score = white_score(v.status, v.winner, v.moves.len())?;
    let list = List::of(v.time);
    let mut kept = Store::open();
    let results = match rating {
        Rating::None => return None,
        Rating::Pair(w, b) => kept.rate_pair(w, b, list, score),
        Rating::Against(me) => {
            let engine = PlayerResult {
                name: format!("open-chess level {level}"),
                rating: Some(engine_rating(level)),
                diff: None,
            };
            if v.player == chess_core::Side::White {
                [kept.rate_against(me, list, engine_rating(level), score), engine]
            } else {
                [engine, kept.rate_against(me, list, engine_rating(level), 1.0 - score)]
            }
        }
    };
    kept.save();
    log::line(
        Source::Game,
        format!("{} rating: {} · {}", list.key(), results[0].describe(), results[1].describe()),
    );
    Some(results)
}

#[allow(clippy::too_many_arguments)]
fn play_engine<L: Link>(
    link: &mut L,
    mut controller: Controller,
    mut opponent: Opponent,
    mut engine: EngineInfo,
    lists: &Lists,
    rating: &Rating,
    o: &Options,
    view: &watch::Sender<View>,
    requests: &mpsc::Receiver<Request>,
) -> io::Result<()> {
    let mut game = (controller.games(), game_id(o.seed, 0), now_ms());
    // The ratings once this game is over (rated once).
    let mut result: Option<Option<[crate::ratings::PlayerResult; 2]>> = None;
    let make_view = |controller: &Controller, link: &L, engine: &EngineInfo, game: &(u32, String, i64)| {
        let mut v = View::new(controller, link, engine, game.1.clone(), game.2, Instant::now());
        v.names = names(rating, controller, engine);
        v
    };
    publish(view, make_view(&controller, link, &engine, &game));
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
            let result = apply(r.change, &mut controller, link, &opponent);
            replies.push((r.reply, result));
        }
        if let Some(switch) = controller.take_switch() {
            switch_to(switch, lists, &mut controller, link, &mut opponent, &mut engine);
        }
        controller.tick(link, Instant::now());
        if controller.games() != game.0 {
            game = (controller.games(), game_id(o.seed, controller.games()), now_ms());
            result = None;
        }
        let mut v = make_view(&controller, link, &engine, &game);
        // Rated once, when it ends (a takeback after that doesn't rate it
        // again).
        if !v.started() && v.status != chess_core::Status::Created && !v.awaiting_board {
            let r = result.get_or_insert_with(|| rate(rating, &v, controller.level()));
            v.ratings.clone_from(r);
        }
        publish(view, v);
        // Answered once the change is in what the clients read.
        for (tx, result) in replies {
            let _ = tx.send(result);
        }
    }
}

/// Load the book or start the engine picked in the clock's menu.
fn switch_to(
    switch: Switch,
    lists: &Lists,
    controller: &mut Controller,
    board: &mut impl Board,
    opponent: &mut Opponent,
    engine: &mut EngineInfo,
) {
    match switch {
        Switch::Book(None) => controller.set_book(None, None, board),
        Switch::Book(Some(i)) => match lists.books.get(i).map(|p| load_book(p)) {
            Some(Ok(book)) => controller.set_book(Some(book), Some(i), board),
            Some(Err(e)) => {
                log::line(Source::Engine, format!("{e:#}"));
                board.send(Command::ClockText { text: "bad book".into(), beep: 1 });
            }
            None => {}
        },
        Switch::Engine(i) => {
            let Some(choice) = lists.engines.get(i) else { return };
            log::line(Source::Engine, format!("starting {}", choice.name));
            match Opponent::start(choice.spec.clone()) {
                Ok(started) => {
                    *opponent = started;
                    *engine = choice.info();
                    if let Some(t) = controller.engine_changed(i, board, Instant::now()) {
                        opponent.ask(t);
                    }
                }
                Err(e) => {
                    log::line(Source::Engine, format!("{}: {e:#}", choice.name));
                    board.send(Command::ClockText { text: "no engine".into(), beep: 1 });
                }
            }
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
    requests: &mpsc::Receiver<Request>,
) -> io::Result<()> {
    let waiting = game_id(o.seed, 0);
    let created = now_ms();
    let mut online = OnlinePlay::new("", o.white_on_left);
    let mut signed_in = false;
    // The last finished game's ratings, by game id.
    let mut ratings: Option<(String, [crate::ratings::PlayerResult; 2])> = None;
    publish(view, View::online(&online, link, &waiting, created, Instant::now()));
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
                FromServer::Ratings { game, players } => {
                    log::line(
                        Source::Online,
                        format!("game {game}: {} · {}", players[0].describe(), players[1].describe()),
                    );
                    ratings = Some((game, players));
                }
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
        let mut v = View::online(&online, link, &waiting, created, Instant::now());
        if let Some((id, r)) = &ratings
            && *id == v.game_id
            && !v.started()
        {
            v.ratings = Some(r.clone());
        }
        publish(view, v);
    }
}

fn play_livechess<L: Link>(
    link: &mut L,
    addr: &str,
    o: &Options,
    view: &watch::Sender<View>,
    requests: &mpsc::Receiver<Request>,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chess_core::{Game, Side, Status};
    use chess_dgt::Placement;

    use super::*;
    use crate::controller::Setup;
    use crate::links::{ChessnutLink, Wire};

    /// A Chessnut board that shows each position for a while, sending it
    /// over and over as the GO does.
    struct Scripted {
        positions: Vec<(Vec<u8>, Duration)>,
        started: Instant,
    }

    impl Wire for Scripted {
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn read(&mut self, _: Duration) -> io::Result<Vec<Vec<u8>>> {
            std::thread::sleep(Duration::from_millis(20));
            let mut t = self.started.elapsed();
            for (frame, hold) in &self.positions {
                if t < *hold {
                    return Ok(vec![frame.clone()]);
                }
                t -= *hold;
            }
            Ok(self.positions.last().map(|(f, _)| vec![f.clone()]).unwrap_or_default())
        }
    }

    fn frame(moves: &str) -> Vec<u8> {
        let g = Game::from_moves(None, moves).unwrap();
        chess_chessnut::protocol::position_bytes(&Placement::from_fen(&g.fen()).unwrap())
    }

    #[test]
    fn two_players_play_to_mate_and_are_rated() {
        let dir = std::env::temp_dir().join(format!("oc-play-{}", std::process::id()));
        crate::ratings::TEST_PATH.with(|p| *p.borrow_mut() = Some(dir.join("ratings.json")));
        let hold = Duration::from_millis(600);
        let positions = ["", "f2f3", "f2f3 e7e5", "f2f3 e7e5 g2g4", "f2f3 e7e5 g2g4 d8h4"]
            .iter()
            .map(|m| (frame(m), hold))
            .collect();
        let mut link =
            ChessnutLink::over(Box::new(Scripted { positions, started: Instant::now() }), Instant::now());
        let controller = Controller::new(Setup { two_players: true, ..Setup::default() });
        let mode = Mode::Engine {
            controller: Box::new(controller),
            opponent: Opponent::start(EngineSpec::Builtin).unwrap(),
            engine: EngineInfo { name: None },
            lists: Lists::default(),
            rating: Rating::Pair("Ann".into(), "Bob".into()),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let o = Options {
            white_on_left: true,
            listen: None,
            app: None,
            name: "test".into(),
            seed: 1,
            stop: stop.clone(),
        };
        let (view, mut seen) = watch::channel(View::waiting("test"));
        let (_tx, requests) = mpsc::channel();
        let watcher = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                if seen.borrow_and_update().ratings.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            stop.store(true, Ordering::SeqCst);
            seen.borrow().clone()
        });
        drive(&mut link, mode, &o, &view, &requests).unwrap();
        let v = watcher.join().unwrap();
        assert_eq!((v.status, v.winner), (Status::Mate, Some(Side::Black)));
        assert_eq!(v.status_line(), "game over: Black wins by checkmate");
        assert_eq!(v.name_of(Side::White), "Ann");
        let r = v.ratings.clone().expect("rated");
        assert_eq!(r[0].describe(), "Ann 1500 → 1480 (-20)");
        assert_eq!(r[1].describe(), "Bob 1500 → 1520 (+20)");
        let words = v.moves_in_words();
        assert_eq!(words.len(), 4);
        assert_eq!(words[3].0, "2…");
        assert!(words[3].2.to_lowercase().contains("checkmate"), "{:?}", words[3]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
