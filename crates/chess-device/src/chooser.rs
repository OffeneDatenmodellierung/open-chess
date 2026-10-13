//! The terminal screen's game picker: what kind of game to play on the
//! board once it's connected. ←/→ change a setting, ↑/↓ move between them,
//! typing fills in a name, a token or an address, Enter starts.
//!
//! For Lichess and an open-chess server the "game" setting also lists what
//! the site has open for the account (looked up once the token is in):
//! games still in progress, to go back to after a disconnect, and
//! challenges waiting to be accepted.

use chess_core::Side;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::choice::{GameChoice, Site, Start};
use crate::controller::TimeControl;
use crate::online::Open;
use crate::settings::Settings;

/// The kinds of game, in the order offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Computer,
    TwoPlayers,
    Lichess,
    Server,
    DgtPage,
}

const KINDS: [Kind; 5] = [Kind::Computer, Kind::TwoPlayers, Kind::Lichess, Kind::Server, Kind::DgtPage];

impl Kind {
    fn label(self, engine: Option<&str>) -> String {
        match self {
            Kind::Computer => match engine {
                Some(e) => format!("play the computer ({e})"),
                None => "play the computer".into(),
            },
            Kind::TwoPlayers => "two players on this board".into(),
            Kind::Lichess => "Lichess (online)".into(),
            Kind::Server => "an open-chess server (online)".into(),
            Kind::DgtPage => "the board for Lichess's DGT page (lichess.org/dgt)".into(),
        }
    }
}

/// The time controls offered (the command line's is added if it's not
/// one of them).
const TIMES: [Option<TimeControl>; 7] = [
    None,
    Some(TimeControl { minutes: 3, increment_secs: 2 }),
    Some(TimeControl { minutes: 5, increment_secs: 3 }),
    Some(TimeControl { minutes: 10, increment_secs: 5 }),
    Some(TimeControl { minutes: 15, increment_secs: 10 }),
    Some(TimeControl { minutes: 30, increment_secs: 0 }),
    Some(TimeControl { minutes: 90, increment_secs: 30 }),
];

/// How an online game starts, before the site's open games.
const STARTS: [&str; 3] = [
    "a new one: wait for it to start on the site",
    "a new one: challenge a player",
    "a new one: the site's computer",
];

/// One line of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Kind,
    Level,
    Play,
    Time,
    Name,
    White,
    Black,
    Token,
    Url,
    Game,
    Opponent,
    SiteLevel,
    SidePlay,
    Rated,
}

impl Row {
    fn text(self) -> bool {
        matches!(self, Row::Name | Row::White | Row::Black | Row::Token | Row::Url | Row::Opponent)
    }
}

/// What the command line and the saved settings start the form with.
#[derive(Debug, Clone)]
pub struct Defaults {
    pub settings: Settings,
    pub level: u8,
    pub play: Side,
    pub time: Option<TimeControl>,
    /// The UCI engine "the computer" is, if not open-chess's.
    pub engine: Option<String>,
    pub lichess_token: Option<String>,
    pub server: Option<String>,
    pub server_token: Option<String>,
    pub livechess: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            settings: Settings::default(),
            level: 4,
            play: Side::White,
            time: None,
            engine: None,
            lichess_token: None,
            server: None,
            server_token: None,
            livechess: "127.0.0.1:1982".into(),
        }
    }
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Start(GameChoice),
}

/// The open games the site has for a token, as last looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Not looked up for this site and token.
    Not,
    Looking,
    Found(Vec<Open>),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Form {
    kind: usize,
    focus: usize,
    level: u8,
    play: Side,
    times: Vec<Option<TimeControl>>,
    time: usize,
    name: String,
    white: String,
    black: String,
    lichess_token: String,
    server_url: String,
    server_token: String,
    start: usize,
    opponent: String,
    site_level: u8,
    /// 0 random, 1 white, 2 black.
    site_play: usize,
    rated: bool,
    engine: Option<String>,
    livechess: String,
    /// The site's open games, for (site, token).
    lookup: Lookup,
    looked_up_for: Option<(String, String)>,
    pub error: Option<String>,
}

