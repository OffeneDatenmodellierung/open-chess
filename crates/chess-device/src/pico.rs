//! PicoChess's web API, so tools written for PicoChess work with the
//! device: the `/event` websocket and its messages, `/channel` commands,
//! and `/dgt` and `/info` queries (as PicoChess v4's `server.py` has them).
//!
//! The messages are made here from the device's [`View`] of the game,
//! without I/O: a new game is a `Game` message, each move a `Fen` (played
//! by the `user` or the `computer`), a takeback a `Fen` with `reload`, the
//! engine's move waiting for the board a `Light`, its arrival a `Clear`,
//! the end a `GameEnd`, and the clocks a `Clock` with PicoChess's HTML.

use std::time::Instant;

use chess_core::{Game, Side, Status};
use chess_core::{PgnGame, PgnPlayer, pgn, result_str};
use serde_json::{Value, json};

use crate::view::View;

/// The game as PGN, PicoChess's way (headers, no comments).
fn pgn_of(v: &View, white: &str, black: &str) -> String {
    let game = Game::from_moves(v.initial_fen.as_deref(), &v.moves.join(" ")).ok();
    let sans: Vec<String> = game.as_ref().map(|g| g.sans().to_vec()).unwrap_or_default();
    let first = game.as_ref().map_or(Side::White, Game::first_to_move);
    let first_fullmove =
        v.initial_fen.as_deref().and_then(|f| f.split(' ').nth(5)).and_then(|n| n.parse().ok()).unwrap_or(1);
    let player = |name: &str| PgnPlayer { name: name.into(), rating: None, rating_diff: None, title: None };
    pgn(&PgnGame {
        event: "open-chess",
        site: String::new(),
        game_id: &v.game_id,
        created_at_ms: v.created_ms,
        white: player(white),
        black: player(black),
        status: v.status,
        winner: v.winner,
        clock: v.time.map(|t| (t.minutes * 60, t.increment_secs)),
        initial_fen: v.initial_fen.as_deref(),
        sans: &sans,
        first_to_move: first,
        first_fullmove,
        clocks_ms: &[],
    })
}

fn names(v: &View) -> (String, String) {
    let engine = v.engine.name.clone().unwrap_or_else(|| format!("open-chess level {}", v.level));
    if v.player == Side::White { ("Player".into(), engine) } else { (engine, "Player".into()) }
}

/// The PGN headers PicoChess's clients show.
pub fn headers(v: &View) -> Value {
    let (white, black) = names(v);
    json!({
        "Event": "open-chess",
        "Site": "",
        "White": white,
        "Black": black,
        "Result": result_str(v.status, v.winner),
    })
}

fn board_message(v: &View, event: &str, mv: &str, play: &str) -> Value {
    let (white, black) = names(v);
    json!({"pgn": pgn_of(v, &white, &black), "fen": v.fen, "event": event, "move": mv, "play": play})
}

/// The latest board message, as `/dgt?action=get_last_move` answers and a
/// newly connected client gets first.
pub fn last_move(v: &View) -> Value {
    match v.moves.last() {
        None => board_message(v, "Game", "0000", "newgame"),
        Some(m) => board_message(v, "Fen", m, "reload"),
    }
}

/// The clock as PicoChess's web clock shows it (HTML).
pub fn clock_text(v: &View, now: Instant) -> Option<String> {
    let [w, b] = v.clocks_at(now)?;
    let hms = |ms: i64| {
        let s = (ms.max(0) + 999) / 1000;
        format!("{}:{:02}.{:02}", s / 3600, (s / 60) % 60, s % 60)
    };
    let (icon, l_cls, r_cls) = match v.running {
        None => ("fa-sort", "ctime-l", "ctime-r"),
        Some(Side::White) => ("fa-caret-left", "ctime-l ctime-active", "ctime-r ctime-inactive"),
        Some(Side::Black) => ("fa-caret-right", "ctime-l ctime-inactive", "ctime-r ctime-active"),
    };
    Some(format!(
        "<span class=\"{l_cls}\">{}</span><i class=\"fa {icon}\"></i><span class=\"{r_cls}\">{}</span>",
        hms(w),
        hms(b)
    ))
}

pub fn clock_message(v: &View, now: Instant) -> Option<Value> {
    clock_text(v, now)
        .map(|t| json!({"event": "Clock", "msg": t, "running": v.running.is_some(), "menu_active": false}))
}

/// What a client sees on connecting.
pub fn greeting(v: &View, now: Instant) -> Vec<Value> {
    let mut out = vec![last_move(v), json!({"event": "Header", "headers": headers(v)})];
    if v.awaiting_board
        && let Some(m) = v.moves.last()
    {
        out.push(json!({"event": "Light", "move": m}));
    }
    out.extend(clock_message(v, now));
    out
}

