//! The Centaur's e-paper screen: what to draw, without I/O.
//!
//! The screen is 128 × 296 pixels, black on white, upright. The board fills
//! its width (16-pixel squares), with the players and clocks above it and
//! the game's state and latest moves below. [`Frame`] is the picture;
//! [`Frame::panel_bytes`] is it as the panel takes it (turned round, as the
//! screen is mounted). The piece pictures are open-chess's own.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_7X13_BOLD};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 296;
const BYTES: usize = WIDTH / 8 * HEIGHT;

/// A picture for the screen. `On` is black.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    /// 1 bits are white, rows of 16 bytes, top first.
    bits: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Frame")
    }
}

impl Default for Frame {
    fn default() -> Self {
        Frame { bits: vec![0xff; BYTES] }
    }
}

impl Frame {
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        x < WIDTH && y < HEIGHT && self.bits[(x + y * WIDTH) / 8] & (0x80 >> (x % 8)) == 0
    }

    pub fn set(&mut self, x: usize, y: usize, black: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let (i, bit) = ((x + y * WIDTH) / 8, 0x80 >> (x % 8));
        if black {
            self.bits[i] &= !bit;
        } else {
            self.bits[i] |= bit;
        }
    }

    /// The picture as the panel takes it: turned half round.
    pub fn panel_bytes(&self) -> Vec<u8> {
        let mut out = Frame::default();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if self.pixel(x, y) {
                    out.set(WIDTH - 1 - x, HEIGHT - 1 - y, true);
                }
            }
        }
        out.bits
    }

    fn text(&mut self, s: &str, x: i32, y: i32, bold: bool) {
        let font = if bold { &FONT_7X13_BOLD } else { &FONT_6X10 };
        let _ = Text::with_baseline(
            s,
            Point::new(x, y),
            MonoTextStyle::new(font, BinaryColor::On),
            Baseline::Top,
        )
        .draw(self);
    }
}

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Frame {
    type Color = BinaryColor;
    type Error = std::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if let (Ok(x), Ok(y)) = (usize::try_from(p.x), usize::try_from(p.y)) {
                self.set(x, y, c.is_on());
            }
        }
        Ok(())
    }
}

/// 12 × 12 pieces, `#` filled.
const PAWN: [&str; 12] = [
    "............",
    ".....##.....",
    "....####....",
    "....####....",
    ".....##.....",
    "....####....",
    ".....##.....",
    ".....##.....",
    "....####....",
    "...######...",
    "..########..",
    "............",
];
const ROOK: [&str; 12] = [
    "............",
    "..##.##.##..",
    "..########..",
    "...######...",
    "....####....",
    "....####....",
    "....####....",
    "....####....",
    "...######...",
    "..########..",
    "..########..",
    "............",
];
const KNIGHT: [&str; 12] = [
    "............",
    "....#.#.....",
    "...######...",
    "..#######...",
    "..##.####...",
    ".....####...",
    "....####....",
    "...#####....",
    "...######...",
    "..########..",
    "..########..",
    "............",
];
const BISHOP: [&str; 12] = [
    ".....##.....",
    "....#..#....",
    "....##.#....",
    "...##.###...",
    "...######...",
    "....####....",
    ".....##.....",
    "....####....",
    "...######...",
    "..########..",
    "..########..",
    "............",
];
const QUEEN: [&str; 12] = [
    ".#...##...#.",
    ".##.####.##.",
    "..########..",
    "..########..",
    "...######...",
    "....####....",
    "....####....",
    "...######...",
    "...######...",
    "..########..",
    "..########..",
    "............",
];
const KING: [&str; 12] = [
    ".....##.....",
    "....####....",
    ".....##.....",
    "..##.##.##..",
    ".##########.",
    ".##########.",
    "..########..",
    "...######...",
    "....####....",
    "...######...",
    "..########..",
    "............",
];

fn sprite(piece: char) -> Option<&'static [&'static str; 12]> {
    Some(match piece.to_ascii_lowercase() {
        'p' => &PAWN,
        'r' => &ROOK,
        'n' => &KNIGHT,
        'b' => &BISHOP,
        'q' => &QUEEN,
        'k' => &KING,
        _ => return None,
    })
}