impl Form {
    pub fn new(d: &Defaults) -> Form {
        let s = &d.settings;
        let mut times = TIMES.to_vec();
        let saved_time = s.time.parse::<TimeControl>().ok();
        let want = d.time.or(saved_time);
        if let Some(t) = want
            && !times.contains(&Some(t))
        {
            times.push(Some(t));
        }
        let time = times.iter().position(|t| *t == want).unwrap_or(0);
        let or = |a: &Option<String>, b: &str| a.clone().unwrap_or_else(|| b.to_owned());
        Form {
            kind: 0,
            focus: 0,
            level: s.level.unwrap_or(d.level).clamp(1, 8),
            play: d.play,
            times,
            time,
            name: if s.name.is_empty() { crate::settings::login_name() } else { s.name.clone() },
            white: if s.white.is_empty() { "White".into() } else { s.white.clone() },
            black: if s.black.is_empty() { "Black".into() } else { s.black.clone() },
            lichess_token: or(&d.lichess_token, &s.lichess_token),
            server_url: or(&d.server, &s.server_url),
            server_token: or(&d.server_token, &s.server_token),
            start: 0,
            opponent: String::new(),
            site_level: 4,
            site_play: 0,
            rated: false,
            engine: d.engine.clone(),
            livechess: d.livechess.clone(),
            lookup: Lookup::Not,
            looked_up_for: None,
            error: None,
        }
    }

    /// Start on this kind (e.g. what the command line asked for).
    pub fn select(&mut self, kind: Kind) {
        self.kind = KINDS.iter().position(|k| *k == kind).unwrap_or(0);
    }

    pub fn kind(&self) -> Kind {
        KINDS[self.kind]
    }

    fn open(&self) -> &[Open] {
        match &self.lookup {
            Lookup::Found(v) => v,
            _ => &[],
        }
    }

    fn starts(&self) -> usize {
        STARTS.len() + self.open().len()
    }

    fn rows(&self) -> Vec<Row> {
        let mut r = vec![Row::Kind];
        match self.kind() {
            Kind::Computer => r.extend([Row::Level, Row::Play, Row::Time, Row::Name]),
            Kind::TwoPlayers => r.extend([Row::White, Row::Black, Row::Time]),
            Kind::Lichess | Kind::Server => {
                if self.kind() == Kind::Server {
                    r.push(Row::Url);
                }
                r.extend([Row::Token, Row::Game]);
                match self.start {
                    1 => r.extend([Row::Opponent, Row::SidePlay, Row::Time, Row::Rated]),
                    2 => r.extend([Row::SiteLevel, Row::SidePlay, Row::Time]),
                    _ => {}
                }
            }
            Kind::DgtPage => {}
        }
        r
    }

    fn row(&self) -> Row {
        let rows = self.rows();
        rows[self.focus.min(rows.len() - 1)]
    }

    /// Typing goes into the field (so letters aren't commands).
    pub fn typing(&self) -> bool {
        self.row().text()
    }

    /// The site and token to look the open games up for, when they've
    /// changed since the last look (and the game setting is in view).
    pub fn wants_lookup(&mut self) -> Option<(String, String)> {
        let (base, token) = match self.kind() {
            Kind::Lichess => (Site::Lichess.base().to_owned(), self.lichess_token.trim().to_owned()),
            Kind::Server => {
                (self.server_url.trim().trim_end_matches('/').to_owned(), self.server_token.trim().to_owned())
            }
            _ => return None,
        };
        if base.is_empty() || token.is_empty() || self.row().text() {
            return None;
        }
        let key = Some((base.clone(), token.clone()));
        if self.looked_up_for == key {
            return None;
        }
        self.looked_up_for = key;
        self.lookup = Lookup::Looking;
        self.start = self.start.min(STARTS.len() - 1);
        Some((base, token))
    }

