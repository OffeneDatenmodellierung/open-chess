//! `open-chess-device`: play the engine on an electronic board.
//!
//! ```sh
//! open-chess-device --board /dev/ttyACM0                 # open-chess's engine, level 4
//! open-chess-device --board /dev/ttyACM0 --level 7 --book gm2001.bin
//! open-chess-device --board /dev/rfcomm0 --uci /usr/games/stockfish --move-time 2 \
//!     --uci-option "Skill Level=5" --play black --time 5+3
//! ```
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
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use chess_analysis::book::Book;
    use chess_core::Side;
    use chess_dgt::serial::Port;
    use chess_dgt::{Event, Follower, Observation, Session};
    use clap::Parser;
    use open_chess_device::controller::{Controller, Setup, TimeControl, describe};
    use open_chess_device::opponent::{EngineSpec, Opponent};

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

    fn run(args: &Args) -> Result<()> {
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
        let book = match &args.book {
            Some(p) => {
                let bytes = std::fs::read(p).with_context(|| format!("reading the book {p}"))?;
                let book = Book::from_bytes(&bytes).with_context(|| p.clone())?;
                eprintln!("book: {p}, {} entries", book.len());
                Some(book)
            }
            None => None,
        };
        let player = if args.play == "black" { Side::Black } else { Side::White };
        let opponent = Opponent::start(spec)?;
        let mut port =
            Port::open(&args.board).with_context(|| format!("opening the board on {}", args.board))?;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
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
            controller.tick(&mut session, Instant::now());
            session.poll(now);
        }
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
