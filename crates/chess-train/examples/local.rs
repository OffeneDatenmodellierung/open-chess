//! A quick local test of parameter changes, without the server or the
//! host application (esports-lab's `chess train local` does the same):
//!
//! ```text
//! cargo run --release -p open-chess-train --example local -- \
//!     se_depth=6 200 nodes=20000 2
//! ```
//! Arguments: the change (`name=value,…`), pairs, time control, threads.

use std::sync::atomic::AtomicBool;

use chess_analysis::SearchParams;
use open_chess_train::run::{Job, Kind};
use open_chess_train::sprt::Penta;
use open_chess_train::worker::{Limits, run};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let set = args.first().cloned().unwrap_or_default();
    let pairs: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(100);
    let tc = args.get(2).map_or("nodes=20000", String::as_str).parse().expect("tc");
    let threads: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(2);
    let base = SearchParams::default();
    let mut test = base;
    for kv in set.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').expect("name=value");
        assert!(test.set(k, v.parse().expect("number")), "no parameter {k}");
    }
    let stop = AtomicBool::new(false);
    let mut total = Penta::default();
    let started = std::time::Instant::now();
    let chunk = threads as u64 * 4;
    let mut done = 0;
    let mut seed = 1;
    while done < pairs {
        let n = chunk.min(pairs - done);
        let job = Job {
            run_id: 0,
            kind: Kind::Sprt,
            engine: String::new(),
            seed,
            count: n,
            tc,
            a: test,
            b: base,
            step: None,
            random_plies: 0,
        };
        let (r, _) = run(&job, "local", Limits { threads, memory_mb: 32 * threads }, &stop);
        total.add(&r.penta);
        done += n;
        seed += 1;
    }
    let (e, lo, hi) = total.elo().unwrap_or_default();
    println!(
        "{set}: {done} pairs {:?}  Elo {e:+.1} [{lo:+.1}, {hi:+.1}]  LLR(0,5) {:.2}  {:.0}s",
        total.0,
        total.llr(0.0, 5.0),
        started.elapsed().as_secs_f64()
    );
}