/// Draw a piece at a square's top-left: a black piece filled, a white one
/// in outline, each with a white margin so it shows on a dark square.
fn draw_piece(f: &mut Frame, piece: char, x0: usize, y0: usize) {
    let Some(rows) = sprite(piece) else { return };
    // The sprite in a 14 × 14 grid with a one-pixel margin all round.
    let filled = |gx: usize, gy: usize| {
        (1..13).contains(&gx) && (1..13).contains(&gy) && rows[gy - 1].as_bytes()[gx - 1] == b'#'
    };
    let around = |gx: usize, gy: usize| {
        let (xs, ys) = (gx.saturating_sub(1)..=gx + 1, gy.saturating_sub(1)..=gy + 1);
        ys.flat_map(|y| xs.clone().map(move |x| (x, y))).collect::<Vec<_>>()
    };
    for gy in 0..14 {
        for gx in 0..14 {
            if !around(gx, gy).iter().any(|&(x, y)| filled(x, y)) {
                continue;
            }
            let inside = filled(gx, gy);
            let edge = inside && around(gx, gy).iter().any(|&(x, y)| !filled(x, y));
            let black = if piece.is_ascii_lowercase() { inside } else { edge };
            f.set(x0 + 1 + gx, y0 + 1 + gy, black);
        }
    }
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shown {
    /// The position (FEN; its placement is drawn).
    pub fen: String,
    /// Draw the board from Black's side.
    pub flipped: bool,
    /// The last move's squares (0 = a1), marked.
    pub marked: Vec<u8>,
    /// The top line (players, or the mode).
    pub title: String,
    /// The clocks line, if there's a clock.
    pub clocks: Option<String>,
    /// What's going on: "Your move", "Make e5 on the board", a result.
    pub status: String,
    /// The latest moves, numbered.
    pub moves: String,
}

/// Wrap text into lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Draw what's shown.
pub fn render(s: &Shown) -> Frame {
    let mut f = Frame::default();
    f.text(&s.title.chars().take(21).collect::<String>(), 1, 1, false);
    if let Some(c) = &s.clocks {
        f.text(&c.chars().take(18).collect::<String>(), 1, 13, true);
    }
    let top = 30usize;
    let placement = s.fen.split(' ').next().unwrap_or("");
    let mut board = [None; 64];
    for (row, rank) in placement.split('/').enumerate().take(8) {
        let mut file = 0usize;
        for c in rank.chars() {
            if let Some(n) = c.to_digit(10) {
                file += n as usize;
            } else if file < 8 {
                board[(7 - row) * 8 + file] = Some(c);
                file += 1;
            }
        }
    }
    for rank in 0..8usize {
        for file in 0..8usize {
            let (col, row) = if s.flipped { (7 - file, rank) } else { (file, 7 - rank) };
            let (x0, y0) = (col * 16, top + row * 16);
            let sq = (rank * 8 + file) as u8;
            let dark = (rank + file) % 2 == 0;
            let marked = s.marked.contains(&sq);
            for y in 0..16 {
                for x in 0..16 {
                    // Dark squares dotted; a marked square's edge drawn.
                    let dot = dark && x % 2 == 0 && y % 2 == 0;
                    let frame = marked && (x < 2 || y < 2 || x > 13 || y > 13);
                    f.set(x0 + x, y0 + y, dot || frame);
                }
            }
            if let Some(p) = board[usize::from(sq)] {
                draw_piece(&mut f, p, x0, y0);
            }
        }
    }
    let mut y = i32::try_from(top + 128 + 6).unwrap_or(164);
    for line in wrap(&s.status, 18).into_iter().take(3) {
        f.text(&line, 1, y, true);
        y += 14;
    }
    y += 4;
    for line in wrap(&s.moves, 21).into_iter().rev().take(9).collect::<Vec<_>>().into_iter().rev() {
        f.text(&line, 1, y, false);
        y += 11;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn draws_the_board_and_text() {
        let f = render(&Shown {
            fen: START.into(),
            title: "You vs open-chess L4".into(),
            clocks: Some("5:00   5:00".into()),
            status: "Your move".into(),
            moves: String::new(),
            ..Shown::default()
        });
        // a1 (bottom-left, dark, a white rook): its outline is black, its
        // middle white.
        let (x0, y0) = (0, 30 + 7 * 16);
        assert!(f.pixel(x0 + 2 + 2, y0 + 2 + 1), "the rook's top edge");
        assert!(!f.pixel(x0 + 2 + 5, y0 + 2 + 6), "the white rook's middle");
        // a8: a black rook, filled.
        assert!(f.pixel(2 + 5, 30 + 2 + 6));
        // An empty light square (b3) stays white.
        assert!(!(0..16).any(|d| f.pixel(16 + d, 30 + 5 * 16 + d)));
        // Text above the board.
        assert!((0..120).any(|x| (1..11).any(|y| f.pixel(x, y))));
    }

    #[test]
    fn the_panel_gets_it_turned_round() {
        let mut f = Frame::default();
        f.set(0, 0, true);
        let bytes = f.panel_bytes();
        assert_eq!(bytes.len(), 4736);
        assert_eq!(bytes[0], 0xff);
        assert_eq!(bytes[4735], 0xfe, "the top-left pixel ends bottom-right");
    }

    #[test]
    fn flipped_for_black() {
        let f = render(&Shown { fen: START.into(), flipped: true, ..Shown::default() });
        // From Black's side, h1's white rook is top-left: outline only.
        assert!(!f.pixel(2 + 5, 30 + 2 + 6));
        assert!(f.pixel(2 + 2, 30 + 2 + 1));
    }

    /// The picture as text, for looking at.
    pub fn ascii(f: &Frame, rows: std::ops::Range<usize>) -> String {
        rows.map(|y| (0..WIDTH).map(|x| if f.pixel(x, y) { '#' } else { '.' }).collect::<String>() + "\n")
            .collect()
    }

    #[test]
    #[ignore = "prints the screen"]
    fn show() {
        let f = render(&Shown {
            fen: START.into(),
            marked: vec![12, 28],
            title: "You vs open-chess L4".into(),
            clocks: Some("4:57  5:03".into()),
            status: "Make e5 on the board".into(),
            moves: "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6".into(),
            ..Shown::default()
        });
        println!("{}", ascii(&f, 0..240));
    }

    #[test]
    fn wraps_text() {
        assert_eq!(wrap("make Nf6 on the board now", 12), ["make Nf6 on", "the board", "now"]);
    }
}