/// The messages for the game going from `old` to `new`.
pub fn changes(old: &View, new: &View, now: Instant) -> Vec<Value> {
    let mut out = Vec::new();
    if old.game_id != new.game_id {
        out.push(board_message(new, "Game", "0000", "newgame"));
        out.push(json!({"event": "Header", "headers": headers(new)}));
        out.extend(clock_message(new, now));
        return out;
    }
    let grew = new.moves.len() > old.moves.len() && new.moves.starts_with(&old.moves);
    if grew {
        let m = new.moves.last().expect("a move");
        let mover = Side::at_ply(
            if new.initial_fen.as_deref().is_some_and(|f| f.split(' ').nth(1) == Some("b")) {
                Side::Black
            } else {
                Side::White
            },
            new.moves.len() - 1,
        );
        if mover == new.player {
            out.push(board_message(new, "Fen", m, "user"));
        } else if new.awaiting_board {
            // The engine's move, waiting for the board.
            out.push(json!({"event": "Light", "move": m}));
        } else {
            out.push(board_message(new, "Fen", m, "computer"));
        }
    } else if new.moves != old.moves {
        out.push(board_message(new, "Fen", new.moves.last().map_or("0000", String::as_str), "reload"));
    }
    if old.awaiting_board && !new.awaiting_board && new.moves == old.moves {
        // The engine's move made on the board.
        let m = new.moves.last().map_or("0000", String::as_str);
        out.push(json!({"event": "Clear"}));
        out.push(board_message(new, "Fen", m, "computer"));
    }
    if old.status == Status::Started && new.status != Status::Started {
        out.push(json!({"event": "GameEnd", "result": result_str(new.status, new.winner)}));
    }
    if old.player != new.player || old.level != new.level {
        out.push(json!({"event": "Header", "headers": headers(new)}));
    }
    if old.running != new.running || old.paused != new.paused || grew {
        out.extend(clock_message(new, now));
    }
    out
}

/// A `/channel` command, as far as the device acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A clock button (0–4, from the left).
    Button(u8),
    /// A new time control: minutes and increment, or none.
    Time(Option<(u32, u32)>),
    /// Something PicoChess does that the device doesn't (moves are made on
    /// the board).
    Unsupported(String),
}

/// Read a `/channel` POST's form fields.
pub fn command<S: std::hash::BuildHasher>(form: &std::collections::HashMap<String, String, S>) -> Command {
    let get = |k: &str| form.get(k).map_or("", String::as_str);
    let int = |k: &str| get(k).trim().parse::<u32>().unwrap_or(0);
    match get("action") {
        "clockbutton" => match get("button").trim().parse::<u8>() {
            Ok(b @ 0..=4) => Command::Button(b),
            _ => Command::Unsupported(format!("clockbutton {}", get("button"))),
        },
        "new_time" => match int("time_mode") {
            // Blitz (minutes) and Fischer (minutes + increment).
            1 | 2 if int("time") > 0 => Command::Time(Some((int("time"), int("fischer")))),
            _ => Command::Time(None),
        },
        other => Command::Unsupported(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Controller, Setup};
    use crate::view::{EngineInfo, game_id};
    use chess_dgt::{Follower, Session};
    use std::collections::HashMap;

    fn view(moves: &[&str], awaiting: bool) -> View {
        let c = Controller::new(Setup { time: Some("5+3".parse().unwrap()), ..Setup::default() });
        let mut s = Session::new(Follower::new());
        for m in moves {
            s.follower_mut().play(m).unwrap();
        }
        let mut v = View::new(&c, &s, &EngineInfo { name: None }, game_id(1, 1), 0, Instant::now());
        v.awaiting_board = awaiting;
        v
    }

    #[test]
    fn a_game_reads_as_picochess_messages() {
        let now = Instant::now();
        let start = view(&[], false);
        let g = greeting(&start, now);
        assert_eq!(g[0]["event"], "Game");
        assert_eq!(g[0]["play"], "newgame");
        assert_eq!(g[1]["headers"]["Black"], "open-chess level 4");
        assert_eq!(g[2]["event"], "Clock");
        assert!(g[2]["msg"].as_str().unwrap().contains("0:05.00"));

        // The player's move.
        let e4 = view(&["e2e4"], false);
        let m = changes(&start, &e4, now);
        assert_eq!(
            (m[0]["event"].as_str(), m[0]["play"].as_str(), m[0]["move"].as_str()),
            (Some("Fen"), Some("user"), Some("e2e4"))
        );
        assert!(m[0]["pgn"].as_str().unwrap().contains("1. e4"));

        // The engine's reply: lit while it waits, then made.
        let waiting = view(&["e2e4", "e7e5"], true);
        let m = changes(&e4, &waiting, now);
        assert_eq!((m[0]["event"].as_str(), m[0]["move"].as_str()), (Some("Light"), Some("e7e5")));
        let made = view(&["e2e4", "e7e5"], false);
        let m = changes(&waiting, &made, now);
        assert_eq!(m[0]["event"], "Clear");
        assert_eq!((m[1]["event"].as_str(), m[1]["play"].as_str()), (Some("Fen"), Some("computer")));

        // A takeback.
        let m = changes(&made, &e4, now);
        assert_eq!((m[0]["event"].as_str(), m[0]["play"].as_str()), (Some("Fen"), Some("reload")));
    }

    #[test]
    fn reads_channel_commands() {
        let f = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        assert_eq!(command(&f(&[("action", "clockbutton"), ("button", "3")])), Command::Button(3));
        assert_eq!(
            command(&f(&[("action", "new_time"), ("time_mode", "2"), ("time", "5"), ("fischer", "3")])),
            Command::Time(Some((5, 3)))
        );
        assert_eq!(
            command(&f(&[("action", "new_time"), ("time_mode", "0"), ("time", "10")])),
            Command::Time(None)
        );
        assert_eq!(command(&f(&[("action", "move")])), Command::Unsupported("move".into()));
    }
}
