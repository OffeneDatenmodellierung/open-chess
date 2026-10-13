//! The terminal screen: pick a board, pick a game, watch it, read the log.
//!
//! Boards found nearby (Bluetooth LE) and attached (USB and serial ports,
//! Chessnut boards on USB) are listed to choose from. Once one is
//! connected the screen asks what to play ([`crate::chooser`]): the
//! computer, two people on the board, Lichess or an open-chess server (a
//! new game, one still in progress, or a challenge waiting), or the board
//! for Lichess's DGT page. Then it shows the board, the players and their
//! clocks, the opening and every move in words beside its notation (newest
//! first), and the log newest first: the board, the engine, the online
//! game, and every request from the app, PicoChess's tools and Lichess's
//! DGT page. When the game ends it says who won and what it did to the
//! players' ratings; `n` picks the next game on the same board.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use chess_core::Side;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::watch;

use crate::choice::GameChoice;
pub use crate::chooser::Defaults;
use crate::chooser::{Action, Form};
use crate::discover::Found;
use crate::log::{self, Source};
use crate::online::Open;
use crate::theme;
use crate::view::View;

/// Games for a connected board, each with the flag that ends it.
pub type Games = mpsc::Receiver<(GameChoice, Arc<AtomicBool>)>;

/// Connects to a board: returns the thread holding it, which plays the
/// games that come on the channel and stops when the flag is set (or the
/// board fails).
pub type Connect = dyn Fn(Found, Arc<AtomicBool>, Games) -> JoinHandle<io::Result<()>>;

/// Lists the boards there are (slow: it scans Bluetooth).
pub type Scan = dyn Fn() -> Vec<Found> + Send + Sync;

/// A board connected, or connecting.
struct Connected {
    found: Found,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
    games: mpsc::Sender<(GameChoice, Arc<AtomicBool>)>,
}

impl Connected {
    fn open(connect: &Connect, found: Found) -> Connected {
        log::line(Source::Device, format!("connecting to {}", found.describe()));
        let stop = Arc::new(AtomicBool::new(false));
        let (games, rx) = mpsc::channel();
        let thread = Some(connect(found.clone(), stop.clone(), rx));
        Connected { found, stop, thread, games }
    }

    /// Start a game on the board: the flag ends it.
    fn play(&self, choice: GameChoice) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        let _ = self.games.send((choice, flag.clone()));
        flag
    }
}

enum State {
    Picking { list: ListState },
    Choosing { board: Connected },
    Playing { board: Connected, game: Arc<AtomicBool> },
}

/// Which pane ↑/↓ scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Moves,
    Log,
}

/// A look-up of the site's open games, when it's answered.
type Looked = Arc<Mutex<Option<(String, String, Result<Vec<Open>, String>)>>>;

struct App {
    state: State,
    found: Arc<Mutex<Vec<Found>>>,
    scanning: Arc<AtomicBool>,
    log_scroll: usize,
    moves_scroll: usize,
    pane: Pane,
    title: String,
    /// Where the log is also written.
    log_file: Option<std::path::PathBuf>,
    form: Form,
    looked: Looked,
    /// Where the top half is on screen (the mouse wheel scrolls the moves
    /// there, the log below).
    top: Rect,
}

/// Run the screen. `start` connects at once (a board named on the command
/// line) and plays `first` if given; otherwise it starts by listing boards.
pub fn run(
    title: &str,
    view: &watch::Receiver<View>,
    scan: &Arc<Scan>,
    connect: &Connect,
    start: Option<Found>,
    first: Option<GameChoice>,
    defaults: &Defaults,
) -> io::Result<()> {
    log::quiet(true);
    let log_file = log::file_path();
    let mut terminal = ratatui::init();
    // The mouse wheel scrolls (hold Option on macOS, or Shift elsewhere,
    // to select text; --log-file keeps the log in a file).
    let _ = execute!(io::stdout(), EnableMouseCapture);
    let mut app = App {
        state: State::Picking { list: ListState::default().with_selected(Some(0)) },
        found: Arc::new(Mutex::new(Vec::new())),
        scanning: Arc::new(AtomicBool::new(false)),
        log_scroll: 0,
        moves_scroll: 0,
        pane: Pane::Log,
        title: title.to_owned(),
        log_file,
        form: Form::new(defaults),
        looked: Arc::new(Mutex::new(None)),
        top: Rect::default(),
    };
    let result = main_loop(&mut terminal, &mut app, view, scan, connect, start, first);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    log::quiet(false);
    result
}

