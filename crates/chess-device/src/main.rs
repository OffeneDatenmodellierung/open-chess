//! `open-chess-device`: play the engine on an electronic board.
//!
//! ```sh
//! open-chess-device --board /dev/ttyACM0                 # open-chess's engine, level 4
//! open-chess-device --board /dev/ttyACM0 --level 7 --book gm2001.bin
//! open-chess-device --board /dev/rfcomm0 --uci /usr/games/stockfish --move-time 2 \
//!     --uci-option "Skill Level=5" --play black --time 5+3
//! open-chess-device --board /dev/ttyACM0 --listen 0.0.0.0:8080 --app dist/chess
//! sudo open-chess-device --board /dev/rfcomm0 --clock dgtpi   # on a DGT Pi
//! open-chess-device --centaur                                 # on a DGT Centaur
//! open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board chessnut   # Chessnut Air / Go
//! open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board pegasus    # DGT Pegasus
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
    use chess_core::{Game, GameError, Played, Side};
    use chess_dgt::serial::Port;
    use chess_dgt::{ClockButton, Command, Event, Follower, Observation, Session};
    use chess_dgtpi::driver::{Clock, Report};
    use clap::Parser;
    use open_chess_device::board::{Board, centaur_button};
    use open_chess_device::controller::{Controller, Setup, TimeControl, describe};
    use open_chess_device::dgtpi;
    use open_chess_device::opponent::{EngineSpec, Opponent};
    use open_chess_device::view::{EngineInfo, View, game_id};
    use open_chess_device::web::{self, Change, Request, Web};
    use tokio::sync::watch;

    #[derive(Debug, Parser)]
    #[command(name = "open-chess-device", version, about = "Play the engine on an electronic chess board")]
    struct Args {
        /// The board's port: /dev/ttyACM0 (USB, Linux), /dev/cu.usbmodem… (macOS),
        /// /dev/rfcomm0 (Bluetooth, after `rfcomm bind`); on a Centaur, its
        /// controller's port (default /dev/serial0).
        #[arg(long)]
        board: Option<String>,

        /// Running on a DGT Centaur: its board controller, lights and buttons.
        #[arg(long)]
        centaur: bool,

        /// A Bluetooth LE board's address (from `bluetoothctl`, `scan on`).
        #[arg(long)]
        ble: Option<String>,

        /// Which kind of Bluetooth LE board.
        #[arg(long, default_value = "chessnut", value_parser = ["chessnut", "pegasus"])]
        ble_board: String,

        /// The board uses a random (static) Bluetooth address rather than
        /// its public one (`bluetoothctl info` says "random").
        #[arg(long)]
        random_address: bool,

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

        /// Where the clock is: on the board's cable, a DGT Pi's own clock
        /// (I²C, needs root), or a simulated one (for trying it out).
        #[arg(long, default_value = "board", value_parser = ["board", "dgtpi", "sim"])]
        clock: String,

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

    // ── the board, whichever kind ──

    /// What a board hands the controller.
    enum Input {
        Event(Event),
        Button(ClockButton),
    }

    /// A board on a port: the game as it follows it, and the I/O.
    trait Link: Board {
        /// Write what's queued, read what's arrived.
        fn exchange(&mut self, now: Instant) -> Result<Vec<Input>>;
    }

    /// A DGT board (USB, serial, Bluetooth), with a clock on its cable or
    /// a DGT Pi's own.
    struct DgtLink {
        session: Session,
        port: Port,
        clock: Option<Clock>,
    }

    impl Board for DgtLink {
        fn game(&self) -> &Game {
            self.session.game()
        }
        fn awaiting_board(&self) -> bool {
            self.session.awaiting_board()
        }
        fn play(&mut self, uci: &str) -> Result<Played, GameError> {
            Board::play(&mut self.session, uci)
        }
        fn send(&mut self, c: Command) {
            self.session.send(c);
        }
    }

    impl Link for DgtLink {
        fn exchange(&mut self, now: Instant) -> Result<Vec<Input>> {
            for c in self.session.take_commands() {
                match &self.clock {
                    Some(clock) if dgtpi::is_clock(&c) => {
                        if let Some(r) = dgtpi::request(&c) {
                            clock.send(r);
                        }
                    }
                    _ => self.port.send(&c)?,
                }
            }
            let mut buf = [0u8; 512];
            let n = self.port.read(&mut buf)?;
            let mut inputs: Vec<Input> =
                self.session.receive(&buf[..n], now).into_iter().map(Input::Event).collect();
            if let Some(clock) = &self.clock {
                for r in clock.reports() {
                    match r {
                        Report::Event(e) => inputs.extend(dgtpi::button(e).map(Input::Button)),
                        Report::Ready => eprintln!("clock ready"),
                        Report::Failed(why) => eprintln!("clock: {why}"),
                    }
                }
            }
            self.session.poll(now);
            Ok(inputs)
        }
    }

    /// A DGT Centaur's board controller.
    struct CentaurLink {
        session: chess_centaur::Session,
        port: Port,
    }

    impl Board for CentaurLink {
        fn game(&self) -> &Game {
            self.session.game()
        }
        fn awaiting_board(&self) -> bool {
            self.session.awaiting_board()
        }
        fn play(&mut self, uci: &str) -> Result<Played, GameError> {
            Board::play(&mut self.session, uci)
        }
        fn send(&mut self, c: Command) {
            Board::send(&mut self.session, c);
        }
    }

    impl Link for CentaurLink {
        fn exchange(&mut self, now: Instant) -> Result<Vec<Input>> {
            self.session.poll(now);
            for w in self.session.take_writes() {
                self.port.write_all(&w)?;
            }
            let mut buf = [0u8; 512];
            let n = self.port.read(&mut buf)?;
            let mut inputs = Vec::new();
            for e in self.session.receive(&buf[..n], Instant::now()) {
                match e {
                    chess_centaur::Event::Connected(a) => {
                        inputs.push(Input::Event(Event::Connected { major: a.0, minor: a.1 }));
                    }
                    chess_centaur::Event::Game(o) => inputs.push(Input::Event(Event::Game(o))),
                    chess_centaur::Event::Button(b) => inputs.extend(centaur_button(b).map(Input::Button)),
                    chess_centaur::Event::Battery { level, charging } => {
                        if level <= 3 && !charging {
                            eprintln!("battery low ({level}/20)");
                        }
                    }
                }
            }
            Ok(inputs)
        }
    }

    /// A Bluetooth LE board's GATT link.
    #[cfg(target_os = "linux")]
    type Ble = chess_ble::Gatt<chess_ble::l2cap::Channel>;

    /// How long to wait for the board's notifications each time round.
    const BLE_WAIT: Duration = Duration::from_millis(100);

    #[cfg(target_os = "linux")]
    fn ble_connect(args: &Args, address: &str, notify: &str) -> Result<(Ble, u16)> {
        use chess_ble::l2cap::{AddressType, Channel};
        let a: chess_ble::l2cap::Address = address.parse().map_err(anyhow::Error::msg)?;
        let kind = if args.random_address { AddressType::Random } else { AddressType::Public };
        let channel = Channel::connect(a, kind).with_context(|| format!("connecting to {address}"))?;
        let mut gatt = chess_ble::Gatt::new(channel);
        gatt.exchange_mtu(185)?;
        let uuid: chess_ble::Uuid = notify.parse().map_err(anyhow::Error::msg)?;
        let handle = gatt.subscribe(uuid).context("subscribing to the board")?;
        Ok((gatt, handle))
    }

    /// The board's notifications for a moment.
    #[cfg(target_os = "linux")]
    fn ble_notifications(gatt: &mut Ble, handle: u16) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let mut wait = BLE_WAIT;
        while let Some((h, data)) = gatt.notification(wait)? {
            if h == handle {
                out.push(data);
            }
            wait = Duration::ZERO;
        }
        Ok(out)
    }

    /// A Chessnut Air / Go over Bluetooth LE.
    #[cfg(target_os = "linux")]
    struct ChessnutLink {
        gatt: Ble,
        notify: u16,
        write: chess_ble::Uuid,
        session: chess_chessnut::Session,
    }

    #[cfg(target_os = "linux")]
    impl Board for ChessnutLink {
        fn game(&self) -> &Game {
            self.session.game()
        }
        fn awaiting_board(&self) -> bool {
            Board::awaiting_board(&self.session)
        }
        fn play(&mut self, uci: &str) -> Result<Played, GameError> {
            Board::play(&mut self.session, uci)
        }
        fn send(&mut self, c: Command) {
            Board::send(&mut self.session, c);
        }
    }

    #[cfg(target_os = "linux")]
    impl Link for ChessnutLink {
        fn exchange(&mut self, now: Instant) -> Result<Vec<Input>> {
            for w in self.session.take_writes() {
                self.gatt.write_to(self.write, &w)?;
            }
            for n in ble_notifications(&mut self.gatt, self.notify)? {
                for e in self.session.receive(&n, now) {
                    if let chess_chessnut::Event::Battery { percent, charging: false } = e
                        && percent < 10
                    {
                        eprintln!("battery low ({percent}%)");
                    }
                }
            }
            Ok(self
                .session
                .poll(Instant::now())
                .into_iter()
                .filter_map(|e| match e {
                    chess_chessnut::Event::Game(o) => Some(Input::Event(Event::Game(o))),
                    chess_chessnut::Event::Battery { .. } => None,
                })
                .collect())
        }
    }

    /// A DGT Pegasus over Bluetooth LE: the DGT protocol over the Nordic
    /// UART service.
    #[cfg(target_os = "linux")]
    struct PegasusLink {
        gatt: Ble,
        notify: u16,
        write: chess_ble::Uuid,
        session: Session,
    }

    #[cfg(target_os = "linux")]
    impl Board for PegasusLink {
        fn game(&self) -> &Game {
            self.session.game()
        }
        fn awaiting_board(&self) -> bool {
            self.session.awaiting_board()
        }
        fn play(&mut self, uci: &str) -> Result<Played, GameError> {
            Board::play(&mut self.session, uci)
        }
        fn send(&mut self, c: Command) {
            if let Some(c) = chess_dgt::pegasus::translate(c) {
                self.session.send(c);
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Link for PegasusLink {
        fn exchange(&mut self, now: Instant) -> Result<Vec<Input>> {
            for c in self.session.take_commands() {
                if let Some(c) = chess_dgt::pegasus::translate(c) {
                    self.gatt.write_to(self.write, &c.encode())?;
                }
            }
            let mut inputs = Vec::new();
            for n in ble_notifications(&mut self.gatt, self.notify)? {
                inputs.extend(self.session.receive(&n, now).into_iter().map(Input::Event));
            }
            self.session.poll(Instant::now());
            Ok(inputs)
        }
    }

    #[cfg(target_os = "linux")]
    fn play_ble(
        args: &Args,
        address: &str,
        controller: Controller,
        opponent: &Opponent,
        engine: &EngineInfo,
        seed: u64,
    ) -> Result<()> {
        let now = Instant::now();
        eprintln!("connecting to {address} ({})", args.ble_board);
        if args.ble_board == "pegasus" {
            let (gatt, notify) = ble_connect(args, address, chess_dgt::pegasus::NOTIFY)?;
            let write = chess_dgt::pegasus::WRITE.parse().map_err(anyhow::Error::msg)?;
            let mut session = chess_dgt::pegasus::session(Follower::new());
            session.start(now);
            play(&mut PegasusLink { gatt, notify, write, session }, controller, opponent, engine, seed, args)
        } else {
            let (gatt, notify) = ble_connect(args, address, chess_chessnut::protocol::READ)?;
            let write = chess_chessnut::protocol::WRITE.parse().map_err(anyhow::Error::msg)?;
            let mut session = chess_chessnut::Session::new(Follower::new());
            session.start(now);
            play(&mut ChessnutLink { gatt, notify, write, session }, controller, opponent, engine, seed, args)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn play_ble(_: &Args, _: &str, _: Controller, _: &Opponent, _: &EngineInfo, _: u64) -> Result<()> {
        bail!("--ble: Bluetooth LE boards need Linux (a Raspberry Pi, or a Linux computer)")
    }

    /// The DGT Pi's own clock, or a simulated one; `None` for a clock on
    /// the board's cable.
    fn open_clock(kind: &str) -> Result<Option<Clock>> {
        let clock = match kind {
            #[cfg(target_os = "linux")]
            "dgtpi" => {
                let bus = chess_dgtpi::bcm::BcmBus::open().context("the DGT Pi's clock")?;
                Clock::spawn(bus)?
            }
            #[cfg(not(target_os = "linux"))]
            "dgtpi" => bail!("--clock dgtpi: the DGT Pi's clock needs Linux on the DGT Pi"),
            "sim" => Clock::spawn(chess_dgtpi::sim::SimClock::off())?,
            _ => return Ok(None),
        };
        Ok(Some(clock))
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
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        let controller = Controller::new(Setup {
            player,
            level: args.level,
            book,
            time: args.time,
            white_on_left: args.white_clock == "left",
            seed,
        });
        let now = Instant::now();
        if let Some(address) = &args.ble {
            return play_ble(args, address, controller, &opponent, &engine, seed);
        }
        if args.centaur {
            let path = args.board.as_deref().unwrap_or("/dev/serial0");
            let port = Port::open_at(path, 1_000_000)
                .with_context(|| format!("opening the Centaur's controller on {path}"))?;
            let mut session = chess_centaur::Session::new(chess_centaur::Follower::new());
            session.start(now);
            eprintln!("waiting for the Centaur's controller on {path} (you play {})", args.play);
            play(&mut CentaurLink { session, port }, controller, &opponent, &engine, seed, args)
        } else {
            let Some(path) = args.board.as_deref() else {
                bail!("--board: the board's port (or --centaur, or --ble)")
            };
            let port = Port::open(path).with_context(|| format!("opening the board on {path}"))?;
            let clock = open_clock(&args.clock)?;
            let mut session = Session::new(Follower::new());
            session.start(now);
            eprintln!("waiting for the board on {path} (you play {})", args.play);
            play(&mut DgtLink { session, port, clock }, controller, &opponent, &engine, seed, args)
        }
    }

    /// Play on a board until it fails.
    fn play<L: Link>(
        link: &mut L,
        mut controller: Controller,
        opponent: &Opponent,
        engine: &EngineInfo,
        seed: u64,
        args: &Args,
    ) -> Result<()> {
        // What the app sees, and the settings it changes.
        let mut game = (controller.games(), game_id(seed, 0), now_ms());
        let (view_tx, view_rx) =
            watch::channel(View::new(&controller, link, engine, game.1.clone(), game.2, Instant::now()));
        let (request_tx, requests) = mpsc::channel::<Request>();
        if let Some(addr) = &args.listen {
            let at = web::spawn(addr, Web::new(view_rx, request_tx, args.app.clone(), &args.name))
                .with_context(|| format!("listening on {addr}"))?;
            eprintln!("the app: http://{at}/");
        }

        loop {
            let now = Instant::now();
            for input in link.exchange(now)? {
                let think = match input {
                    Input::Event(e) => {
                        log(&e, link);
                        controller.on_event(&e, link, now)
                    }
                    Input::Button(b) => controller.on_button(b, link, now),
                };
                if let Some(t) = think {
                    opponent.ask(t);
                }
            }
            while let Some(r) = opponent.poll() {
                if let Some(e) = &r.error {
                    eprintln!("engine: {e}");
                }
                if let Some(t) = controller.on_reply(r.ply, r.uci.as_deref(), link, Instant::now()) {
                    opponent.ask(t);
                }
                if link.awaiting_board() {
                    println!(
                        "engine plays {}  (make it on the board)",
                        link.game().sans().last().map_or("", String::as_str)
                    );
                }
            }
            let mut replies = Vec::new();
            while let Ok(r) = requests.try_recv() {
                let result = apply(r.change, &mut controller, link, opponent);
                replies.push((r.reply, result));
            }
            controller.tick(link, Instant::now());

            if controller.games() != game.0 {
                game = (controller.games(), game_id(seed, controller.games()), now_ms());
            }
            let v = View::new(&controller, link, engine, game.1.clone(), game.2, Instant::now());
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
        }
    }

    /// A settings change from the app.
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

    fn now_ms() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
    }

    fn log(e: &Event, board: &impl Board) {
        match e {
            Event::Connected { .. } => eprintln!("board connected"),
            Event::Game(Observation::NewGame) => println!("new game"),
            Event::Game(Observation::Move { .. } | Observation::Takeback { .. }) => {
                println!("{}", describe(board.game()));
            }
            _ => {}
        }
    }
}