    /// The answer to [`Form::wants_lookup`], if it's still for this site
    /// and token.
    pub fn looked_up(&mut self, base: &str, token: &str, result: Result<Vec<Open>, String>) {
        if self.looked_up_for.as_ref().is_some_and(|(b, t)| b == base && t == token) {
            self.lookup = match result {
                Ok(v) => Lookup::Found(v),
                Err(e) => Lookup::Failed(e),
            };
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Action {
        let rows = self.rows();
        self.focus = self.focus.min(rows.len() - 1);
        let row = rows[self.focus];
        match code {
            KeyCode::Up | KeyCode::BackTab => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1).min(rows.len() - 1),
            KeyCode::Left => self.cycle(row, -1),
            KeyCode::Right => self.cycle(row, 1),
            KeyCode::Enter => {
                self.error = None;
                match self.choice() {
                    Ok(c) => return Action::Start(c),
                    Err(e) => self.error = Some(e),
                }
            }
            KeyCode::Backspace => {
                if let Some(t) = self.text_mut(row) {
                    t.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(t) = self.text_mut(row) {
                    t.push(c);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn text_mut(&mut self, row: Row) -> Option<&mut String> {
        Some(match row {
            Row::Name => &mut self.name,
            Row::White => &mut self.white,
            Row::Black => &mut self.black,
            Row::Token if self.kind() == Kind::Lichess => &mut self.lichess_token,
            Row::Token => &mut self.server_token,
            Row::Url => &mut self.server_url,
            Row::Opponent => &mut self.opponent,
            _ => return None,
        })
    }

    fn cycle(&mut self, row: Row, by: i32) {
        let step = |at: usize, n: usize| -> usize {
            let n = i32::try_from(n.max(1)).unwrap_or(1);
            usize::try_from((i32::try_from(at).unwrap_or(0) + by).rem_euclid(n)).unwrap_or(0)
        };
        match row {
            Row::Kind => {
                self.kind = step(self.kind, KINDS.len());
                self.focus = 0;
            }
            Row::Level => self.level = (u8::try_from(step(usize::from(self.level - 1), 8)).unwrap_or(0)) + 1,
            Row::SiteLevel => {
                self.site_level = (u8::try_from(step(usize::from(self.site_level - 1), 8)).unwrap_or(0)) + 1;
            }
            Row::Play => self.play = self.play.opposite(),
            Row::SidePlay => self.site_play = step(self.site_play, 3),
            Row::Time => self.time = step(self.time, self.times.len()),
            Row::Game => self.start = step(self.start, self.starts()),
            Row::Rated => self.rated = !self.rated,
            _ => {}
        }
    }

    fn time(&self) -> Option<TimeControl> {
        self.times[self.time]
    }

    /// The game the form describes, or what's missing.
    pub fn choice(&self) -> Result<GameChoice, String> {
        let colour = [None, Some(Side::White), Some(Side::Black)][self.site_play];
        Ok(match self.kind() {
            Kind::Computer => GameChoice::Computer {
                name: self.name.trim().to_owned(),
                level: self.level,
                play: self.play,
                time: self.time(),
            },
            Kind::TwoPlayers => GameChoice::TwoPlayers {
                white: self.white.trim().to_owned(),
                black: self.black.trim().to_owned(),
                time: self.time(),
            },
            Kind::DgtPage => GameChoice::DgtPage { addr: self.livechess.clone() },
            Kind::Lichess | Kind::Server => {
                let (site, token) = if self.kind() == Kind::Lichess {
                    (Site::Lichess, self.lichess_token.trim())
                } else {
                    let url = self.server_url.trim().trim_end_matches('/');
                    if url.is_empty() {
                        return Err("type the server's address, e.g. https://chess.example.org".into());
                    }
                    (Site::Server(url.to_owned()), self.server_token.trim())
                };
                if token.is_empty() {
                    return Err("type the account's API token (with the board:play scope)".into());
                }
                let start = match self.start {
                    0 => Start::Wait,
                    1 if self.opponent.trim().is_empty() => return Err("type the player to challenge".into()),
                    1 => Start::Challenge(self.opponent.trim().to_owned()),
                    2 => Start::Computer(self.site_level),
                    n => match self.open().get(n - STARTS.len()) {
                        Some(Open::Playing { id, .. }) => Start::Resume(id.clone()),
                        Some(Open::Challenged { id, .. }) => Start::Accept(id.clone()),
                        None => Start::Wait,
                    },
                };
                GameChoice::Online {
                    site,
                    token: token.to_owned(),
                    start,
                    play: colour,
                    time: self.time(),
                    rated: self.rated && self.start == 1,
                }
            }
        })
    }

    /// What to remember for next time.
    pub fn settings(&self) -> Settings {
        Settings {
            name: self.name.trim().to_owned(),
            white: self.white.trim().to_owned(),
            black: self.black.trim().to_owned(),
            level: Some(self.level),
            time: self.time().map(|t| t.to_string()).unwrap_or_default(),
            lichess_token: self.lichess_token.trim().to_owned(),
            server_url: self.server_url.trim().to_owned(),
            server_token: self.server_token.trim().to_owned(),
        }
    }

    fn value(&self, row: Row) -> String {
        let time = |t: Option<TimeControl>| t.map_or_else(|| "no clock".to_owned(), |t| t.to_string());
        match row {
            Row::Kind => self.kind().label(self.engine.as_deref()),
            Row::Level if self.engine.is_some() => format!("{} (open-chess's engine only)", self.level),
            Row::Level => format!("{} (rated {})", self.level, crate::ratings::engine_rating(self.level)),
            Row::SiteLevel => self.site_level.to_string(),
            Row::Play => self.play.name().to_owned(),
            Row::SidePlay => ["random", "white", "black"][self.site_play].to_owned(),
            Row::Time => time(self.time()),
            Row::Name => self.name.clone(),
            Row::White => self.white.clone(),
            Row::Black => self.black.clone(),
            Row::Url => self.server_url.clone(),
            Row::Opponent => self.opponent.clone(),
            Row::Rated => if self.rated { "yes" } else { "no" }.to_owned(),
            Row::Token => {
                let t = if self.kind() == Kind::Lichess { &self.lichess_token } else { &self.server_token };
                mask(t)
            }
            Row::Game => {
                let n = self.start;
                let base = if n < STARTS.len() {
                    STARTS[n].to_owned()
                } else {
                    self.open().get(n - STARTS.len()).map(Open::describe).unwrap_or_default()
                };
                let found = match &self.lookup {
                    Lookup::Looking => "  (looking for your open games…)".to_owned(),
                    Lookup::Found(v) if !v.is_empty() => format!("  ({} open: ←/→)", v.len()),
                    Lookup::Failed(e) => format!("  (couldn't look: {e})"),
                    _ => String::new(),
                };
                format!("{base}{found}")
            }
        }
    }

    fn label(row: Row) -> &'static str {
        match row {
            Row::Kind => "Game type",
            Row::Level => "Level",
            Row::SiteLevel => "Its level",
            Row::Play | Row::SidePlay => "You play",
            Row::Time => "Clock",
            Row::Name => "Your name",
            Row::White => "White",
            Row::Black => "Black",
            Row::Token => "API token",
            Row::Url => "Server",
            Row::Game => "Game",
            Row::Opponent => "Challenge",
            Row::Rated => "Rated",
        }
    }

    /// The form as lines, the focused one marked.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let rows = self.rows();
        let focus = self.focus.min(rows.len() - 1);
        let mut out: Vec<Line> = rows
            .iter()
            .enumerate()
            .map(|(i, &r)| {
                let here = i == focus;
                let arrows = if here && !r.text() { "◀ ▶ " } else { "" };
                let cursor = if here && r.text() { "▏" } else { "" };
                let style =
                    if here { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                Line::from(vec![
                    Span::styled(format!("{:>10}: ", Form::label(r)), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}{cursor}", self.value(r)), style),
                    Span::styled(format!(" {arrows}"), Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect();
        let hint = match self.kind() {
            Kind::DgtPage => format!(
                "Enter, then open lichess.org/dgt in a browser on this computer (ws://{}).",
                self.livechess
            ),
            Kind::TwoPlayers => "Ratings are kept for both names. Set the pieces up and Enter.".into(),
            Kind::Computer if self.engine.is_none() => {
                "Set the pieces up and Enter. Rated against the level.".into()
            }
            Kind::Computer => "Set the pieces up and Enter.".into(),
            Kind::Lichess => "Token: lichess.org/account/oauth/token (board:play, challenge:write).".into(),
            Kind::Server => "Token: the server's account page (board:play, challenge:write).".into(),
        };
        out.push(Line::from(""));
        out.push(Line::styled(hint, Style::default().fg(Color::DarkGray)));
        if let Some(e) = &self.error {
            out.push(Line::styled(e.clone(), Style::default().fg(Color::Red)));
        }
        out
    }
}

/// A token, all but its prefix and last four characters hidden.
fn mask(t: &str) -> String {
    let n = t.chars().count();
    if n <= 8 {
        return "•".repeat(n);
    }
    let head: String = t.chars().take(4).collect();
    let tail: String = t.chars().skip(n - 4).collect();
    format!("{head}{}{tail}", "•".repeat(6))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new(&Defaults { level: 4, livechess: "127.0.0.1:1982".into(), ..Defaults::default() })
    }

    #[test]
    fn plays_the_computer_by_default() {
        let mut f = form();
        f.key(KeyCode::Down); // level
        f.key(KeyCode::Right);
        f.key(KeyCode::Down); // you play
        f.key(KeyCode::Right);
        let Action::Start(GameChoice::Computer { level, play, time, .. }) = f.key(KeyCode::Enter) else {
            panic!("no game")
        };
        assert_eq!((level, play, time), (5, Side::Black, None));
    }

    #[test]
    fn two_players_type_their_names() {
        let mut f = form();
        f.key(KeyCode::Right); // two players
        f.key(KeyCode::Down);
        for _ in 0..5 {
            f.key(KeyCode::Backspace);
        }
        for c in "Ann".chars() {
            f.key(KeyCode::Char(c));
        }
        assert!(f.typing());
        let Action::Start(GameChoice::TwoPlayers { white, black, .. }) = f.key(KeyCode::Enter) else {
            panic!("no game")
        };
        assert_eq!((white.as_str(), black.as_str()), ("Ann", "Black"));
    }

    #[test]
    fn online_needs_a_token_then_offers_the_open_games() {
        let mut f = form();
        f.select(Kind::Server);
        f.focus = 1;
        for c in "http://localhost:8080".chars() {
            f.key(KeyCode::Char(c));
        }
        assert!(matches!(f.key(KeyCode::Enter), Action::None));
        assert!(f.error.as_deref().is_some_and(|e| e.contains("token")));
        f.key(KeyCode::Down);
        for c in "secret-token".chars() {
            f.key(KeyCode::Char(c));
        }
        assert_eq!(f.wants_lookup(), None, "not while typing");
        f.key(KeyCode::Down); // the game row
        assert_eq!(f.wants_lookup(), Some(("http://localhost:8080".into(), "secret-token".into())));
        assert_eq!(f.wants_lookup(), None, "looked up once");
        f.looked_up(
            "http://localhost:8080",
            "secret-token",
            Ok(vec![Open::Playing {
                id: "g1".into(),
                opponent: "bob".into(),
                side: Side::Black,
                my_turn: true,
            }]),
        );
        f.key(KeyCode::Left); // wraps round to the open game
        assert!(f.value(Row::Game).contains("resume v bob"));
        let Action::Start(GameChoice::Online { start, site, .. }) = f.key(KeyCode::Enter) else {
            panic!("no game")
        };
        assert_eq!(start, Start::Resume("g1".into()));
        assert_eq!(site, Site::Server("http://localhost:8080".into()));
        assert_eq!(mask("lip_abcdefghij"), "lip_••••••ghij");
    }
}
