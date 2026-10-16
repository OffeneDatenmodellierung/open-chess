//! `open-chess-uci datagen`: self-play training data on every core.
//!
//! ```text
//! open-chess-uci datagen --games 100000 --nodes 5000 --threads 12 --out gen1.bin
//! open-chess-uci datagen --games 1000 --format text --out sample.txt
//! ```
//!
//! Each worker thread plays whole games with its own searcher and seed
//! and sends the records to one writer, so output order depends on
//! thread timing but each game's records are contiguous. A `.json`
//! sidecar next to the output records the provenance: engine id, the
//! network and parameters in use, the settings, seed and counts, so a
//! net can be traced to its data.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use chess_analysis::nnue::Network;
use chess_analysis::{SearchParams, engine_id};
use chess_train::datagen::{Record, Settings, play};
use chess_train::play::Player;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Bullet,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub games: u64,
    pub threads: usize,
    pub out: PathBuf,
    pub format: Format,
    pub seed: u64,
    pub hash_mb: usize,
    pub net: Option<PathBuf>,
    pub settings: Settings,
    /// `name=value` search parameter overrides.
    pub params: Vec<(String, i32)>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            games: 1000,
            threads: std::thread::available_parallelism().map_or(1, usize::from),
            out: PathBuf::from("data.bin"),
            format: Format::Bullet,
            seed: 1,
            hash_mb: 16,
            net: None,
            settings: Settings::default(),
            params: Vec::new(),
        }
    }
}

/// Parse `datagen` arguments (`--key value`).
pub fn parse(words: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut i = 0;
    let mut format_set = false;
    while i < words.len() {
        let key = words[i].as_str();
        let value = words.get(i + 1).ok_or_else(|| format!("{key} needs a value"))?;
        let number = || value.parse::<u64>().map_err(|_| format!("{key}: {value:?} isn't a number"));
        match key {
            "--games" => args.games = number()?,
            "--threads" => args.threads = number()?.max(1) as usize,
            "--out" => args.out = PathBuf::from(value),
            "--format" => {
                args.format = match value.as_str() {
                    "text" => Format::Text,
                    "bullet" | "bin" => Format::Bullet,
                    other => return Err(format!("--format {other:?}: use text or bullet")),
                };
                format_set = true;
            }
            "--seed" => args.seed = number()?,
            "--hash" => args.hash_mb = number()?.max(1) as usize,
            "--net" => args.net = Some(PathBuf::from(value)),
            "--nodes" => args.settings.nodes = number()?.max(1),
            "--random-plies" => args.settings.random_plies = number()? as u32,
            "--skip-plies" => args.settings.skip_plies = number()? as u32,
            "--resign-cp" => args.settings.resign_cp = number()? as i32,
            "--resign-moves" => args.settings.resign_moves = number()? as u32,
            "--max-plies" => args.settings.max_plies = number()? as usize,
            "--max-start-cp" => args.settings.max_start_cp = number()? as i32,
            "--set" => {
                let (k, v) =
                    value.split_once('=').ok_or_else(|| format!("--set wants name=value, not {value:?}"))?;
                let v: i32 = v.parse().map_err(|_| format!("--set {k}: {v:?} isn't a number"))?;
                args.params.push((k.to_string(), v));
            }
            other => return Err(format!("unknown option {other}")),
        }
        i += 2;
    }
    if !format_set && args.out.extension().is_some_and(|e| e == "txt") {
        args.format = Format::Text;
    }
    Ok(args)
}

pub const USAGE: &str = "usage: open-chess-uci datagen [--games N] [--nodes N] [--threads N] [--out FILE] \
[--format text|bullet] [--seed N] [--hash MB] [--net FILE] [--random-plies N] [--skip-plies N] \
[--resign-cp N] [--resign-moves N] [--max-plies N] [--max-start-cp N] [--set name=value]...";

