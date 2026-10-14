//! `open-chess`: the chess server, the board runtime, the photo reader and
//! its training in one program, each a Cargo feature (`server`, `device`,
//! `vision`, `train`; all by default).
//!
//! ```sh
//! open-chess serve                  # the Lichess-compatible server
//! open-chess analyse                # the fair-play worker
//! open-chess device --board /dev/ttyACM0
//! open-chess photo read board.jpg   # the position in a photo
//! open-chess photo start board.jpg --computer 4    # carry it over to an online game
//! open-chess photo move board.jpg   # play the move a new photo shows
//! open-chess train board-reader …   # train the board reader
//! open-chess                        # serves with DATABASE_URL set, else the board screen
//! open-chess-device …               # a link to this program: the board runtime
//! ```
//!
//! A build without a part says which feature it needs.

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

#[cfg(feature = "vision")]
mod photo;

#[derive(Parser)]
#[command(
    name = "open-chess",
    version,
    about = "open-chess: a Lichess-compatible chess server and an electronic-board runtime",
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

const AFTER_HELP: &str = if cfg!(all(feature = "server", feature = "device")) {
    "With no command: serve when DATABASE_URL is set, otherwise the board screen."
} else if cfg!(feature = "server") {
    "With no command: serve. This build has no board runtime (the `device` feature)."
} else if cfg!(feature = "device") {
    "With no command: the board screen. This build has no server (the `server` feature)."
} else {
    "This build has neither the server nor the board runtime."
};

#[derive(Subcommand)]
enum Cmd {
    #[cfg(feature = "server")]
    #[command(flatten)]
    Server(Box<chess_server::cli::Cmd>),
    /// Play chess on an electronic board.
    #[cfg(feature = "device")]
    Device(Box<chess_device::cli::Args>),
    /// Read a board from a photo; start an online game from it, or play
    /// the move a new photo shows.
    #[cfg(feature = "vision")]
    Photo {
        #[command(subcommand)]
        cmd: Box<photo::Cmd>,
    },
    /// Train open-chess's networks.
    #[cfg(feature = "train")]
    Train {
        #[command(subcommand)]
        cmd: TrainCmd,
    },
    /// A command this build doesn't have.
    #[cfg(not(all(feature = "server", feature = "device", feature = "vision", feature = "train")))]
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[cfg(feature = "train")]
#[derive(Subcommand)]
enum TrainCmd {
    /// The board reader (photo → position), on licence-checked datasets.
    BoardReader(Box<chess_vision_train::Args>),
}

/// The commands each part brings, to say which feature a missing one needs.
#[allow(dead_code)] // a build without the server, and the tests
const SERVER_COMMANDS: &[&str] = &["serve", "analyse", "migrate", "photos", "bot", "healthcheck"];

fn main() -> ExitCode {
    // Run as `open-chess-device` (a link to this program): the board runtime.
    let invoked = std::env::args_os().next().unwrap_or_default();
    let stem = std::path::Path::new(&invoked).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
    if stem == "open-chess-device" {
        #[cfg(feature = "device")]
        return chess_device::cli::main(
            &<chess_device::cli::Args as CommandFactory>::command().get_matches(),
        );
        #[cfg(not(feature = "device"))]
        return missing("device", "the board runtime");
    }

    let matches = Cli::command().get_matches();
    #[cfg(feature = "device")]
    if let Some(("device", sub)) = matches.subcommand() {
        return chess_device::cli::main(sub);
    }
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    match cli.cmd {
        #[cfg(feature = "server")]
        Some(Cmd::Server(cmd)) => server(Some(*cmd)),
        #[cfg(feature = "device")]
        Some(Cmd::Device(_)) => unreachable!("run above"),
        #[cfg(feature = "vision")]
        Some(Cmd::Photo { cmd }) => done(photo::run(*cmd)),
        #[cfg(feature = "train")]
        Some(Cmd::Train { cmd: TrainCmd::BoardReader(args) }) => done(chess_vision_train::run(&args)),
        #[cfg(not(all(feature = "server", feature = "device", feature = "vision", feature = "train")))]
        Some(Cmd::Other(args)) => {
            let name = args.first().map_or("", String::as_str);
            if SERVER_COMMANDS.contains(&name) {
                missing("server", &format!("`{name}`"))
            } else if name == "device" {
                missing("device", "the board runtime")
            } else if name == "photo" {
                missing("vision", "the photo reader")
            } else if name == "train" {
                missing("train", "training")
            } else {
                eprintln!("open-chess: no command `{name}` (open-chess --help lists them)");
                ExitCode::from(2)
            }
        }
        None => default(),
    }
}

/// `open-chess` with no command.
fn default() -> ExitCode {
    #[cfg(feature = "server")]
    if !cfg!(feature = "device") || chess_server::cli::configured() {
        return server(None);
    }
    #[cfg(feature = "device")]
    {
        let args =
            <chess_device::cli::Args as CommandFactory>::command().get_matches_from(["open-chess-device"]);
        chess_device::cli::main(&args)
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = Cli::command().print_help();
        ExitCode::from(2)
    }
}

#[cfg(feature = "server")]
fn server(cmd: Option<chess_server::cli::Cmd>) -> ExitCode {
    done(chess_server::cli::run(cmd))
}

#[allow(dead_code)] // a build with neither the server, the photo reader nor training
fn done(r: anyhow::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("open-chess: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[allow(dead_code)] // only a build without a part uses it
fn missing(feature: &str, what: &str) -> ExitCode {
    eprintln!(
        "open-chess: this build doesn't have {what}: get the full download, or build it with \
         `cargo install open-chess --features {feature}`"
    );
    ExitCode::from(2)
}

#[cfg(not(any(feature = "server", feature = "device", feature = "vision", feature = "train")))]
compile_error!("open-chess needs at least one of its features: server, device, vision, train");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_parse() {
        Cli::command().debug_assert();
        #[cfg(feature = "server")]
        for c in SERVER_COMMANDS {
            assert!(Cli::command().find_subcommand(c).is_some(), "{c}");
        }
        #[cfg(feature = "device")]
        {
            let m = Cli::command()
                .try_get_matches_from(["open-chess", "device", "--board", "/dev/x", "--level", "7"])
                .unwrap();
            let (name, sub) = m.subcommand().unwrap();
            assert_eq!(name, "device");
            assert_eq!(sub.get_one::<String>("board").map(String::as_str), Some("/dev/x"));
        }
        #[cfg(feature = "vision")]
        for args in [
            &["open-chess", "photo", "read", "board.jpg", "--weights", "r.safetensors"][..],
            &[
                "open-chess",
                "photo",
                "start",
                "--placement",
                "4k3/8/8/8/8/8/8/4K3",
                "--computer",
                "3",
                "--time",
                "10+5",
            ],
            &["open-chess", "photo", "move", "board.jpg", "--game", "abc", "--dry-run"],
        ] {
            Cli::command().try_get_matches_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
        #[cfg(feature = "vision")]
        assert!(
            Cli::command().try_get_matches_from(["open-chess", "photo", "start", "board.jpg"]).is_err(),
            "an opponent or the computer is needed"
        );
        #[cfg(feature = "train")]
        {
            let m = Cli::command()
                .try_get_matches_from([
                    "open-chess",
                    "train",
                    "board-reader",
                    "--datasets",
                    "chessred",
                    "--data-root",
                    "/d",
                    "--out",
                    "runs/x",
                ])
                .unwrap();
            assert_eq!(m.subcommand_name(), Some("train"));
        }
    }
}
