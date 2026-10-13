//! The DGT Centaur's e-paper screen, showing the game.
//!
//! A thread follows the game's [`View`] and redraws the screen when
//! something changes: with a quick partial refresh, and a full one now
//! and then to clear the ghosting e-paper builds up. Clocks are redrawn
//! every few seconds rather than every second, which e-paper can't keep
//! up with.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chess_centaur::epd::{Epd, Panel};
use chess_centaur::screen::{Shown, render};
use chess_core::Side;
use tokio::sync::watch;

use crate::view::View;

/// A full refresh after this many partial ones.
const FULL_EVERY: u32 = 30;
/// Redraw a running clock this often.
const CLOCKS: Duration = Duration::from_secs(10);

fn clock(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn square(name: &[u8]) -> Option<u8> {
    let (f, r) = (name.first()?.checked_sub(b'a')?, name.get(1)?.checked_sub(b'1')?);
    (f < 8 && r < 8).then_some(r * 8 + f)
}

/// What the screen shows for a view.
pub fn shown(v: &View, now: Instant) -> Shown {
    let marked = v
        .moves
        .last()
        .map(|m| [square(&m.as_bytes()[0..2.min(m.len())]), square(m.as_bytes().get(2..4).unwrap_or(&[]))])
        .map(|sq| sq.into_iter().flatten().collect())
        .unwrap_or_default();
    let side = if v.player == Side::White { "W" } else { "B" };
    let clocks = v.clocks_at(now).map(|[w, b]| {
        let run = |s: Side| if v.running == Some(s) { ">" } else { " " };
        format!("{}{} {}{}", run(Side::White), clock(w), run(Side::Black), clock(b))
    });
    Shown {
        fen: v.fen.clone(),
        flipped: v.player == Side::Black,
        marked,
        title: format!("You ({side}) v {}", v.opponent()),
        clocks,
        status: v.status_line(),
        moves: v.movetext(),
    }
}

/// Keep the screen showing the game, on its own thread.
pub fn spawn<P: Panel + Send + 'static>(
    panel: P,
    mut view: watch::Receiver<View>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("e-paper".into()).spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else { return };
        let mut epd = Epd::new(panel);
        epd.init();
        let mut last: Option<Shown> = None;
        let mut partials = 0u32;
        let mut clocks_at = Instant::now();
        loop {
            let now = Instant::now();
            let s = shown(&view.borrow_and_update(), now);
            let changed = last
                .as_ref()
                .is_none_or(|l| Shown { clocks: None, ..l.clone() } != Shown { clocks: None, ..s.clone() });
            let clocks_due = s.clocks != last.as_ref().and_then(|l| l.clocks.clone())
                && now.duration_since(clocks_at) >= CLOCKS;
            if changed || clocks_due {
                let bytes = render(&s).panel_bytes();
                if last.is_none() || partials >= FULL_EVERY {
                    epd.full(&bytes);
                    partials = 0;
                } else {
                    epd.partial(&bytes);
                    partials += 1;
                }
                clocks_at = now;
                last = Some(s);
            }
            // Wake on a change, or to redraw the clocks.
            let gone = rt.block_on(async {
                matches!(tokio::time::timeout(Duration::from_secs(1), view.changed()).await, Ok(Err(_)))
            });
            if gone {
                epd.sleep();
                return;
            }
        }
    })
}

/// The Centaur's own panel, on its Raspberry Pi.
#[cfg(target_os = "linux")]
pub fn start(view: watch::Receiver<View>) {
    use crate::log::{self, Source};
    match chess_centaur::epd::pi::Pi::open() {
        Ok(pi) => match spawn(pi, view) {
            Ok(_) => log::line(Source::Device, "e-paper screen on"),
            Err(e) => log::line(Source::Device, format!("e-paper screen: {e}")),
        },
        Err(e) => log::line(Source::Device, format!("no e-paper screen (SPI and GPIO): {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Controller, Setup};
    use crate::view::{EngineInfo, game_id};
    use chess_dgt::{Follower, Session};

    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl Panel for Recorder {
        fn reset(&mut self) {}
        fn command(&mut self, c: u8) {
            self.0.lock().unwrap().push(c);
        }
        fn data(&mut self, _: &[u8]) {}
        fn wait(&mut self, _: Duration) {}
    }

    #[test]
    fn redraws_when_the_game_changes() {
        let (tx, rx) = watch::channel(View::waiting("no board"));
        let rec = Recorder::default();
        let handle = spawn(rec.clone(), rx).unwrap();
        #[allow(clippy::naive_bytecount)]
        let refreshes = || rec.0.lock().unwrap().iter().filter(|&&c| c == 0x12).count();
        let until = Instant::now() + Duration::from_secs(5);
        while refreshes() < 1 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(refreshes(), 1, "a full refresh to start");
        let c = Controller::new(Setup::default());
        let mut s = Session::new(Follower::new());
        s.follower_mut().play("e2e4").unwrap();
        tx.send(View::new(&c, &s, &EngineInfo { name: None }, game_id(1, 1), 0, Instant::now())).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while refreshes() < 2 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(refreshes(), 2, "a partial refresh for the move");
        assert!(rec.0.lock().unwrap().contains(&0x91), "partial mode");
        drop(tx);
        handle.join().unwrap();
        assert!(rec.0.lock().unwrap().ends_with(&[0x50, 0x02, 0x07]), "asleep at the end");
    }

    #[test]
    fn shows_the_game() {
        let c = Controller::new(Setup { time: Some("5+3".parse().unwrap()), ..Setup::default() });
        let mut s = Session::new(Follower::new());
        s.follower_mut().play("e2e4").unwrap();
        let v = View::new(&c, &s, &EngineInfo { name: None }, game_id(1, 1), 0, Instant::now());
        let sh = shown(&v, Instant::now());
        assert_eq!(sh.marked, [12, 28]);
        assert_eq!(sh.title, "You (W) v open-chess level 4");
        assert_eq!(sh.status, "make e4 on the board");
        assert_eq!(sh.moves, "1. e4");
        assert_eq!(sh.clocks.as_deref(), Some(" 5:00  5:00"));
    }
}
