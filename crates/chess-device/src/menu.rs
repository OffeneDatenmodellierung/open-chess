//! The clock's menu, as PicoChess has it on a DGT3000: the right-hand
//! button opens it; minus and plus go through the choices, the right-hand
//! button again picks one, back goes up a level (and out at the top), and
//! play leaves it. The menu has the time control, the engine's level, the
//! side you play, the opening book and the engine. Without I/O: it says
//! what to show on the clock and what was picked.

use chess_core::Side;

use crate::controller::TimeControl;

/// The names of the books and engines the menu offers, and which ones are
/// in use. Engine 0 is open-chess's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    pub books: Vec<String>,
    pub book: Option<usize>,
    pub engines: Vec<String>,
    pub engine: usize,
}

/// What's set now, for the menu to start at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub time: Option<TimeControl>,
    pub level: u8,
    pub player: Side,
}

/// Something picked in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Time(Option<TimeControl>),
    Level(u8),
    Player(Side),
    /// A book by its number in [`Choices::books`], or no book.
    Book(Option<usize>),
    /// An engine by its number in [`Choices::engines`].
    Engine(usize),
}

/// What a button press in the menu does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Show this on the clock; the menu stays open.
    Show(String),
    /// A choice was made; the menu has closed.
    Picked(Picked),
    /// The menu has closed with nothing picked.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Time,
    Level,
    Side,
    Book,
    Engine,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Top,
    In(Item),
}

/// PicoChess's time controls: untimed, sudden death, then Fischer.
pub const TIMES: [Option<(u32, u32)>; 16] = [
    None,
    Some((1, 0)),
    Some((3, 0)),
    Some((5, 0)),
    Some((10, 0)),
    Some((15, 0)),
    Some((30, 0)),
    Some((60, 0)),
    Some((1, 1)),
    Some((3, 2)),
    Some((5, 3)),
    Some((10, 5)),
    Some((15, 10)),
    Some((30, 15)),
    Some((60, 30)),
    Some((90, 30)),
];

/// The open menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    page: Page,
    /// Where the top level is.
    top: usize,
    /// Where the open item is.
    at: usize,
    items: Vec<Item>,
    times: Vec<Option<TimeControl>>,
}

fn time_label(t: Option<TimeControl>) -> String {
    match t {
        None => "no time".into(),
        Some(t) if t.increment_secs == 0 => format!("{} min", t.minutes),
        Some(t) => t.to_string(),
    }
}

/// Up to eight characters (the DGT3000's display).
fn fit(s: &str) -> String {
    s.chars().take(8).collect()
}

impl Menu {
    /// Open the menu at its top; what to show.
    pub fn open(choices: &Choices, current: Current) -> (Menu, String) {
        let mut items = vec![Item::Time, Item::Level, Item::Side];
        if !choices.books.is_empty() {
            items.push(Item::Book);
        }
        if choices.engines.len() > 1 {
            items.push(Item::Engine);
        }
        items.push(Item::Version);
        let mut times: Vec<Option<TimeControl>> = TIMES
            .iter()
            .map(|t| t.map(|(minutes, increment_secs)| TimeControl { minutes, increment_secs }))
            .collect();
        if !times.contains(&current.time) {
            times.push(current.time);
        }
        let m = Menu { page: Page::Top, top: 0, at: 0, items, times };
        let shown = m.label(choices);
        (m, shown)
    }

    fn count(&self, item: Item, c: &Choices) -> usize {
        match item {
            Item::Time => self.times.len(),
            Item::Level => 8,
            Item::Side => 2,
            Item::Book => c.books.len() + 1,
            Item::Engine => c.engines.len(),
            Item::Version => 1,
        }
    }

    /// Where an item's choices start: at what's set now.
    fn start(&self, item: Item, c: &Choices, now: Current) -> usize {
        match item {
            Item::Time => self.times.iter().position(|t| *t == now.time).unwrap_or(0),
            Item::Level => usize::from(now.level.clamp(1, 8) - 1),
            Item::Side => usize::from(now.player == Side::Black),
            Item::Book => c.book.map_or(0, |b| b + 1),
            Item::Engine => c.engine,
            Item::Version => 0,
        }
    }

    fn label(&self, c: &Choices) -> String {
        match self.page {
            Page::Top => match self.items[self.top] {
                Item::Time => "time".into(),
                Item::Level => "level".into(),
                Item::Side => "side".into(),
                Item::Book => "book".into(),
                Item::Engine => "engine".into(),
                Item::Version => "version".into(),
            },
            Page::In(Item::Time) => time_label(self.times[self.at]),
            Page::In(Item::Level) => format!("level {}", self.at + 1),
            Page::In(Item::Side) => (if self.at == 0 { "white" } else { "black" }).into(),
            Page::In(Item::Book) => match self.at {
                0 => "no book".into(),
                n => fit(&c.books[n - 1]),
            },
            Page::In(Item::Engine) => fit(&c.engines[self.at]),
            Page::In(Item::Version) => fit(concat!("v", env!("CARGO_PKG_VERSION"))),
        }
    }

