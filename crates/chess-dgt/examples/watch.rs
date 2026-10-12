//! Follow a game on a DGT board and print it as it's played.
//!
//! ```sh
//! cargo run -p open-chess-dgt --features serial --example watch -- /dev/ttyACM0
//! ```
//!
//! Set up the starting position to begin (either way round). Each move
//! shows here and, with a DGT3000 on the board's cable, on the clock.
//! Ctrl-C to stop.

use std::process::ExitCode;

use open_chess_dgt::serial::{Flow, Port, run};
use open_chess_dgt::{ClockReport, Command, Event, Follower, Observation, Session};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: watch <port>   (e.g. /dev/ttyACM0, /dev/cu.usbmodem…, /dev/rfcomm0)");
        return ExitCode::from(2);
    };
    let mut port = match Port::open(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut session = Session::new(Follower::new());
    let result = run(&mut port, &mut session, |event, session| {
        match event {
            Event::Connected { major, minor } => println!("board connected (version {major}.{minor})"),
            Event::Reading(p) => eprintln!("  board: {}", p.fen()),
            Event::Game(Observation::Move { san, .. }) => {
                let game = session.follower().game();
                let ply = game.ply();
                let number = ply.div_ceil(2);
                if ply % 2 == 1 {
                    println!("{number}. {san}");
                } else {
                    println!("{number}... {san}");
                }
                session.send(Command::ClockText { text: san.clone(), beep: 1 });
            }
            Event::Game(Observation::NewGame) => {
                println!("new game ({:?})", session.follower().orientation());
            }
            Event::Game(o) => println!("{o:?}"),
            Event::Clock(ClockReport::Ack(a)) => {
                if let Some(b) = a.button() {
                    println!("clock button {b:?}");
                }
            }
            Event::Clock(_) | Event::Info(_) => {}
        }
        Flow::Continue
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{path}: {e}");
            ExitCode::FAILURE
        }
    }
}
