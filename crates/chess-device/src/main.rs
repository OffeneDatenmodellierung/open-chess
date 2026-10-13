//! `open-chess-device`: play on an electronic board.
//!
//! ```sh
//! open-chess-device                                       # a screen to choose a board
//! open-chess-device --board /dev/ttyACM0                  # open-chess's engine, level 4
//! open-chess-device --board /dev/ttyACM0 --level 7 --book gm2001.bin
//! open-chess-device --board /dev/rfcomm0 --uci /usr/games/stockfish --move-time 2 \
//!     --uci-option "Skill Level=5" --play black --time 5+3
//! open-chess-device --board /dev/ttyACM0 --listen 0.0.0.0:8080 --app dist/chess
//! sudo open-chess-device --board /dev/rfcomm0 --clock dgtpi   # on a DGT Pi
//! open-chess-device --centaur                                 # on a DGT Centaur
//! open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board chessnut   # Chessnut Air / Go
//! open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board pegasus    # DGT Pegasus
//! open-chess-device --usb /dev/hidraw0                        # a Chessnut board on USB
//! OPEN_CHESS_TOKEN=lip_… open-chess-device --board /dev/ttyACM0 --online   # on lichess.org
//! open-chess-device --board /dev/ttyACM0 --livechess          # for lichess.org/dgt
//! ```
//!
//! With no board named (or `--tui`), a terminal screen lists the boards
//! found to choose from, then shows the game and a log, newest first.
//! With `--listen`, a phone on the same network opens the device's address
//! and gets the Chess lab app showing the game, with the device's settings.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chess_analysis::book::Book;
use chess_core::Side;
use chess_dgtpi::driver::Clock;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};
use open_chess_device::board::Board;
use open_chess_device::choice::{GameChoice, Site, Start};
use open_chess_device::controller::{Controller, Setup, TimeControl};
use open_chess_device::discover::{self, BleKind, Found};
use open_chess_device::links::Link;
use open_chess_device::links::{AnyLink, CentaurLink, ChessnutLink, DgtLink, PegasusLink};
use open_chess_device::log::{self, Source};
use open_chess_device::menu::Choices;
use open_chess_device::online::{Challenge, Connection, Opening};
use open_chess_device::opponent::{EngineSpec, Opponent};
use open_chess_device::play::{self, Mode, Options, Rating};
use open_chess_device::settings::Settings;
use open_chess_device::view::View;
use open_chess_device::{ble, picoini, tui};
use tokio::sync::watch;

#[derive(Debug, Clone, Parser)]
#[allow(clippy::struct_excessive_bools)] // command-line switches
#[command(name = "open-chess-device", version, about = "Play chess on an electronic board")]
struct Args {
    /// A DGT board's port: /dev/ttyACM0 (USB, Linux), /dev/cu.usbmodem… (macOS),
    /// COM3 (Windows), /dev/rfcomm0 (Bluetooth, after `rfcomm bind`); with
    /// --centaur, its controller's port (default /dev/serial0).
    #[arg(long)]
    board: Option<String>,

    /// Running on a DGT Centaur: its board controller, lights and buttons.
    #[arg(long)]
    centaur: bool,

    /// A Bluetooth LE board's address (from the screen, or `bluetoothctl`;
    /// on a Mac, the id macOS gives it).
    #[arg(long)]
    ble: Option<String>,

    /// Which kind of Bluetooth LE board.
    #[arg(long, default_value = "chessnut", value_parser = ["chessnut", "pegasus"])]
    ble_board: String,

    /// The board uses a random (static) Bluetooth address (Linux).
    #[arg(long)]
    random_address: bool,

    /// A Chessnut board on USB: its hidraw node (Linux), e.g. /dev/hidraw0.
    #[arg(long)]
    usb: Option<String>,

    /// Show the terminal screen (the default when no board is named).
    #[arg(long)]
    tui: bool,

    /// On a Centaur, leave its e-paper screen alone.
    #[arg(long)]
    no_screen: bool,

    /// Take settings from PicoChess's picochess.ini (the board's port, the
    /// DGT Pi's clock, engine, level, book, time control, web port, clock
    /// side); options given here win.
    #[arg(long)]
    picochess_ini: Option<std::path::PathBuf>,