    /// A DGT3000 button (0 back, 1 minus, 2 play, 3 plus, 4 ok).
    pub fn press(&mut self, button: u8, c: &Choices, now: Current) -> Step {
        match (button, self.page) {
            (2, _) | (0, Page::Top) => return Step::Closed,
            (0, Page::In(_)) => self.page = Page::Top,
            (1 | 3, Page::Top) => self.top = step(self.top, self.items.len(), button == 3),
            (1 | 3, Page::In(item)) => self.at = step(self.at, self.count(item, c), button == 3),
            (4, Page::Top) => {
                let item = self.items[self.top];
                self.at = self.start(item, c, now);
                self.page = Page::In(item);
            }
            (4, Page::In(item)) => {
                return match item {
                    Item::Time => Step::Picked(Picked::Time(self.times[self.at])),
                    Item::Level => Step::Picked(Picked::Level(u8::try_from(self.at + 1).unwrap_or(8))),
                    Item::Side => {
                        Step::Picked(Picked::Player(if self.at == 0 { Side::White } else { Side::Black }))
                    }
                    Item::Book => Step::Picked(Picked::Book(self.at.checked_sub(1))),
                    Item::Engine => Step::Picked(Picked::Engine(self.at)),
                    Item::Version => {
                        self.page = Page::Top;
                        Step::Show(self.label(c))
                    }
                };
            }
            _ => {}
        }
        Step::Show(self.label(c))
    }
}

fn step(at: usize, n: usize, forward: bool) -> usize {
    if n == 0 {
        0
    } else if forward {
        (at + 1) % n
    } else {
        (at + n - 1) % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Current {
        Current { time: Some(TimeControl { minutes: 5, increment_secs: 3 }), level: 4, player: Side::White }
    }

    fn shows(s: Step) -> String {
        match s {
            Step::Show(t) => t,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn goes_through_the_items_and_back() {
        let c = Choices::default();
        let (mut m, shown) = Menu::open(&c, now());
        assert_eq!(shown, "time");
        assert_eq!(shows(m.press(3, &c, now())), "level");
        assert_eq!(shows(m.press(3, &c, now())), "side");
        assert_eq!(shows(m.press(3, &c, now())), "version", "no books or engines to choose");
        assert_eq!(shows(m.press(3, &c, now())), "time", "round again");
        assert_eq!(shows(m.press(1, &c, now())), "version");
        assert_eq!(m.press(0, &c, now()), Step::Closed);
    }

    #[test]
    fn picks_a_time_starting_at_the_current_one() {
        let c = Choices::default();
        let (mut m, _) = Menu::open(&c, now());
        assert_eq!(shows(m.press(4, &c, now())), "5+3");
        assert_eq!(shows(m.press(3, &c, now())), "10+5");
        assert_eq!(
            m.press(4, &c, now()),
            Step::Picked(Picked::Time(Some(TimeControl { minutes: 10, increment_secs: 5 })))
        );
        // Untimed is first: back from 1 minute.
        let cur = Current { time: Some(TimeControl { minutes: 1, increment_secs: 0 }), ..now() };
        let (mut m, _) = Menu::open(&c, cur);
        assert_eq!(shows(m.press(4, &c, cur)), "1 min");
        assert_eq!(shows(m.press(1, &c, cur)), "no time");
        assert_eq!(m.press(4, &c, cur), Step::Picked(Picked::Time(None)));
    }

    #[test]
    fn an_unlisted_time_is_offered_too() {
        let c = Choices::default();
        let odd = Some(TimeControl { minutes: 7, increment_secs: 2 });
        let cur = Current { time: odd, ..now() };
        let (mut m, _) = Menu::open(&c, cur);
        assert_eq!(shows(m.press(4, &c, cur)), "7+2");
    }

    #[test]
    fn levels_sides_books_and_engines() {
        let c = Choices {
            books: vec!["h-varied".into(), "gm2001-long-name".into()],
            book: Some(0),
            engines: vec!["open-chess".into(), "stockfish".into()],
            engine: 0,
        };
        let (mut m, _) = Menu::open(&c, now());
        m.press(3, &c, now());
        assert_eq!(shows(m.press(4, &c, now())), "level 4");
        assert_eq!(shows(m.press(3, &c, now())), "level 5");
        assert_eq!(m.press(4, &c, now()), Step::Picked(Picked::Level(5)));

        let (mut m, _) = Menu::open(&c, now());
        m.press(3, &c, now());
        m.press(3, &c, now());
        assert_eq!(shows(m.press(4, &c, now())), "white");
        assert_eq!(shows(m.press(3, &c, now())), "black");
        assert_eq!(m.press(4, &c, now()), Step::Picked(Picked::Player(Side::Black)));

        let (mut m, _) = Menu::open(&c, now());
        for _ in 0..3 {
            m.press(3, &c, now());
        }
        assert_eq!(shows(m.press(4, &c, now())), "h-varied");
        assert_eq!(shows(m.press(3, &c, now())), "gm2001-l");
        assert_eq!(shows(m.press(3, &c, now())), "no book");
        assert_eq!(m.press(4, &c, now()), Step::Picked(Picked::Book(None)));

        let (mut m, _) = Menu::open(&c, now());
        assert_eq!(shows(m.press(1, &c, now())), "version");
        assert_eq!(shows(m.press(1, &c, now())), "engine");
        assert_eq!(shows(m.press(4, &c, now())), "open-che");
        assert_eq!(shows(m.press(3, &c, now())), "stockfis");
        assert_eq!(shows(m.press(0, &c, now())), "engine", "back to the top");
        assert_eq!(m.press(2, &c, now()), Step::Closed, "play leaves");
    }
}
