//! `open-chess-device`: play the engine on an electronic board.
//!
//! ```sh
//! open-chess-device --board /dev/ttyACM0                 # open-chess's engine, level 4
//! open-chess-device --board /dev/ttyACM0 --level 7 --book gm2001.bin
//! open-chess-device --board /dev/rfcomm0 --uci /usr/games/stockfish --move-time 2 \
//!     --uci-option "Skill Level=5" --play black --time 5+3
//! open-chess-device --board /dev/ttyACM0 --listen 0.0.0.0:8080 --app dist/chess
//! ```
//!
//! With `--listen`, a phone on the same network opens the device's address
//! and gets the Chess lab app showing the game, with the device's settings.
//!
//! Set up the starting position (either way round) to begin; set it up
//! again for a new game. Take moves back by putting the pieces back.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("open-chess-device needs a Unix system (Linux, macOS) for the board's serial port");
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
mod unix {
    use std::process::ExitCode;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use anyhow::{Context, Result, bail};
    use chess_analysis::book::Book;
    use chess_core::Side;
    use chess_dgt::serial::Port;
    use chess_dgt::{Event, Follower, Observation, Session};
    use clap::Parser;
    use open_chess_device::controller::{Controller, Setup, TimeControl, describe};
    use open_chess_device::opponent::{EngineSpec, Opponent};
    use open_chess_device::view::{EngineInfo, View, game_id};
    use open_chess_device::web::{self, Change, Request, Web};
    use tokio::sync::watch;

    #[derive(Debug, Parser)]
    #[command(name = "open-chess-device", version, about = "Play the engine on an electronic chess board")]
    struct Args {
        /// The board's port: /dev/ttyACM0 (USB, Linux), /dev/cu.usbmodem… (macOS),
        /// /dev/rfcomm0 (Bluetooth, after `rfcomm bind`).
        #[arg(long)]
        board: String,

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

        /// Serve the app and the device's API here, e.g. 0.0.0.0:8080 (every
        /// network the device is on: a home network, not the internet).
        #[arg(long)]
        listen: Option<String>,

        /// The Chess lab app's files (a build of web/chess), served at /app/.
        #[arg(long)]
        app: Option<std::path::PathBuf>,

        /// The device's name, shown in the app.
        #[arg(long, default_value = "open-chess board")]
        name: String,
    }