/// Generate the data. Progress goes to `log`; the records to `args.out`.
///
/// # Panics
///
/// If a worker thread panicked (an internal bug).
pub fn run(args: &Args, mut log: impl FnMut(&str)) -> Result<(), String> {
    let mut params = SearchParams::default();
    for (k, v) in &args.params {
        if !params.set(k, *v) {
            return Err(format!("no search parameter named {k}"));
        }
    }
    let net = match &args.net {
        Some(p) => {
            let bytes = std::fs::read(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
            Some(Arc::new(Network::from_bytes(&bytes).map_err(|e| format!("{}: {e}", p.display()))?))
        }
        None => None,
    };
    let file =
        std::fs::File::create(&args.out).map_err(|e| format!("can't create {}: {e}", args.out.display()))?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    let started = Instant::now();
    let counter = AtomicU64::new(0);
    let (tx, rx) = mpsc::sync_channel::<Vec<Record>>(args.threads * 2);
    let positions = std::thread::scope(|s| {
        for t in 0..args.threads {
            let tx = tx.clone();
            let counter = &counter;
            let net = net.clone();
            s.spawn(move || {
                let mut player = Player::new(params, args.hash_mb);
                player.searcher.set_network(net);
                loop {
                    let g = counter.fetch_add(1, Ordering::Relaxed);
                    if g >= args.games {
                        break;
                    }
                    let seed = splitmix(args.seed ^ g.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (t as u64) << 48);
                    let records = play(&mut player, args.settings, seed);
                    if tx.send(records).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut positions = 0u64;
        let mut games = 0u64;
        for records in rx {
            games += 1;
            positions += records.len() as u64;
            for r in &records {
                let written = match args.format {
                    Format::Bullet => writer.write_all(&r.bullet()),
                    Format::Text => writeln!(writer, "{}", r.text()),
                };
                if let Err(e) = written {
                    log(&format!("write failed: {e}"));
                    return positions;
                }
            }
            if games.is_multiple_of(100) || games == args.games {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                log(&format!(
                    "{games}/{} games, {positions} positions, {:.0} positions/s",
                    args.games,
                    positions as f64 / secs
                ));
            }
        }
        positions
    });
    writer.flush().map_err(|e| e.to_string())?;
    let sidecar = sidecar_path(&args.out);
    let provenance = format!(
        "{{\n  \"engine\": \"{}\",\n  \"format\": \"{}\",\n  \"net\": {},\n  \"params\": {},\n  \"games\": {},\n  \"positions\": {positions},\n  \
         \"seed\": {},\n  \"nodes\": {},\n  \"random_plies\": {},\n  \"skip_plies\": {},\n  \"resign_cp\": {},\n  \"resign_moves\": {},\n  \"max_plies\": {},\n  \"max_start_cp\": {},\n  \
         \"seconds\": {}\n}}\n",
        engine_id(),
        match args.format {
            Format::Bullet => "bulletformat",
            Format::Text => "text",
        },
        args.net.as_ref().map_or("null".to_string(), |p| format!("{:?}", p.display().to_string())),
        serde_params(&params),
        args.games,
        args.seed,
        args.settings.nodes,
        args.settings.random_plies,
        args.settings.skip_plies,
        args.settings.resign_cp,
        args.settings.resign_moves,
        args.settings.max_plies,
        args.settings.max_start_cp,
        started.elapsed().as_secs()
    );
    std::fs::write(&sidecar, provenance).map_err(|e| format!("can't write {}: {e}", sidecar.display()))?;
    log(&format!(
        "wrote {positions} positions to {} (provenance in {})",
        args.out.display(),
        sidecar.display()
    ));
    Ok(())
}

fn serde_params(p: &SearchParams) -> String {
    let inner: Vec<String> = p.to_map().iter().map(|(k, v)| format!("\"{k}\": {v}")).collect();
    format!("{{{}}}", inner.join(", "))
}

fn sidecar_path(out: &Path) -> PathBuf {
    let mut p = out.as_os_str().to_owned();
    p.push(".json");
    PathBuf::from(p)
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn arguments_parse() {
        let a = parse(&words("--games 5 --nodes 200 --threads 2 --out x.txt --set rfp_margin=90")).unwrap();
        assert_eq!(a.games, 5);
        assert_eq!(a.settings.nodes, 200);
        assert_eq!(a.format, Format::Text, "from the extension");
        assert_eq!(a.params, vec![("rfp_margin".to_string(), 90)]);
        assert!(parse(&words("--format pgn")).is_err());
        assert!(parse(&words("--games")).is_err());
    }

    #[test]
    fn a_tiny_run_writes_records_and_provenance() {
        let dir = std::env::temp_dir().join(format!("open-chess-datagen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.bin");
        let a = Args {
            games: 4,
            threads: 2,
            out: out.clone(),
            seed: 3,
            settings: Settings { nodes: 200, ..Settings::default() },
            ..Args::default()
        };
        run(&a, |_| {}).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() % 32, 0);
        let side = std::fs::read_to_string(sidecar_path(&out)).unwrap();
        assert!(side.contains("\"format\": \"bulletformat\""));
        assert!(side.contains(&format!("\"positions\": {}", bytes.len() / 32)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