fn rescan(app: &App, scan: &Arc<Scan>) {
    if app.scanning.swap(true, Ordering::SeqCst) {
        return;
    }
    let (found, scanning, scan) = (app.found.clone(), app.scanning.clone(), scan.clone());
    std::thread::spawn(move || {
        log::line(Source::Device, "looking for boards…");
        let list = scan();
        log::line(Source::Device, format!("found {} board(s)", list.len()));
        *found.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = list;
        scanning.store(false, Ordering::SeqCst);
    });
}

fn picking() -> State {
    State::Picking { list: ListState::default().with_selected(Some(0)) }
}

/// Look up the site's open games for the form, on a thread.
fn look_up(app: &mut App) {
    if !matches!(app.state, State::Choosing { .. }) {
        return;
    }
    if let Some((base, token)) = app.form.wants_lookup() {
        let looked = app.looked.clone();
        std::thread::spawn(move || {
            let r = crate::online::open_games(&base, &token).map_err(|e| format!("{e:#}"));
            *looked.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((base, token, r));
        });
    }
    let answer = app.looked.lock().ok().and_then(|mut l| l.take());
    if let Some((base, token, r)) = answer {
        app.form.looked_up(&base, &token, r);
    }
}

fn stop_all(state: &State) {
    match state {
        State::Playing { board, game } => {
            game.store(true, Ordering::SeqCst);
            board.stop.store(true, Ordering::SeqCst);
        }
        State::Choosing { board } => board.stop.store(true, Ordering::SeqCst),
        State::Picking { .. } => {}
    }
}

