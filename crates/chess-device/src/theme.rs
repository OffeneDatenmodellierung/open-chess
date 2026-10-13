//! The terminal screen's colours: the site's look by default (royal navy
//! and orange, as the esports site and the Chess lab app), or the
//! terminal's own colours (`--theme terminal`).

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// The colours the screen uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// The background; `Reset` keeps the terminal's.
    pub paper: Color,
    pub ink: Color,
    pub muted: Color,
    pub rule: Color,
    /// Titles and the focused setting.
    pub action: Color,
    pub good: Color,
    pub danger: Color,
    pub accent: Color,
    pub sq_light: Color,
    pub sq_dark: Color,
    /// The last move's squares, and the engine's move waiting to be made.
    pub last: Color,
    pub waiting: Color,
    pub white_piece: Color,
    pub black_piece: Color,
    /// The log's sources.
    pub board: Color,
    pub engine: Color,
    pub online: Color,
    pub web: Color,
}

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The site's look (its dark theme).
pub const SITE: Theme = Theme {
    paper: hex(0x06_114a),
    ink: hex(0xff_ffff),
    muted: hex(0xa9_bdee),
    rule: hex(0x25_3f9c),
    action: hex(0xff_7a1a),
    good: hex(0x5e_e0a0),
    danger: hex(0xff_5577),
    accent: hex(0xff_c233),
    // The site's board blues, a shade deeper so white pieces stand out.
    sq_light: hex(0x8f_a6dd),
    sq_dark: hex(0x45_5fb5),
    last: hex(0xff_c233),
    waiting: hex(0xff_7a1a),
    white_piece: hex(0xff_ffff),
    black_piece: hex(0x02_0825),
    board: hex(0x7f_d4ff),
    engine: hex(0xd7_a6ff),
    online: hex(0xff_c233),
    web: hex(0xa9_bdee),
};

/// The terminal's own colours.
pub const TERMINAL: Theme = Theme {
    paper: Color::Reset,
    ink: Color::Reset,
    muted: Color::DarkGray,
    rule: Color::Reset,
    action: Color::Reset,
    good: Color::Green,
    danger: Color::Red,
    accent: Color::Cyan,
    sq_light: Color::Rgb(181, 136, 99),
    sq_dark: Color::Rgb(120, 82, 54),
    last: Color::Rgb(170, 162, 58),
    waiting: Color::Rgb(200, 90, 60),
    white_piece: Color::White,
    black_piece: Color::Black,
    board: Color::Cyan,
    engine: Color::Magenta,
    online: Color::Yellow,
    web: Color::Blue,
};

static CHOSEN: OnceLock<Theme> = OnceLock::new();

/// Choose the theme ("site" or "terminal"); the first choice stands.
pub fn choose(name: &str) {
    let _ = CHOSEN.set(if name.eq_ignore_ascii_case("terminal") { TERMINAL } else { SITE });
}

/// The theme in use (the site's unless chosen otherwise).
pub fn get() -> &'static Theme {
    CHOSEN.get().unwrap_or(&SITE)
}

impl Theme {
    pub fn base(&self) -> Style {
        Style::default().bg(self.paper).fg(self.ink)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.rule)
    }

    pub fn title(&self) -> Style {
        Style::default().fg(self.action).add_modifier(Modifier::BOLD)
    }
}

/// The open-chess logo for the terminal: a knight in an open ring with the
/// amber dot, three lines tall, then the name and `tagline`.
pub fn logo(tagline: &str) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::{Line, Span};
    let t = get();
    let ring = Style::default().fg(t.board);
    let dot = Style::default().fg(t.accent);
    let knight = Style::default().fg(t.ink).add_modifier(Modifier::BOLD);
    vec![
        Line::from(vec![Span::styled(" ╭───╮", ring), Span::styled("●", dot)]),
        Line::from(vec![
            Span::styled(" │ ", ring),
            Span::styled("♞", knight),
            Span::styled(" │  ", ring),
            Span::styled("open-chess", t.title()),
            Span::styled(format!("  {}", env!("CARGO_PKG_VERSION")), t.muted()),
        ]),
        Line::from(vec![Span::styled(" ╰───╯  ", ring), Span::styled(tagline.to_owned(), t.muted())]),
    ]
}

/// A panel title led by the logo's knight.
pub fn brand_title(text: &str) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    let t = get();
    Line::from(vec![
        Span::styled(" ♞", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(text.to_owned(), t.title()),
    ])
}
