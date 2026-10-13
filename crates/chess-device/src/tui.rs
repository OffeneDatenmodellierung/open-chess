//! The terminal screen: pick a board, watch the game, read the log.
//!
//! Boards found nearby (Bluetooth LE) and attached (USB and serial ports,
//! Chessnut boards on USB) are listed to choose from; once one is
//! connected the screen shows the board, the game and its clocks, and the
//! log newest first: the board, the engine, the online game, and every
//! request from the app, PicoChess's tools and Lichess's DGT page.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use chess_core::{Game, Side, Status};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::watch;

use crate::discover::Found;
use crate::log::{self, Source};
use crate::view::View;

/// Starts playing on a board: returns the thread playing, which stops when
/// the flag is set (or the board fails).
pub type Connect = dyn Fn(Found, Arc<AtomicBool>) -> JoinHandle<io::Result<()>>;

/// Lists the boards there are (slow: it scans Bluetooth).
pub type Scan = dyn Fn() -> Vec<Found> + Send + Sync;

enum State {
    Picking { list: ListState },
    Playing { found: Found, stop: Arc<AtomicBool>, thread: Option<JoinHandle<io::Result<()>>> },
}

struct App {
    state: State,
    found: Arc<Mutex<Vec<Found>>>,
    scanning: Arc<AtomicBool>,
    log_scroll: usize,
    title: String,
}

/// Run the screen. `start` connects at once (a board named on the command
/// line); otherwise it starts by listing boards.
pub fn run(
    title: &str,
    view: &watch::Receiver<View>,
    scan: &Arc<Scan>,
    connect: &Connect,
    start: Option<Found>,
) -> io::Result<()> {
    log::quiet(true);
    let mut terminal = ratatui::init();
    let result = main_loop(&mut terminal, title, view, scan, connect, start);
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

fn main_loop(
    terminal: &mut DefaultTerminal,
    title: &str,
    view: &watch::Receiver<View>,
    scan: &Arc<Scan>,
    connect: &Connect,
    start: Option<Found>,
) -> io::Result<()> {
    let mut app = App {
        state: State::Picking { list: ListState::default().with_selected(Some(0)) },
        found: Arc::new(Mutex::new(Vec::new())),
        scanning: Arc::new(AtomicBool::new(false)),
        log_scroll: 0,
        title: title.to_owned(),
    };
    match start {
        Some(f) => {
            let stop = Arc::new(AtomicBool::new(false));
            let thread = Some(connect(f.clone(), stop.clone()));
            app.state = State::Playing { found: f, stop, thread };
        }
        None => rescan(&app, scan),
    }
    loop {
        // A board that stopped (failed, or unplugged): back to the list.
        if let State::Playing { thread, found, .. } = &mut app.state
            && thread.as_ref().is_some_and(JoinHandle::is_finished)
        {
            let what = found.describe();
            match thread.take().map(JoinHandle::join) {
                Some(Ok(Err(e))) => log::line(Source::Device, format!("{what}: {e}")),
                Some(Err(_)) => log::line(Source::Device, format!("{what}: stopped unexpectedly")),
                _ => log::line(Source::Device, format!("{what}: disconnected")),
            }
            app.state = State::Picking { list: ListState::default().with_selected(Some(0)) };
        }
        let v = view.borrow().clone();
        terminal.draw(|f| draw(f, &mut app, &v))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if let State::Playing { stop, .. } = &app.state {
                    stop.store(true, Ordering::SeqCst);
                }
                return Ok(());
            }
            KeyCode::PageUp => app.log_scroll = app.log_scroll.saturating_sub(10),
            KeyCode::PageDown => app.log_scroll += 10,
            KeyCode::Home => app.log_scroll = 0,
            _ => {}
        }
        let count = app.found.lock().map_or(0, |l| l.len());
        match &mut app.state {
            State::Picking { list } => match k.code {
                KeyCode::Up => list.select(Some(list.selected().unwrap_or(0).saturating_sub(1))),
                KeyCode::Down => {
                    list.select(Some((list.selected().unwrap_or(0) + 1).min(count.saturating_sub(1))));
                }
                KeyCode::Char('r') => rescan(&app, scan),
                KeyCode::Enter => {
                    let chosen = list.selected().and_then(|i| app.found.lock().ok()?.get(i).cloned());
                    if let Some(f) = chosen {
                        log::line(Source::Device, format!("connecting to {}", f.describe()));
                        let stop = Arc::new(AtomicBool::new(false));
                        let thread = Some(connect(f.clone(), stop.clone()));
                        app.state = State::Playing { found: f, stop, thread };
                    }
                }
                _ => {}
            },
            State::Playing { stop, .. } => {
                if k.code == KeyCode::Char('b') {
                    // Disconnect: the board loop stops and the list comes back.
                    stop.store(true, Ordering::SeqCst);
                }
            }
        }
    }
}

fn draw(f: &mut Frame, app: &mut App, v: &View) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(12), Constraint::Min(5), Constraint::Length(1)])
        .split(f.area());
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(30)])
        .split(rows[0]);
    match &mut app.state {
        State::Picking { list } => {
            draw_board(f, top[0], v);
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
            let widget = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("▶ ");
            f.render_stateful_widget(widget, top[1], list);
        }
        State::Playing { found, .. } => {
            draw_board(f, top[0], v);
            draw_game(f, top[1], v, &format!(" {}: {} ", app.title, found.describe()));
        }
    }
    draw_log(f, rows[1], app.log_scroll);
    let keys = match app.state {
        State::Picking { .. } => " ↑↓ choose · Enter connect · r look again · PgUp/PgDn log · q quit",
        State::Playing { .. } => " b back to the boards · PgUp/PgDn log · Home newest · q quit",
    };
    f.render_widget(Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)), rows[2]);
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