    /// open-chess's engine at this level, 1 (beginner) to 8 (the clock's
    /// buttons change it during play).
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=8))]
    level: u8,

    /// A UCI engine instead (Stockfish, Leela, …).
    #[arg(long)]
    uci: Option<String>,

    /// Seconds per move for the UCI engine (without --time).
    #[arg(long, default_value_t = 2.0)]
    move_time: f64,

    /// UCI engine options, NAME=VALUE (repeat for more).
    #[arg(long = "uci-option")]
    uci_options: Vec<String>,

    /// A Polyglot opening book (.bin) for the engine's opening moves.
    #[arg(long)]
    book: Option<String>,

    /// Books the clock's menu offers: every .bin here (default: the
    /// folder of --book).
    #[arg(long)]
    books: Option<std::path::PathBuf>,

    /// UCI engines the clock's menu offers besides open-chess's own: every
    /// program here (with --picochess-ini: the folder of its engine).
    #[arg(long)]
    engines: Option<std::path::PathBuf>,

    /// The side you play.
    #[arg(long, default_value = "white", value_parser = ["white", "black"])]
    play: String,

    /// Play to a time control, minutes+increment (e.g. 5+3), shown on the
    /// DGT3000. The engine then plays to the clock too.
    #[arg(long)]
    time: Option<TimeControl>,

    /// Which side of the clock is White's.
    #[arg(long, default_value = "left", value_parser = ["left", "right"])]
    white_clock: String,

    /// Where the clock is: on the board's cable, a DGT Pi's own clock
    /// (I²C, needs root), or a simulated one (for trying it out).
    #[arg(long, default_value = "board", value_parser = ["board", "dgtpi", "sim"])]
    clock: String,

    /// Serve the app and the device's APIs here, e.g. 0.0.0.0:8080 (every
    /// network the device is on: a home network, not the internet).
    #[arg(long)]
    listen: Option<String>,

    /// The Chess lab app's files (a build of web/chess), served at /app/.
    #[arg(long)]
    app: Option<std::path::PathBuf>,

    /// The device's name, shown in the app.
    #[arg(long, default_value = "open-chess board")]
    name: String,

    /// Play online instead of the engine: on lichess.org, or the open-chess
    /// server at this address. Games started on the site or the app (a
    /// seek, a challenge accepted) are played on the board.
    #[arg(long, num_args = 0..=1, default_missing_value = "https://lichess.org")]
    online: Option<String>,

    /// The account's API token for --online (with the board:play scope;
    /// challenge:write too for --challenge).
    #[arg(long, env = "OPEN_CHESS_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// With --online: challenge this player at the start (to --time, playing
    /// --play's colour).
    #[arg(long)]
    challenge: Option<String>,

    /// Be the board for Lichess's DGT page (lichess.org/dgt) instead of
    /// playing the engine: answer DGT LiveChess's websocket here (the page
    /// looks on localhost:1982, on the computer its browser runs on).
    #[arg(long, num_args = 0..=1, default_missing_value = "127.0.0.1:1982")]
    livechess: Option<String>,

    /// Log more for finding faults (every Bluetooth message the board
    /// sends and receives, and what it offers), and write the log to a file:
    /// --log-file, or open-chess-device.log in the current folder.
    #[arg(long, env = "OPEN_CHESS_DEBUG")]
    debug: bool,

    /// Also write the log to this file (started afresh each run).
    #[arg(long)]
    log_file: Option<std::path::PathBuf>,
}

