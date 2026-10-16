//! `open-chess-uci`: with no arguments, the UCI protocol on stdin/stdout.
//! `bench [depth]` and `datagen …` run and exit.

use std::io;
use std::process::ExitCode;

use open_chess_uci::{bench, datagen, uci};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => match uci::run(io::stdin().lock(), io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("open-chess-uci: {e}");
                ExitCode::FAILURE
            }
        },
        Some("bench") => {
            let depth = match args.get(1).map(|d| d.parse::<u8>().map_err(|_| d)) {
                None => bench::DEFAULT_DEPTH,
                Some(Ok(d)) => d,
                Some(Err(d)) => {
                    eprintln!("open-chess-uci bench: {d:?} isn't a depth");
                    return ExitCode::from(2);
                }
            };
            let b = bench::run(depth, None, 16, |s| eprintln!("{s}"));
            println!("{} nodes {} nps", b.nodes, b.nps());
            ExitCode::SUCCESS
        }
        Some("datagen") => {
            let parsed = datagen::parse(&args[1..]);
            let a = match parsed {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("open-chess-uci datagen: {e}\n{}", datagen::USAGE);
                    return ExitCode::from(2);
                }
            };
            match datagen::run(&a, |s| eprintln!("{s}")) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("open-chess-uci datagen: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("tune") => {
            println!("{}", uci::Engine::tune_lines());
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("open-chess-uci {}", chess_analysis::engine_id());
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") => {
            println!(
                "open-chess-uci            the UCI protocol on stdin/stdout\n\
                 open-chess-uci bench [D]  search the benchmark positions to depth D (default {})\n\
                 open-chess-uci datagen …  self-play training data ({})\n\
                 open-chess-uci tune       the search tunables as OpenBench SPSA input",
                bench::DEFAULT_DEPTH,
                datagen::USAGE
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("open-chess-uci: unknown command {other:?} (--help lists them)");
            ExitCode::from(2)
        }
    }
}