fn main_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    view: &watch::Receiver<View>,
    scan: &Arc<Scan>,
    connect: &Connect,
    start: Option<Found>,
    first: Option<GameChoice>,
) -> io::Result<()> {
    match start {
        Some(f) => {
            let board = Connected::open(connect, f);
            app.state = match first {
                Some(c) => {
                    let game = board.play(c);
                    State::Playing { board, game }
                }
                None => State::Choosing { board },
            };
        }
        None => rescan(app, scan),
    }
    loop {
        // A board that stopped (failed, or unplugged): back to the list.
        if let State::Choosing { board } | State::Playing { board, .. } = &mut app.state
            && board.thread.as_ref().is_some_and(JoinHandle::is_finished)
        {
            let what = board.found.describe();
            match board.thread.take().map(JoinHandle::join) {
                Some(Ok(Err(e))) => log::line(Source::Device, format!("{what}: {e}")),
                Some(Err(_)) => log::line(Source::Device, format!("{what}: stopped unexpectedly")),
                _ => log::line(Source::Device, format!("{what}: disconnected")),
            }
            app.state = picking();
        }
        look_up(app);
        let v = view.borrow().clone();
        terminal.draw(|f| draw(f, app, &v))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let k = match event::read()? {
            Event::Key(k) => k,
            Event::Mouse(m) => {
                let over_top = m.row < app.top.y + app.top.height;
                let scroll = if over_top { &mut app.moves_scroll } else { &mut app.log_scroll };
                match m.kind {
                    // Newest at the top: down goes back in time.
                    MouseEventKind::ScrollDown => *scroll += 3,
                    MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
        let typing = matches!(app.state, State::Choosing { .. }) && app.form.typing();
        let quit = ctrl_c
            || (!typing && matches!(k.code, KeyCode::Char('q')))
            || (k.code == KeyCode::Esc && !matches!(app.state, State::Choosing { .. }));
        if quit {
            stop_all(&app.state);
            return Ok(());
        }
        match k.code {
            KeyCode::PageDown => app.log_scroll += 10,
            KeyCode::PageUp => app.log_scroll = app.log_scroll.saturating_sub(10),
            KeyCode::Home => {
                app.log_scroll = 0;
                app.moves_scroll = 0;
            }
            _ => {}
        }
        let state = std::mem::replace(&mut app.state, picking());
        app.state = next(app, state, k.code, scan, connect);
    }
}

/// What a key does in each state.
fn next(app: &mut App, state: State, code: KeyCode, scan: &Arc<Scan>, connect: &Connect) -> State {
    let count = app.found.lock().map_or(0, |l| l.len());
    match state {
        State::Picking { mut list } => {
            match code {
                KeyCode::Up => list.select(Some(list.selected().unwrap_or(0).saturating_sub(1))),
                KeyCode::Down => {
                    list.select(Some((list.selected().unwrap_or(0) + 1).min(count.saturating_sub(1))));
                }
                KeyCode::Char('r') => rescan(app, scan),
                KeyCode::Enter => {
                    let chosen = list.selected().and_then(|i| app.found.lock().ok()?.get(i).cloned());
                    if let Some(f) = chosen {
                        return State::Choosing { board: Connected::open(connect, f) };
                    }
                }
                _ => {}
            }
            State::Picking { list }
        }
        State::Choosing { board } => match code {
            KeyCode::Esc => {
                board.stop.store(true, Ordering::SeqCst);
                picking()
            }
            code => match app.form.key(code) {
                Action::Start(choice) => {
                    app.form.settings().save();
                    app.moves_scroll = 0;
                    let game = board.play(choice);
                    State::Playing { board, game }
                }
                Action::None => State::Choosing { board },
            },
        },
        State::Playing { board, game } => match code {
            KeyCode::Char('b') => {
                game.store(true, Ordering::SeqCst);
                board.stop.store(true, Ordering::SeqCst);
                picking()
            }
            KeyCode::Char('n') => {
                log::line(Source::Device, "choose the next game");
                game.store(true, Ordering::SeqCst);
                State::Choosing { board }
            }
            code => {
                let scroll =
                    if app.pane == Pane::Moves { &mut app.moves_scroll } else { &mut app.log_scroll };
                match code {
                    KeyCode::Tab => app.pane = if app.pane == Pane::Moves { Pane::Log } else { Pane::Moves },
                    KeyCode::Down => *scroll += 1,
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    _ => {}
                }
                State::Playing { board, game }
            }
        },
    }
}

/// A bordered panel in the theme's colours.
fn panel<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    let t = theme::get();
    Block::default()
        .borders(Borders::ALL)
        .border_style(t.border())
        .title(title.into().patch_style(t.title()))
        .style(t.base())
}

fn draw(f: &mut Frame, app: &mut App, v: &View) {
    f.render_widget(Block::default().style(theme::get().base()), f.area());
    let height = f.area().height;
    let top_height = (height.saturating_mul(3) / 5).max(13);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(top_height), Constraint::Min(5), Constraint::Length(1)])
        .split(f.area());
    app.top = rows[0];
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(30)])
        .split(rows[0]);
    draw_board(f, top[0], v);
    match &mut app.state {
        State::Picking { list } => {
            let found = app.found.lock().map(|l| l.clone()).unwrap_or_default();
            let scanning = app.scanning.load(Ordering::SeqCst);
            let items: Vec<ListItem> = if found.is_empty() {
                vec![ListItem::new(if scanning {
                    "looking…"
                } else {
                    "none found: plug a board in, or switch it on, then r"
                })]
            } else {
                found.iter().map(|b| ListItem::new(b.describe())).collect()
            };
            let title =
                format!(" {}: choose a board{} ", app.title, if scanning { " (looking…)" } else { "" });
            let pick = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(4), Constraint::Min(3)])
                .split(top[1]);
            f.render_widget(
                Paragraph::new(theme::logo("play chess on a real board")).style(theme::get().base()),
                pick[0],
            );
            let widget = List::new(items)
                .block(panel(theme::brand_title(&title)))
                .highlight_style(Style::default().bg(theme::get().action).fg(theme::get().paper))
                .highlight_symbol("▶ ");
            f.render_stateful_widget(widget, pick[1], list);
        }
        State::Choosing { board } => {
            let title = format!(" {}: {}: choose a game ", app.title, board.found.describe());
            f.render_widget(
                Paragraph::new(app.form.lines())
                    .wrap(Wrap { trim: false })
                    .block(panel(theme::brand_title(&title))),
                top[1],
            );
        }
        State::Playing { board, .. } => {
            let mut title = format!(" {}: {} ", app.title, board.found.describe());
            if app.pane == Pane::Moves {
                title.push_str("(↑↓ scroll the moves) ");
            }
            app.moves_scroll = app.moves_scroll.min(v.moves.len().saturating_sub(1));
            draw_game(f, top[1], v, &title, app.moves_scroll);
        }
    }
    app.log_scroll = app.log_scroll.min(log::len().saturating_sub(1));
    let log_title = if app.pane == Pane::Log && matches!(app.state, State::Playing { .. }) {
        " (↑↓ scroll)"
    } else {
        ""
    };
    draw_log(f, rows[1], app.log_scroll, app.log_file.as_deref(), log_title);
    let keys = match app.state {
        State::Picking { .. } => " ↑↓ choose · Enter connect · r look again · PgUp/PgDn log · q quit",
        State::Choosing { .. } => {
            " ↑↓ setting · ←→ change it · type to fill in · Enter start · Esc back to the boards"
        }
        State::Playing { .. } => {
            " n next game · b back to the boards · Tab moves/log · ↑↓ or wheel scroll · Home newest · q quit"
        }
    };
    f.render_widget(Paragraph::new(keys).style(theme::get().base().fg(theme::get().muted)), rows[2]);
}