fn main() -> ExitCode {
    let matches = Args::command().get_matches();
    let args = match Args::from_arg_matches(&matches) {
        Ok(a) => {
            log::debug_on(a.debug);
            let file = a.log_file.clone().or_else(|| a.debug.then(|| "open-chess-device.log".into()));
            if let Some(path) = file {
                match log::to_file(&path) {
                    Ok(()) => log::line(Source::Device, format!("logging to {}", path.display())),
                    Err(e) => eprintln!("can't write the log to {}: {e}", path.display()),
                }
            }
            a
        }
        Err(e) => e.exit(),
    };
    let args = match &args.picochess_ini {
        Some(path) => match picoini::read(path) {
            Ok(ini) => with_ini(args.clone(), &matches, &ini),
            Err(e) => {
                eprintln!("open-chess-device: reading {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => args,
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("open-chess-device: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Fill in what PicoChess's settings say, where the command line didn't.
fn with_ini(mut args: Args, m: &ArgMatches, ini: &picoini::Settings) -> Args {
    let given =
        |id: &str| matches!(m.value_source(id), Some(ValueSource::CommandLine | ValueSource::EnvVariable));
    if !given("board") && !args.centaur && args.ble.is_none() && args.usb.is_none() {
        args.board = ini.dgt_port.clone().or(args.board);
    }
    if ini.dgtpi && !given("clock") {
        args.clock = "dgtpi".into();
    }
    if !given("uci") && ini.engine.is_some() {
        args.uci.clone_from(&ini.engine);
        if !given("engines") {
            args.engines =
                ini.engine.as_deref().and_then(|e| std::path::Path::new(e).parent()).map(Into::into);
        }
        if !given("uci_options") {
            args.uci_options.clone_from(&ini.uci_options);
        }
    }
    if !given("book") && ini.book.is_some() {
        args.book.clone_from(&ini.book);
    }
    if !given("time") && ini.time.is_some() {
        args.time = ini.time;
    }
    if !given("listen") && ini.listen.is_some() {
        args.listen.clone_from(&ini.listen);
    }
    if !given("white_clock")
        && let Some(side) = &ini.clockside
    {
        args.white_clock.clone_from(side);
    }
    log::line(Source::Device, format!("PicoChess settings read; not used here: {}", ini.ignored.join(", ")));
    args
}

fn engine_spec(args: &Args) -> Result<EngineSpec> {
    let Some(path) = &args.uci else { return Ok(EngineSpec::Builtin) };
    let mut options = Vec::new();
    for o in &args.uci_options {
        let Some((k, v)) = o.split_once('=') else { bail!("--uci-option {o}: expected NAME=VALUE") };
        options.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    if !(args.move_time > 0.0 && args.move_time.is_finite()) {
        bail!("--move-time must be a positive number of seconds");
    }
    Ok(EngineSpec::Uci { path: path.clone(), move_time: Duration::from_secs_f64(args.move_time), options })
}

fn load_book(args: &Args) -> Result<Option<Book>> {
    let Some(p) = &args.book else { return Ok(None) };
    Ok(Some(play::load_book(Path::new(p))?))
}

fn stem(p: &Path) -> String {
    p.file_stem().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// The books for the menu (sorted), and which one --book is.
fn book_list(args: &Args) -> (Vec<PathBuf>, Option<usize>) {
    let book = args.book.as_deref().map(Path::new);
    let dir = args.books.clone().or_else(|| {
        book.and_then(Path::parent)
            .map(|d| if d.as_os_str().is_empty() { PathBuf::from(".") } else { d.to_path_buf() })
    });
    let mut books: Vec<PathBuf> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bin")))
        .collect();
    books.sort();
    let at = book.map(|b| {
        books.iter().position(|p| same_file(p, b)).unwrap_or_else(|| {
            books.push(b.to_path_buf());
            books.len() - 1
        })
    });
    (books, at)
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// A program to run: executable on Unix, an .exe on Windows.
fn runnable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.extension().is_none()
            && p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))
    }
}

/// The engines for the menu (open-chess's first), and which one plays.
fn engine_list(args: &Args) -> Result<(Vec<play::EngineChoice>, usize)> {
    let mut engines = vec![play::EngineChoice { name: "open-chess".into(), spec: EngineSpec::Builtin }];
    let chosen = engine_spec(args)?;
    let mut found: Vec<PathBuf> = args
        .engines
        .as_ref()
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| runnable(p))
        .collect();
    found.sort();
    let EngineSpec::Uci { path: uci, move_time, options } = &chosen else {
        engines.extend(found.iter().map(|p| play::EngineChoice {
            name: stem(p),
            spec: EngineSpec::Uci {
                path: p.display().to_string(),
                move_time: Duration::from_secs_f64(args.move_time.max(0.1)),
                options: Vec::new(),
            },
        }));
        return Ok((engines, 0));
    };
    let mut at = 0;
    for p in &found {
        let this = same_file(p, Path::new(uci));
        engines.push(play::EngineChoice {
            name: stem(p),
            spec: EngineSpec::Uci {
                path: p.display().to_string(),
                move_time: *move_time,
                options: if this { options.clone() } else { Vec::new() },
            },
        });
        if this {
            at = engines.len() - 1;
        }
    }
    if at == 0 {
        engines.push(play::EngineChoice { name: stem(Path::new(uci)), spec: chosen.clone() });
        at = engines.len() - 1;
    }
    Ok((engines, at))
}

/// The game the command line asks for (when no screen chooses one).
fn choice_from_args(args: &Args) -> Result<GameChoice> {
    let play = if args.play == "black" { Side::Black } else { Side::White };
    if let Some(addr) = &args.livechess {
        return Ok(GameChoice::DgtPage { addr: addr.clone() });
    }
    if let Some(base) = &args.online {
        let Some(token) = &args.token else {
            bail!("--online needs the account's API token: --token or OPEN_CHESS_TOKEN")
        };
        let site = if base.trim_end_matches('/') == "https://lichess.org" {
            Site::Lichess
        } else {
            Site::Server(base.clone())
        };
        let start = args.challenge.clone().map_or(Start::Wait, Start::Challenge);
        return Ok(GameChoice::Online {
            site,
            token: token.clone(),
            start,
            play: Some(play),
            time: args.time,
            rated: false,
        });
    }
    let saved = Settings::load();
    let name = if saved.name.is_empty() { open_chess_device::settings::login_name() } else { saved.name };
    Ok(GameChoice::Computer { name, level: args.level, play, time: args.time })
}

/// The name of the engine "the computer" is, for the screen.
fn engine_label(args: &Args) -> Option<String> {
    args.uci.as_deref().map(|p| stem(Path::new(p)))
}

/// Who the board plays, for one game.
fn mode(args: &Args, choice: &GameChoice, seed: u64) -> Result<Mode> {
    let request =
        |play: Option<Side>, time: Option<TimeControl>, rated: bool| chess_client::ChallengeRequest {
            rated,
            limit_secs: time.map(|t| t.minutes * 60),
            increment_secs: time.map(|t| t.increment_secs),
            color: Some(play.map_or("random", Side::name).to_owned()),
            ..Default::default()
        };
    let (player, level, time, rating, two_players) = match choice {
        GameChoice::DgtPage { addr } => return Ok(Mode::LiveChess { addr: addr.clone() }),
        GameChoice::Online { site, token, start, play, time, rated } => {
            let opening = match start {
                Start::Wait => Opening::Wait,
                Start::Challenge(who) => Opening::Challenge(Challenge {
                    opponent: who.clone(),
                    request: request(*play, *time, *rated),
                }),
                Start::Computer(level) => {
                    Opening::Computer { level: *level, request: request(*play, *time, false) }
                }
                Start::Resume(id) => Opening::Resume(id.clone()),
                Start::Accept(id) => Opening::Accept(id.clone()),
            };
            log::line(Source::Online, format!("playing online at {}", site.base()));
            return Ok(Mode::Online { connection: Connection::spawn(site.base(), token, opening)? });
        }
        GameChoice::Computer { name, level, play, time } => {
            let rated = args.uci.is_none() && !name.trim().is_empty();
            let rating = if rated { Rating::Against(name.trim().to_owned()) } else { Rating::None };
            (*play, *level, *time, rating, false)
        }
        GameChoice::TwoPlayers { white, black, time } => {
            let name =
                |n: &str, d: &str| if n.trim().is_empty() { d.to_owned() } else { n.trim().to_owned() };
            (Side::White, args.level, *time, Rating::Pair(name(white, "White"), name(black, "Black")), true)
        }
    };
    let (engines, engine_at) = engine_list(args)?;
    let (books, book_at) = book_list(args);
    let opponent = Opponent::start(engines[engine_at].spec.clone())?;
    let engine = engines[engine_at].info();
    let choices = Choices {
        books: books.iter().map(|p| stem(p)).collect(),
        book: book_at,
        engines: engines.iter().map(|e| e.name.clone()).collect(),
        engine: engine_at,
    };
    let controller = Controller::new(Setup {
        player,
        level,
        book: load_book(args)?,
        time,
        white_on_left: args.white_clock == "left",
        seed,
        choices,
        two_players,
    });
    Ok(Mode::Engine {
        controller: Box::new(controller),
        opponent,
        engine,
        lists: play::Lists { books, engines },
        rating,
    })
}

/// The DGT Pi's own clock, or a simulated one; `None` for a clock on the
/// board's cable.
fn open_clock(kind: &str) -> Result<Option<Clock>> {
    let clock = match kind {
        #[cfg(target_os = "linux")]
        "dgtpi" => Clock::spawn(chess_dgtpi::bcm::BcmBus::open().context("the DGT Pi's clock")?)?,
        #[cfg(not(target_os = "linux"))]
        "dgtpi" => bail!("--clock dgtpi: the DGT Pi's clock needs Linux on the DGT Pi"),
        "sim" => Clock::spawn(chess_dgtpi::sim::SimClock::off())?,
        _ => return Ok(None),
    };
    Ok(Some(clock))
}

/// The board the command line names, if any.
fn named(args: &Args) -> Option<Found> {
    if let Some(address) = &args.ble {
        let kind = if args.ble_board == "pegasus" { BleKind::Pegasus } else { BleKind::Chessnut };
        return Some(Found::Ble {
            address: address.clone(),
            name: args.ble_board.clone(),
            kind,
            random: args.random_address,
        });
    }
    if let Some(path) = &args.usb {
        return Some(Found::ChessnutUsb { path: path.clone(), label: "Chessnut".into() });
    }
    if args.centaur {
        return Some(Found::Centaur { path: args.board.clone().unwrap_or_else(|| "/dev/serial0".into()) });
    }
    args.board.as_ref().map(|p| Found::Serial { path: p.clone(), label: "board".into() })
}

fn open(found: &Found, args: &Args) -> Result<AnyLink> {
    let now = Instant::now();
    Ok(match found {
        Found::Serial { path, .. } => AnyLink::Dgt(
            DgtLink::open(path, open_clock(&args.clock)?, now).with_context(|| format!("opening {path}"))?,
        ),
        Found::Centaur { path } => {
            AnyLink::Centaur(CentaurLink::open(path, now).with_context(|| format!("opening {path}"))?)
        }
        Found::ChessnutUsb { path, .. } => {
            AnyLink::Chessnut(ChessnutLink::usb(path, now).with_context(|| format!("opening {path}"))?)
        }
        Found::Ble { address, kind: BleKind::Chessnut, random, .. } => AnyLink::Chessnut(
            ChessnutLink::bluetooth(address, *random, now)
                .with_context(|| format!("connecting to {address}"))?,
        ),
        Found::Ble { address, kind: BleKind::Pegasus, random, .. } => AnyLink::Pegasus(
            PegasusLink::bluetooth(address, *random, now)
                .with_context(|| format!("connecting to {address}"))?,
        ),
    })
}

/// Where a board's games come from: one, from the command line, or one
/// after another from the screen (each with a flag that ends it).
enum Games {
    Once(GameChoice),
    Chosen(mpsc::Receiver<(GameChoice, Arc<AtomicBool>)>),
}

/// Connect to a board and play on it until it fails or `stop` is set.
fn play_on(
    found: &Found,
    args: &Args,
    seed: u64,
    stop: &Arc<AtomicBool>,
    view: &watch::Sender<View>,
    games: &Games,
) -> Result<()> {
    log::line(Source::Device, format!("connecting to {}", found.describe()));
    let mut link = open(found, args)?;
    #[cfg(target_os = "linux")]
    if matches!(found, Found::Centaur { .. }) && !args.no_screen {
        open_chess_device::epaper::start(view.subscribe());
    }
    log::line(Source::Device, format!("connected to {}", found.describe()));
    let o = Options {
        white_on_left: args.white_clock == "left",
        listen: args.listen.clone(),
        app: args.app.clone(),
        name: args.name.clone(),
        seed,
        stop: stop.clone(),
    };
    let requests = play::serve_web(&o, view)?;
    let mut first = true;
    let mut once = match games {
        Games::Once(c) => Some((c.clone(), stop.clone())),
        Games::Chosen(_) => None,
    };
    loop {
        let (choice, game_stop) = match games {
            Games::Once(_) => match once.take() {
                Some(next) => next,
                None => return Ok(()),
            },
            Games::Chosen(rx) => loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(next) => break next,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Keep the board's messages flowing while choosing.
                        link.exchange(Instant::now())?;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            },
        };
        if !first {
            // A fresh game: the pieces go back to the start.
            if let Ok(g) = chess_core::Game::new(None) {
                let _ = link.set_position(&g.fen());
            }
        }
        first = false;
        log::line(Source::Device, format!("playing {}", choice.describe()));
        let mode = match mode(args, &choice, seed) {
            Ok(m) => m,
            Err(e) => {
                log::line(Source::Device, format!("{e:#}"));
                if matches!(games, Games::Once(_)) {
                    return Err(e);
                }
                continue;
            }
        };
        let o = Options { stop: game_stop, ..o.clone() };
        play::drive(&mut link, mode, &o, view, &requests)?;
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

/// Every board there is: ports, Chessnut boards on USB, Bluetooth LE.
fn scan_all() -> Vec<Found> {
    let mut found = discover::serial_ports();
    found.extend(discover::chessnut_usb());
    match ble::scan(6) {
        Ok(b) => found.extend(b),
        Err(e) => log::line(Source::Device, format!("Bluetooth: {e}")),
    }
    found
}

fn run(args: Args) -> Result<()> {
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    let (view_tx, view_rx) = watch::channel(View::waiting("no board yet"));
    let view_tx = Arc::new(view_tx);
    let target = named(&args);
    let screen = args.tui || (target.is_none() && std::io::stdout().is_terminal());
    if !screen {
        let Some(t) = target else {
            bail!("name a board (--board, --centaur, --ble, --usb), or run in a terminal to choose one")
        };
        let choice = choice_from_args(&args)?;
        return play_on(&t, &args, seed, &Arc::new(AtomicBool::new(false)), &view_tx, &Games::Once(choice));
    }
    // A board named on the command line starts straight away with the
    // command line's game; otherwise the screen asks.
    let first = if target.is_some() { Some(choice_from_args(&args)?) } else { None };
    let defaults = tui::Defaults {
        settings: Settings::load(),
        level: args.level,
        play: if args.play == "black" { Side::Black } else { Side::White },
        time: args.time,
        engine: engine_label(&args),
        lichess_token: args
            .token
            .clone()
            .filter(|_| args.online.as_deref().is_none_or(|b| b.contains("lichess.org"))),
        server: args.online.clone().filter(|b| !b.contains("lichess.org")),
        server_token: args
            .token
            .clone()
            .filter(|_| args.online.as_deref().is_some_and(|b| !b.contains("lichess.org"))),
        livechess: args.livechess.clone().unwrap_or_else(|| "127.0.0.1:1982".into()),
    };
    let args = Arc::new(args);
    let connect = {
        let (args, view_tx) = (args.clone(), view_tx.clone());
        move |found: Found, stop: Arc<AtomicBool>, games: mpsc::Receiver<(GameChoice, Arc<AtomicBool>)>| {
            let (args, view_tx) = (args.clone(), view_tx.clone());
            std::thread::spawn(move || {
                play_on(&found, &args, seed, &stop, &view_tx, &Games::Chosen(games))
                    .map_err(|e| std::io::Error::other(format!("{e:#}")))
            })
        }
    };
    let scan: Arc<tui::Scan> = Arc::new(scan_all);
    tui::run(&args.name, &view_rx, &scan, &connect, target, first, &defaults)?;
    Ok(())
}
