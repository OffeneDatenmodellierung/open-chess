//! Reading the settings people already have in PicoChess's `picochess.ini`,
//! so a PicoChess setup carries over: the board's port and the DGT Pi's
//! clock, the engine, its strength and book, the time control, the web
//! server's port and which side of the clock is White's.

use std::path::{Path, PathBuf};

use crate::controller::TimeControl;

/// What `picochess.ini` says that the device understands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub dgt_port: Option<String>,
    pub dgtpi: bool,
    pub engine: Option<String>,
    /// UCI options for the engine's level (`Elo@2200`: limit strength to
    /// 2200; `Level@5` or `Skill@5`: Skill Level 5).
    pub uci_options: Vec<String>,
    pub book: Option<String>,
    pub time: Option<TimeControl>,
    /// `web-server = 80`: listen on every network at that port.
    pub listen: Option<String>,
    /// White's clock on the `left` or `right`.
    pub clockside: Option<String>,
    /// Keys the device doesn't use (for the log).
    pub ignored: Vec<String>,
}

fn truthy(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "on")
}

/// A path in the ini, relative to the ini's folder (PicoChess's home).
fn resolve(dir: &Path, p: &str) -> String {
    if p.starts_with('/') || Path::new(p).is_absolute() {
        return p.to_owned();
    }
    let d = dir.to_string_lossy();
    let sep = if d.contains('\\') && !d.contains('/') { '\\' } else { '/' };
    format!("{}{sep}{p}", d.trim_end_matches(['/', '\\']))
}

/// Read the text of a `picochess.ini` found in `dir`.
pub fn parse(text: &str, dir: &Path) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "dgt-port" => s.dgt_port = Some(value.to_owned()),
            "dgtpi" => s.dgtpi = truthy(value),
            "engine" => s.engine = Some(resolve(dir, value)),
            "engine-level" => s.uci_options = level_options(value),
            "book" => s.book = Some(resolve(dir, value)),
            "time" => s.time = time(value),
            "web-server" => {
                if let Ok(port) = value.parse::<u16>() {
                    s.listen = Some(format!("0.0.0.0:{port}"));
                }
            }
            "clockside" if matches!(value, "left" | "right") => s.clockside = Some(value.to_owned()),
            other => s.ignored.push(other.to_owned()),
        }
    }
    s
}

/// A PicoChess engine level as UCI options.
fn level_options(level: &str) -> Vec<String> {
    let (kind, n) = level.split_once('@').unwrap_or(("", level));
    let Ok(n) = n.trim().parse::<u32>() else { return Vec::new() };
    match kind.to_ascii_lowercase().as_str() {
        "elo" => vec!["UCI_LimitStrength=true".into(), format!("UCI_Elo={n}")],
        "level" | "skill" => vec![format!("Skill Level={n}")],
        _ => Vec::new(),
    }
}

/// `time = 5 3` (Fischer: minutes and increment) or `time = 10`.
fn time(v: &str) -> Option<TimeControl> {
    let mut parts = v.split_whitespace().map(str::parse::<u32>);
    let minutes = parts.next()?.ok()?;
    let increment_secs = parts.next().and_then(Result::ok).unwrap_or(0);
    (minutes > 0).then_some(TimeControl { minutes, increment_secs })
}

/// Read a `picochess.ini`.
pub fn read(path: &Path) -> std::io::Result<Settings> {
    let text = std::fs::read_to_string(path)?;
    let dir: PathBuf = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok(parse(&text, &dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_dgt_pi_setup() {
        let ini = "\
# PicoChess on a DGT Pi
beep-config = none
dgtpi = True
engine = /opt/picochess/engines/aarch64/a-stockf
engine-level = Elo@2200
book = books/h-varied.bin
web-server = 80
time = 5 3
clockside = right
board-type = dgt
#dgt-port = /dev/ttyACM0
";
        let s = parse(ini, Path::new("/opt/picochess"));
        assert!(s.dgtpi);
        assert_eq!(s.engine.as_deref(), Some("/opt/picochess/engines/aarch64/a-stockf"));
        assert_eq!(s.uci_options, ["UCI_LimitStrength=true", "UCI_Elo=2200"]);
        assert_eq!(s.book.as_deref(), Some("/opt/picochess/books/h-varied.bin"));
        assert_eq!(s.listen.as_deref(), Some("0.0.0.0:80"));
        assert_eq!(s.time, Some(TimeControl { minutes: 5, increment_secs: 3 }));
        assert_eq!(s.clockside.as_deref(), Some("right"));
        assert_eq!(s.dgt_port, None, "commented out");
        assert!(s.ignored.contains(&"beep-config".to_string()));
    }

    #[test]
    fn levels_and_times() {
        assert_eq!(level_options("Level@05"), ["Skill Level=5"]);
        assert!(level_options("Personality@Karpov").is_empty());
        assert_eq!(time("10"), Some(TimeControl { minutes: 10, increment_secs: 0 }));
        assert_eq!(time("0"), None);
    }
}
