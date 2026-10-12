//! PGN export in Lichess's layout: the same tags in the same order, and
//! `[%clk]` comments when clock times are known.

use std::fmt::Write as _;

use crate::game::Side;
use crate::status::Status;

#[derive(Debug, Clone, Default)]
pub struct PgnPlayer {
    pub name: String,
    pub rating: Option<i32>,
    pub rating_diff: Option<i32>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PgnGame<'a> {
    pub event: &'a str,
    /// The game's URL.
    pub site: String,
    pub game_id: &'a str,
    /// When the game was created, Unix milliseconds (UTC).
    pub created_at_ms: i64,
    pub white: PgnPlayer,
    pub black: PgnPlayer,
    pub status: Status,
    pub winner: Option<Side>,
    /// (limit, increment) in seconds; `None` for unlimited.
    pub clock: Option<(u32, u32)>,
    /// Set for games from a position.
    pub initial_fen: Option<&'a str>,
    pub sans: &'a [String],
    /// Who moved first and the fullmove number of the first move (from
    /// the starting FEN).
    pub first_to_move: Side,
    pub first_fullmove: u32,
    /// Clock left after each ply, milliseconds (empty: no `[%clk]`).
    pub clocks_ms: &'a [i64],
}

pub fn result_str(status: Status, winner: Option<Side>) -> &'static str {
    if status.is_ongoing() || status.is_void() {
        return "*";
    }
    match winner {
        Some(Side::White) => "1-0",
        Some(Side::Black) => "0-1",
        None => "1/2-1/2",
    }
}

/// The game as PGN text, ending in a blank line.
#[allow(clippy::many_single_char_names)]
pub fn pgn(g: &PgnGame) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(g.created_at_ms);
    let date = format!("{y:04}.{mo:02}.{d:02}");
    let result = result_str(g.status, g.winner);
    let mut out = String::new();
    let mut tag = |k: &str, v: &str| {
        let _ = writeln!(out, "[{k} \"{}\"]", v.replace('\\', "\\\\").replace('"', "\\\""));
    };
    tag("Event", g.event);
    tag("Site", &g.site);
    tag("Date", &date);
    tag("White", &g.white.name);
    tag("Black", &g.black.name);
    tag("Result", result);
    tag("GameId", g.game_id);
    tag("UTCDate", &date);
    tag("UTCTime", &format!("{h:02}:{mi:02}:{s:02}"));
    for (side, p) in [("White", &g.white), ("Black", &g.black)] {
        if let Some(r) = p.rating {
            tag(&format!("{side}Elo"), &r.to_string());
        }
        if let Some(rd) = p.rating_diff {
            tag(&format!("{side}RatingDiff"), &format!("{rd:+}"));
        }
        if let Some(t) = &p.title {
            tag(&format!("{side}Title"), t);
        }
    }
    tag("Variant", if g.initial_fen.is_some() { "From Position" } else { "Standard" });
    tag("TimeControl", &g.clock.map_or_else(|| "-".to_owned(), |(l, i)| format!("{l}+{i}")));
    tag("Termination", g.status.termination());
    if let Some(fen) = g.initial_fen {
        tag("FEN", fen);
        tag("SetUp", "1");
    }
    out.push('\n');
    out.push_str(&movetext(g));
    if !out.ends_with('\n') && !g.sans.is_empty() {
        out.push(' ');
    }
    out.push_str(result);
    out.push_str("\n\n\n");
    out
}

fn movetext(g: &PgnGame) -> String {
    let mut out = String::new();
    let with_clocks = !g.clocks_ms.is_empty();
    let mut number = g.first_fullmove.max(1);
    let mut side = g.first_to_move;
    let mut after_comment = true;
    for (i, san) in g.sans.iter().enumerate() {
        if !out.is_empty() {
            out.push(' ');
        }
        match side {
            Side::White => {
                let _ = write!(out, "{number}. ");
            }
            Side::Black if after_comment => {
                let _ = write!(out, "{number}... ");
            }
            Side::Black => {}
        }
        out.push_str(san);
        after_comment = false;
        if with_clocks && let Some(ms) = g.clocks_ms.get(i) {
            let _ = write!(out, " {{ [%clk {}] }}", clk(*ms));
            after_comment = true;
        }
        if side == Side::Black {
            number += 1;
        }
        side = side.opposite();
    }
    out
}

fn clk(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

/// (year, month, day, hour, minute, second) for Unix milliseconds, UTC.
fn utc_parts(ms: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (tod / 3600) as u32, ((tod / 60) % 60) as u32, (tod % 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game<'a>(sans: &'a [String], clocks: &'a [i64]) -> PgnGame<'a> {
        PgnGame {
            event: "open-chess casual game",
            site: "https://chess.example/abcd1234".into(),
            game_id: "abcd1234",
            created_at_ms: 1_760_000_000_000,
            white: PgnPlayer { name: "alice".into(), rating: Some(1500), ..Default::default() },
            black: PgnPlayer {
                name: "bob".into(),
                rating: Some(1600),
                title: Some("BOT".into()),
                ..Default::default()
            },
            status: Status::Mate,
            winner: Some(Side::Black),
            clock: Some((180, 2)),
            initial_fen: None,
            sans,
            first_to_move: Side::White,
            first_fullmove: 1,
            clocks_ms: clocks,
        }
    }

    #[test]
    fn dates() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(1_760_000_000_000), (2025, 10, 9, 8, 53, 20));
        assert_eq!(utc_parts(951_782_400_000), (2000, 2, 29, 0, 0, 0));
    }

    #[test]
    fn tags_and_movetext() {
        let sans: Vec<String> = ["f3", "e5", "g4", "Qh4#"].iter().map(|s| (*s).to_owned()).collect();
        let p = pgn(&game(&sans, &[]));
        assert!(
            p.starts_with("[Event \"open-chess casual game\"]\n[Site \"https://chess.example/abcd1234\"]\n")
        );
        assert!(p.contains("[Result \"0-1\"]"));
        assert!(p.contains("[BlackTitle \"BOT\"]"));
        assert!(p.contains("[TimeControl \"180+2\"]"));
        assert!(p.contains("[UTCDate \"2025.10.09\"]"));
        assert!(p.contains("\n\n1. f3 e5 2. g4 Qh4# 0-1\n"), "{p}");
        assert!(!p.contains("FEN"));
    }

    #[test]
    fn clocks_and_black_first() {
        let sans: Vec<String> = ["Kd7", "Ra2"].iter().map(|s| (*s).to_owned()).collect();
        let mut g = game(&sans, &[179_000, 61_000]);
        g.first_to_move = Side::Black;
        g.first_fullmove = 40;
        g.initial_fen = Some("4k3/8/8/8/8/8/8/R3K3 b - - 0 40");
        g.status = Status::Started;
        g.winner = None;
        let p = pgn(&g);
        assert!(p.contains("40... Kd7 { [%clk 0:02:59] } 41. Ra2 { [%clk 0:01:01] } *"), "{p}");
        assert!(p.contains("[SetUp \"1\"]"));
        assert!(p.contains("[Variant \"From Position\"]"));
    }

    #[test]
    fn results() {
        assert_eq!(result_str(Status::Draw, None), "1/2-1/2");
        assert_eq!(result_str(Status::Aborted, None), "*");
        assert_eq!(result_str(Status::Resign, Some(Side::White)), "1-0");
    }
}
