//! `open-chess`: the chess server and the board runtime in one program,
//! each a Cargo feature (`server`, `device`; both by default).
//!
//! ```sh
//! open-chess serve                  # the Lichess-compatible server
//! open-chess analyse                # the fair-play worker
//! open-chess device --board /dev/ttyACM0
//! open-chess                        # serves with DATABASE_URL set, else the board screen
//! open-chess-device …               # a link to this program: the board runtime
//! ```
//!
//! A build without a part says which feature it needs.

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

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
} else {
    "With no command: the board screen. This build has no server (the `server` feature)."
};

#[derive(Subcommand)]
enum Cmd {
    #[cfg(feature = "server")]
    #[command(flatten)]
    Server(Box<chess_server::cli::Cmd>),
    /// Play chess on an electronic board.
    #[cfg(feature = "device")]
    Device(Box<chess_device::cli::Args>),
    /// A command this build doesn't have.
    #[cfg(not(all(feature = "server", feature = "device")))]
    #[command(external_subcommand)]
    Other(Vec<String>),
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
        #[cfg(not(all(feature = "server", feature = "device")))]
        Some(Cmd::Other(args)) => {
            let name = args.first().map_or("", String::as_str);
            if SERVER_COMMANDS.contains(&name) {
                missing("server", &format!("`{name}`"))
            } else if name == "device" {
                missing("device", "the board runtime")
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
    unreachable!("a build has the server or the device")
}

#[cfg(feature = "server")]
fn server(cmd: Option<chess_server::cli::Cmd>) -> ExitCode {
    match chess_server::cli::run(cmd) {
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

#[cfg(not(any(feature = "server", feature = "device")))]
compile_error!("open-chess needs the `server` feature, the `device` feature, or both");

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
    }
}