/// The position's squares, a8 first, from a FEN.
fn squares(fen: &str) -> [Option<char>; 64] {
    let mut out = [None; 64];
    let placement = fen.split(' ').next().unwrap_or("");
    for (row, rank) in placement.split('/').enumerate().take(8) {
        let mut file = 0usize;
        for c in rank.chars() {
            if let Some(n) = c.to_digit(10) {
                file += n as usize;
            } else if file < 8 {
                out[row * 8 + file] = Some(c);
                file += 1;
            }
        }
    }
    out
}

/// A piece's glyph: the solid shapes for both sides (told apart by colour,
/// which reads better than outlines on a terminal's squares).
fn glyph(c: char) -> char {
    match c.to_ascii_lowercase() {
        'k' => '♚',
        'q' => '♛',
        'r' => '♜',
        'b' => '♝',
        'n' => '♞',
        'p' => '♟',
        _ => ' ',
    }
}

/// The board as text lines: White at the bottom unless the person plays
/// Black; the last move's squares marked.
pub fn board_lines(v: &View) -> Vec<Line<'static>> {
    let sq = squares(&v.fen);
    let last: Vec<usize> = v
        .moves
        .last()
        .map(|m| {
            let b = m.as_bytes();
            let idx = |f: u8, r: u8| usize::from(b'8' - r) * 8 + usize::from(f - b'a');
            if b.len() >= 4 { vec![idx(b[0], b[1]), idx(b[2], b[3])] } else { Vec::new() }
        })
        .unwrap_or_default();
    let flipped = v.player == Side::Black;
    let t = theme::get();
    let mut lines = Vec::new();
    for r in 0..8 {
        let row = if flipped { 7 - r } else { r };
        let mut spans = vec![Span::styled(format!("{} ", 8 - row), t.muted())];
        for c in 0..8 {
            let col = if flipped { 7 - c } else { c };
            let i = row * 8 + col;
            let light = (row + col) % 2 == 0;
            let mut bg = if light { t.sq_light } else { t.sq_dark };
            if last.contains(&i) {
                bg = if v.awaiting_board { t.waiting } else { t.last };
            }
            let piece = sq[i];
            let fg = if piece.is_some_and(char::is_uppercase) { t.white_piece } else { t.black_piece };
            spans.push(Span::styled(
                format!(" {} ", piece.map_or(' ', glyph)),
                Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }
    let mut files = String::new();
    for c in 0..8u8 {
        let f = if flipped { (b'h' - c) as char } else { (b'a' + c) as char };
        files.push(' ');
        files.push(f);
        files.push(' ');
    }
    lines.push(Line::from(Span::styled(format!("  {files}"), t.muted())));
    lines
}

fn draw_board(f: &mut Frame, area: Rect, v: &View) {
    f.render_widget(Paragraph::new(board_lines(v)).block(panel(" board ")), area);
}

fn clock(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn draw_game(f: &mut Frame, area: Rect, v: &View, title: &str, scroll: usize) {
    let now = Instant::now();
    let t = theme::get();
    let mut lines =
        vec![Line::from(format!("White: {}    Black: {}", v.name_of(Side::White), v.name_of(Side::Black)))];
    if let Some([w, b]) = v.clocks_at(now) {
        let run = |s: Side| if v.running == Some(s) { "▶" } else { " " };
        lines.push(Line::from(format!(
            "{} {}   {} {}{}",
            run(Side::White),
            clock(w),
            run(Side::Black),
            clock(b),
            if v.paused { "  (paused)" } else { "" }
        )));
    }
    let over = !v.started() && v.status != chess_core::Status::Created;
    let style = if over {
        Style::default().add_modifier(Modifier::BOLD).fg(t.good)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    lines.push(Line::styled(v.status_line(), style));
    if over {
        if let Some(r) = &v.ratings {
            for p in r {
                lines.push(Line::from(format!("  {}", p.describe())));
            }
        }
        lines.push(Line::styled("  set the pieces up for another, or n to choose the next game", t.muted()));
    }
    if let Some(name) = v.opening_name() {
        lines.push(Line::styled(format!("Opening: {name}"), Style::default().fg(t.accent)));
    }
    lines.push(Line::from(""));
    let moves = v.moves_in_words();
    for (number, san, words) in moves.iter().rev().skip(scroll) {
        lines.push(Line::from(vec![
            Span::styled(format!("{number:>4} "), t.muted()),
            Span::styled(format!("{san:<8}"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(words.clone()),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(theme::brand_title(title))),
        area,
    );
}

fn draw_log(f: &mut Frame, area: Rect, scroll: usize, file: Option<&std::path::Path>, note: &str) {
    let height = usize::from(area.height.saturating_sub(2));
    let (lines, _) = log::newest(scroll + height);
    let t = theme::get();
    let items: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .map(|l| {
            let secs = l.at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let at = format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
            let colour = match l.source {
                Source::Board | Source::Clock => t.board,
                Source::Engine => t.engine,
                Source::Game => t.good,
                Source::Online => t.online,
                Source::Web | Source::LiveChess => t.web,
                Source::Device => t.muted,
            };
            Line::from(vec![
                Span::styled(format!("{at} "), t.muted()),
                Span::styled(format!("{:<9} ", l.source.label()), Style::default().fg(colour)),
                Span::raw(l.text.clone()),
            ])
        })
        .collect();
    let back = if scroll == 0 { String::new() } else { format!(", {scroll} back") };
    let saved = file.map(|p| format!(", also in {}", p.display())).unwrap_or_default();
    let title = format!(" log, newest first (UTC){back}{saved}{note} ");
    f.render_widget(Paragraph::new(items).wrap(Wrap { trim: false }).block(panel(title)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Controller, Setup};
    use crate::view::{EngineInfo, game_id};
    use chess_dgt::{Follower, Session};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view(moves: &[&str]) -> View {
        let c = Controller::new(Setup { time: Some("5+3".parse().unwrap()), ..Setup::default() });
        let mut s = Session::new(Follower::new());
        for m in moves {
            s.follower_mut().play(m).unwrap();
        }
        View::new(&c, &s, &EngineInfo { name: None }, game_id(1, 1), 0, Instant::now())
    }

    #[test]
    fn draws_the_game_and_the_log() {
        log::quiet(true);
        log::line(Source::LiveChess, "a page connected");
        let v = view(&["e2e4", "e7e5"]);
        let (games, _rx) = mpsc::channel();
        let mut app = App {
            state: State::Playing {
                board: Connected {
                    found: Found::Serial { path: "/dev/ttyACM0".into(), label: "USB".into() },
                    stop: Arc::new(AtomicBool::new(false)),
                    thread: None,
                    games,
                },
                game: Arc::new(AtomicBool::new(false)),
            },
            found: Arc::new(Mutex::new(Vec::new())),
            scanning: Arc::new(AtomicBool::new(false)),
            log_scroll: 0,
            moves_scroll: 0,
            pane: Pane::Log,
            title: "open-chess".into(),
            log_file: None,
            form: Form::new(&Defaults { level: 4, ..Defaults::default() }),
            looked: Arc::new(Mutex::new(None)),
            top: Rect::default(),
        };
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app, &v)).unwrap();
        let screen: String =
            t.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(screen.contains("King's Pawn Game (C20)"), "{screen}");
        assert!(screen.contains("1… e5"), "{screen}");
        assert!(screen.contains("5:00"));
        assert!(screen.contains("a page connected"));
        assert!(screen.contains('♚'));
        assert!(!screen.contains('♔'), "solid glyphs for both sides");
    }

    #[test]
    fn draws_the_game_picker() {
        let (games, _rx) = mpsc::channel();
        let mut app = App {
            state: State::Choosing {
                board: Connected {
                    found: Found::Serial { path: "/dev/ttyACM0".into(), label: "USB".into() },
                    stop: Arc::new(AtomicBool::new(false)),
                    thread: None,
                    games,
                },
            },
            found: Arc::new(Mutex::new(Vec::new())),
            scanning: Arc::new(AtomicBool::new(false)),
            log_scroll: 0,
            moves_scroll: 0,
            pane: Pane::Log,
            title: "open-chess".into(),
            log_file: None,
            form: Form::new(&Defaults::default()),
            looked: Arc::new(Mutex::new(None)),
            top: Rect::default(),
        };
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        t.draw(|f| draw(f, &mut app, &view(&[]))).unwrap();
        let screen: String =
            t.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        for want in ["choose a game", "Game type", "play the computer", "Level", "(rated 1400)", "Your name"]
        {
            assert!(screen.contains(want), "{want}: {screen}");
        }
    }

    #[test]
    fn the_board_picker_shows_the_logo() {
        let mut app = App {
            state: State::Picking { list: ListState::default() },
            found: Arc::new(Mutex::new(vec![Found::Serial {
                path: "/dev/ttyACM0".into(),
                label: "USB".into(),
            }])),
            scanning: Arc::new(AtomicBool::new(false)),
            log_scroll: 0,
            moves_scroll: 0,
            pane: Pane::Log,
            title: "open-chess".into(),
            log_file: None,
            form: Form::new(&Defaults::default()),
            looked: Arc::new(Mutex::new(None)),
            top: Rect::default(),
        };
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app, &view(&[]))).unwrap();
        let screen: String =
            t.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        for want in
            ["╭───╮●", "│ ♞ │  open-chess", "play chess on a real board", "♞ open-chess: choose a board"]
        {
            assert!(screen.contains(want), "{want}: {screen}");
        }
        if let Ok(path) = std::env::var("TUI_SNAPSHOT_PICKER") {
            std::fs::write(path, html(&t)).unwrap();
        }
    }

    /// A test terminal's screen as HTML, in its colours (to look at).
    fn html(t: &Terminal<TestBackend>) -> String {
        use std::fmt::Write as _;
        let colour = |c: ratatui::style::Color, fallback: &str| match c {
            ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => fallback.to_owned(),
        };
        let buf = t.backend().buffer();
        let mut out = String::from(
            "<html><body style='margin:0;background:#000'><pre style='font:14px/1.2 Menlo,monospace;margin:0'>",
        );
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                let bold = if c.modifier.contains(Modifier::BOLD) { "font-weight:bold;" } else { "" };
                let sym = match c.symbol() {
                    "<" => "&lt;",
                    ">" => "&gt;",
                    "&" => "&amp;",
                    s => s,
                };
                let _ = write!(
                    out,
                    "<span style='background:{};color:{};{bold}'>{sym}</span>",
                    colour(c.bg, "#06114a"),
                    colour(c.fg, "#ffffff")
                );
            }
            out.push('\n');
        }
        out + "</pre></body></html>"
    }

    /// Writes the playing screen, a finished game, to $TUI_SNAPSHOT.
    #[test]
    #[ignore = "writes a picture of the screen"]
    fn snapshot() {
        let Ok(path) = std::env::var("TUI_SNAPSHOT") else { return };
        log::quiet(true);
        log::line(Source::Board, "connected to Chessnut GO over Bluetooth");
        log::line(Source::Game, "f3: 1. f3");
        log::line(Source::Game, "classical rating: Ann 1500 → 1480 (-20) · Bob 1500 → 1520 (+20)");
        let mut v = view(&["f2f3", "e7e5", "g2g4", "d8h4"]);
        v.status = chess_core::Status::Mate;
        v.winner = Some(Side::Black);
        v.names = Some(["Ann".into(), "Bob".into()]);
        v.ratings = Some([
            crate::ratings::PlayerResult { name: "Ann".into(), rating: Some(1500), diff: Some(-20) },
            crate::ratings::PlayerResult { name: "Bob".into(), rating: Some(1500), diff: Some(20) },
        ]);
        let (games, _rx) = mpsc::channel();
        let mut app = App {
            state: State::Playing {
                board: Connected {
                    found: Found::Serial { path: "Chessnut GO".into(), label: "Bluetooth".into() },
                    stop: Arc::new(AtomicBool::new(false)),
                    thread: None,
                    games,
                },
                game: Arc::new(AtomicBool::new(false)),
            },
            found: Arc::new(Mutex::new(Vec::new())),
            scanning: Arc::new(AtomicBool::new(false)),
            log_scroll: 0,
            moves_scroll: 0,
            pane: Pane::Log,
            title: "open-chess board".into(),
            log_file: None,
            form: Form::new(&Defaults::default()),
            looked: Arc::new(Mutex::new(None)),
            top: Rect::default(),
        };
        let mut t = Terminal::new(TestBackend::new(120, 34)).unwrap();
        t.draw(|f| draw(f, &mut app, &v)).unwrap();
        let mut page = html(&t);
        let State::Playing { board, .. } = std::mem::replace(&mut app.state, picking()) else { return };
        app.state = State::Choosing { board };
        t.draw(|f| draw(f, &mut app, &v)).unwrap();
        page.push_str(&html(&t));
        std::fs::write(path, page).unwrap();
    }

    #[test]
    fn the_board_turns_for_black() {
        let mut v = view(&[]);
        let lines = board_lines(&v);
        assert!(lines[0].to_string().starts_with("8 "));
        v.player = Side::Black;
        assert!(board_lines(&v)[0].to_string().starts_with("1 "));
    }
}
