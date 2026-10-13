//! The device's folder and what the terminal screen remembers between
//! runs: the names last played under, the computer's level and the time
//! control, the open-chess server's address and the API tokens (so they're
//! typed once). The folder is `$OPEN_CHESS_HOME`, or `open-chess` in the
//! user's config folder (`$XDG_CONFIG_HOME` or `~/.config` on Linux and
//! macOS, `%APPDATA%` on Windows). The settings file is readable by its
//! owner only, as it holds the tokens.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The device's folder, if there's a home to put it in.
pub fn folder() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("OPEN_CHESS_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")));
    base.map(|b| b.join("open-chess"))
}

/// What the screen remembers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The name the person at the board plays the computer under.
    pub name: String,
    /// Two players on one board.
    pub white: String,
    pub black: String,
    /// open-chess's engine level, 1–8.
    pub level: Option<u8>,
    /// The time control, `5+3`, or empty for none.
    pub time: String,
    pub lichess_token: String,
    pub server_url: String,
    pub server_token: String,
    /// A second account's token, for both sides on the board.
    pub lichess_other_token: String,
    pub server_other_token: String,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        folder().map(|d| d.join("device.json"))
    }

    pub fn load() -> Settings {
        Settings::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Save (best effort).
    pub fn save(&self) {
        let Some(path) = Settings::path() else { return };
        if let Err(e) = write_private(&path, &serde_json::to_vec_pretty(self).unwrap_or_default()) {
            crate::log::line(crate::log::Source::Device, format!("saving {}: {e}", path.display()));
        }
    }
}

/// Write a file only its owner can read.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)
}

/// The user's login name, for a first default.
pub fn login_name() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "you".into())
}
