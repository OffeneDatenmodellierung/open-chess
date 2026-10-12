//! The driver against the simulated DGT3000.

use std::time::{Duration, Instant};

use open_chess_dgtpi::driver::{Clock, ClockEvent, Dgt3000, Error, Report, Request, SendError};
use open_chess_dgtpi::protocol::{Run, buttons, cmd};
use open_chess_dgtpi::sim::SimClock;

#[test]
fn wakes_a_clock_that_is_off_and_takes_control() {
    let sim = SimClock::off();
    let mut clock = Dgt3000::new(sim.clone());
    clock.configure().unwrap();
    assert!(sim.is_on() && sim.in_mode_25());
    // Mode 25 unanswered (off) → wake → mode 25 refused → central control → mode 25.
    assert_eq!(
        sim.commands(),
        [cmd::CHANGE_STATE, cmd::PING, cmd::CHANGE_STATE, cmd::SET_CENTRAL_CONTROL, cmd::CHANGE_STATE]
    );
}

#[test]
fn shows_texts_and_sets_the_times() {
    let sim = SimClock::on();
    let mut clock = Dgt3000::new(sim.clone());
    clock.configure().unwrap();

    clock.set_text("e4", 2, 0, 0).unwrap();
    assert_eq!(sim.text().as_deref(), Some("e4"));
    // A second text replaces the first (the old one is ended first).
    clock.set_text("Nf3", 0, 0, 0).unwrap();
    assert_eq!(sim.text().as_deref(), Some("Nf3"));
    clock.end_text().unwrap();
    assert_eq!(sim.text(), None);
    // Ending when there's no text is fine too.
    clock.end_text().unwrap();

    clock.set_and_run((0, 5, 0), Run::Down, (0, 4, 37), Run::Stop).unwrap();
    assert_eq!(sim.times(), Some(((0, 5, 0), Run::Down, (0, 4, 37), Run::Stop)));
}

#[test]
fn commands_before_mode_25_are_refused() {
    let sim = SimClock::on();
    let mut clock = Dgt3000::new(sim);
    assert_eq!(clock.set_and_run((0, 1, 0), Run::Down, (0, 1, 0), Run::Stop), Err(Error::Refused));
}

#[test]
fn no_clock_is_an_error() {
    let mut clock = Dgt3000::new(SimClock::absent());
    assert_eq!(clock.configure(), Err(Error::Send(SendError::Silent)));
}

#[test]
fn reports_buttons_the_lever_and_times() {
    let sim = SimClock::on();
    let mut clock = Dgt3000::new(sim.clone());
    sim.buttons(buttons::ON);
    sim.buttons(buttons::ON | buttons::PLUS);
    sim.buttons(buttons::ON | buttons::PLUS | buttons::FORWARD);
    sim.buttons(buttons::ON);
    sim.buttons(buttons::ON | buttons::LEVER_RIGHT_DOWN);
    sim.report_time((0, 4, 59), (0, 5, 0), true);
    clock.poll();
    let events: Vec<_> = clock.events().collect();
    assert_eq!(
        events,
        [
            ClockEvent::PowerButton(true),
            ClockEvent::Buttons(buttons::PLUS),
            ClockEvent::Buttons(buttons::PLUS | buttons::FORWARD),
            ClockEvent::Lever(true),
            ClockEvent::Time { left: (0, 4, 59), right: (0, 5, 0) },
        ]
    );
}

#[test]
fn a_button_arriving_while_waiting_for_an_answer_is_kept() {
    let sim = SimClock::on();
    let mut clock = Dgt3000::new(sim.clone());
    clock.configure().unwrap();
    sim.buttons(buttons::BACK);
    clock.set_text("hello", 0, 0, 0).unwrap();
    clock.poll();
    assert!(clock.events().any(|e| e == ClockEvent::Buttons(buttons::BACK)));
}

#[test]
fn the_clock_thread_carries_out_requests() {
    let sim = SimClock::off();
    let clock = Clock::spawn(sim.clone()).unwrap();
    clock.send(Request::Text { text: "ready".into(), beep: 0, left_icons: 0, right_icons: 0 });
    clock.send(Request::SetAndRun {
        left: (0, 3, 0),
        left_run: Run::Stop,
        right: (0, 3, 0),
        right_run: Run::Down,
    });
    let until = Instant::now() + Duration::from_secs(5);
    let mut ready = false;
    while Instant::now() < until && sim.times().is_none() {
        ready |= clock.reports().any(|r| r == Report::Ready);
        std::thread::sleep(Duration::from_millis(5));
    }
    ready |= clock.reports().any(|r| r == Report::Ready);
    assert!(ready);
    assert_eq!(sim.text().as_deref(), Some("ready"));
    assert_eq!(sim.times(), Some(((0, 3, 0), Run::Stop, (0, 3, 0), Run::Down)));

    sim.buttons(buttons::ON | buttons::MINUS);
    let until = Instant::now() + Duration::from_secs(5);
    let mut pressed = false;
    while Instant::now() < until && !pressed {
        pressed = clock.reports().any(|r| r == Report::Event(ClockEvent::Buttons(buttons::MINUS)));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pressed);
}