fn glyph(c: char) -> char {
    match c {
        'K' => '♔',
        'Q' => '♕',
        'R' => '♖',
        'B' => '♗',
        'N' => '♘',
        'P' => '♙',
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
    let mut lines = Vec::new();
    for r in 0..8 {
        let row = if flipped { 7 - r } else { r };
        let mut spans = vec![Span::styled(format!("{} ", 8 - row), Style::default().fg(Color::DarkGray))];
        for c in 0..8 {
            let col = if flipped { 7 - c } else { c };
            let i = row * 8 + col;
            let light = (row + col) % 2 == 0;
            let mut bg = if light { Color::Rgb(181, 136, 99) } else { Color::Rgb(120, 82, 54) };
            if last.contains(&i) {
                bg = if v.awaiting_board { Color::Rgb(200, 90, 60) } else { Color::Rgb(170, 162, 58) };
            }
            let piece = sq[i];
            let fg = if piece.is_some_and(char::is_uppercase) { Color::White } else { Color::Black };
            spans.push(Span::styled(
                format!(" {} ", piece.map_or(' ', glyph)),
                Style::default().bg(bg).fg(fg),
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
    lines.push(Line::from(Span::styled(format!("  {files}"), Style::default().fg(Color::DarkGray))));
    lines
}

fn draw_board(f: &mut Frame, area: Rect, v: &View) {
    f.render_widget(
        Paragraph::new(board_lines(v)).block(Block::default().borders(Borders::ALL).title(" board ")),
        area,
    );
}

fn clock(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn draw_game(f: &mut Frame, area: Rect, v: &View, title: &str) {
    let now = Instant::now();
    let opponent = v.engine.name.clone().unwrap_or_else(|| format!("open-chess level {}", v.level));
    let (white, black) =
        if v.player == Side::White { ("you", opponent.as_str()) } else { (opponent.as_str(), "you") };
    let mut lines = vec![Line::from(format!("White: {white}    Black: {black}"))];
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
    let status = match v.status {
        Status::Created => "waiting for a board".to_owned(),
        Status::Started if v.awaiting_board => format!("make {} on the board", last_san(v)),
        Status::Started if v.thinking => "the engine is thinking…".to_owned(),
        Status::Started => format!("{} to move", if v.turn == Side::White { "White" } else { "Black" }),
        s => format!(
            "game over: {} ({})",
            match v.winner {
                Some(Side::White) => "1-0",
                Some(Side::Black) => "0-1",
                None => "½-½",
            },
            s.name()
        ),
    };
    lines.push(Line::styled(status, Style::default().add_modifier(Modifier::BOLD)));
    lines.push(Line::from(""));
    lines.push(Line::from(movetext(v)));
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(title.to_owned())),
        area,
    );
}

fn sans(v: &View) -> Vec<String> {
    Game::from_moves(v.initial_fen.as_deref(), &v.moves.join(" "))
        .map(|g| g.sans().to_vec())
        .unwrap_or_default()
}

fn last_san(v: &View) -> String {
    sans(v).last().cloned().unwrap_or_default()
}

/// The moves, numbered, the latest last.
fn movetext(v: &View) -> String {
    let mut out = String::new();
    for (i, s) in sans(v).iter().enumerate() {
        if i % 2 == 0 {
            out.push_str(&(i / 2 + 1).to_string());
            out.push_str(". ");
        }
        out.push_str(s);
        out.push(' ');
    }
    out
}

fn draw_log(f: &mut Frame, area: Rect, scroll: usize) {
    let height = usize::from(area.height.saturating_sub(2));
    let (lines, _) = log::newest(scroll + height);
    let items: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .map(|l| {
            let secs = l.at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let t = format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
            let colour = match l.source {
                Source::Board => Color::Cyan,
                Source::Engine => Color::Magenta,
                Source::Game => Color::Green,
                Source::Online => Color::Yellow,
                Source::Web | Source::LiveChess => Color::Blue,
                Source::Clock => Color::LightCyan,
                Source::Device => Color::Gray,
            };
            Line::from(vec![
                Span::styled(format!("{t} "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<9} ", l.source.label()), Style::default().fg(colour)),
                Span::raw(l.text.clone()),
            ])
        })
        .collect();
    let title = if scroll == 0 {
        " log, newest first (UTC) ".to_owned()
    } else {
        format!(" log, newest first (UTC), {scroll} back ")
    };
    f.render_widget(Paragraph::new(items).block(Block::default().borders(Borders::ALL).title(title)), area);
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
        let mut app = App {
            state: State::Playing {
                found: Found::Serial { path: "/dev/ttyACM0".into(), label: "USB".into() },
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
            found: Arc::new(Mutex::new(Vec::new())),
            scanning: Arc::new(AtomicBool::new(false)),
            log_scroll: 0,
            title: "open-chess".into(),
        };
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app, &v)).unwrap();
        let screen: String =
            t.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(screen.contains("1. e4 e5"), "{screen}");
        assert!(screen.contains("5:00"));
        assert!(screen.contains("a page connected"));
        assert!(screen.contains('♚'));
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