    pub fn main() -> ExitCode {
        match run(&Args::parse()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("open-chess-device: {e:#}");
                ExitCode::FAILURE
            }
        }
    }

    fn engine_spec(args: &Args) -> Result<EngineSpec> {
        let spec = match &args.uci {
            Some(path) => {
                let mut options = Vec::new();
                for o in &args.uci_options {
                    let Some((k, v)) = o.split_once('=') else {
                        bail!("--uci-option {o}: expected NAME=VALUE")
                    };
                    options.push((k.trim().to_owned(), v.trim().to_owned()));
                }
                if !(args.move_time > 0.0 && args.move_time.is_finite()) {
                    bail!("--move-time must be a positive number of seconds");
                }
                EngineSpec::Uci {
                    path: path.clone(),
                    move_time: Duration::from_secs_f64(args.move_time),
                    options,
                }
            }
            None => EngineSpec::Builtin,
        };
        Ok(spec)
    }

    fn load_book(args: &Args) -> Result<Option<Book>> {
        let book = match &args.book {
            Some(p) => {
                let bytes = std::fs::read(p).with_context(|| format!("reading the book {p}"))?;
                let book = Book::from_bytes(&bytes).with_context(|| p.clone())?;
                eprintln!("book: {p}, {} entries", book.len());
                Some(book)
            }
            None => None,
        };
        Ok(book)
    }

    fn run(args: &Args) -> Result<()> {
        let spec = engine_spec(args)?;
        let book = load_book(args)?;
        let player = if args.play == "black" { Side::Black } else { Side::White };
        let engine = EngineInfo {
            name: args.uci.as_ref().map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned())
            }),
        };
        let opponent = Opponent::start(spec)?;
        let mut port =
            Port::open(&args.board).with_context(|| format!("opening the board on {}", args.board))?;
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        let mut controller = Controller::new(Setup {
            player,
            level: args.level,
            book,
            time: args.time,
            white_on_left: args.white_clock == "left",
            seed,
        });
        let mut session = Session::new(Follower::new());
        session.start(Instant::now());

        // What the app sees, and the settings it changes.
        let mut game = (controller.games(), game_id(seed, 0), now_ms());
        let (view_tx, view_rx) = watch::channel(View::new(
            &controller,
            session.follower(),
            &engine,
            game.1.clone(),
            game.2,
            Instant::now(),
        ));
        let (request_tx, requests) = mpsc::channel::<Request>();
        if let Some(addr) = &args.listen {
            let at = web::spawn(addr, Web::new(view_rx, request_tx, args.app.clone(), &args.name))
                .with_context(|| format!("listening on {addr}"))?;
            eprintln!("the app: http://{at}/");
        }
        eprintln!("waiting for the board on {} (you play {})", args.board, args.play);

        let mut buf = [0u8; 512];
        loop {
            for c in session.take_commands() {
                port.send(&c)?;
            }
            let n = port.read(&mut buf)?;
            let now = Instant::now();
            if n > 0 {
                for e in session.receive(&buf[..n], now) {
                    log(&e, &session);
                    if let Some(t) = controller.on_event(&e, &mut session, now) {
                        opponent.ask(t);
                    }
                }
            }
            while let Some(r) = opponent.poll() {
                if let Some(e) = &r.error {
                    eprintln!("engine: {e}");
                }
                if let Some(t) = controller.on_reply(r.ply, r.uci.as_deref(), &mut session, Instant::now()) {
                    opponent.ask(t);
                }
                if session.follower().awaiting_board() {
                    let g = session.follower().game();
                    println!(
                        "engine plays {}  (make it on the board)",
                        g.sans().last().map_or("", String::as_str)
                    );
                }
            }
            let mut replies = Vec::new();
            while let Ok(r) = requests.try_recv() {
                let result = apply(r.change, &mut controller, &mut session, &opponent);
                replies.push((r.reply, result));
            }
            controller.tick(&mut session, Instant::now());

            if controller.games() != game.0 {
                game = (controller.games(), game_id(seed, controller.games()), now_ms());
            }
            let v =
                View::new(&controller, session.follower(), &engine, game.1.clone(), game.2, Instant::now());
            view_tx.send_if_modified(|old| {
                if old.same(&v) {
                    false
                } else {
                    *old = v;
                    true
                }
            });
            // Answered once the change is in what the app reads.
            for (tx, result) in replies {
                let _ = tx.send(result);
            }
            session.poll(now);
        }
    }

    /// A settings change from the app.
    fn apply(
        change: Change,
        controller: &mut Controller,
        session: &mut Session,
        opponent: &Opponent,
    ) -> Result<String, String> {
        let now = Instant::now();
        match change {
            Change::Level(level) => {
                controller.set_level(level, session);
                Ok(format!("The engine plays at level {}.", controller.level()))
            }
            Change::Player(side) => {
                if let Some(t) = controller.set_player(side, session, now)? {
                    opponent.ask(t);
                }
                Ok(format!("You play {}.", side.name()))
            }
            Change::Time(time) => {
                let label = time.map_or_else(|| "untimed".to_owned(), |t| t.to_string());
                Ok(if controller.set_time(time, session, now) {
                    format!("This game is {label}.")
                } else {
                    format!("The next game is {label}.")
                })
            }
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
    }

    fn log(e: &Event, s: &Session) {
        match e {
            Event::Connected { major, minor } => eprintln!("board connected (version {major}.{minor})"),
            Event::Game(Observation::NewGame) => println!("new game ({:?})", s.follower().orientation()),
            Event::Game(Observation::Move { .. } | Observation::Takeback { .. }) => {
                println!("{}", describe(s.follower().game()));
            }
            _ => {}
        }
    }
}
