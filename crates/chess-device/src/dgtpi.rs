//! A DGT Pi's built-in clock in place of a clock on the board's cable.
//!
//! The controller speaks to "the clock" in board-cable commands
//! (`chess_dgt::Command::ClockText` and friends). On a DGT Pi the clock is
//! on the Pi's I²C pins instead (`open-chess-dgtpi`), so those commands
//! are turned into its requests, and its buttons into the same button
//! numbers a clock on the cable reports.

use chess_dgt::{ClockButton, Command, RunMode};
use chess_dgtpi::driver::{ClockEvent, Request};
use chess_dgtpi::protocol::{Run, buttons};

/// Whether a command is for the clock (rather than the board).
pub fn is_clock(c: &Command) -> bool {
    matches!(
        c,
        Command::ClockText { .. }
            | Command::ClockEndText
            | Command::ClockBeep { .. }
            | Command::ClockButtons
            | Command::ClockVersion
            | Command::ClockSetAndRun { .. }
    )
}

/// The DGT Pi clock's request for a clock command, if it has one.
pub fn request(c: &Command) -> Option<Request> {
    match c {
        Command::ClockText { text, beep } => Some(Request::Text {
            text: text.clone(),
            // On the cable: 62.5 ms plus beep/16 s; on the DGT Pi, units of 62.5 ms.
            beep: if *beep == 0 { 0 } else { beep.saturating_add(1) },
            left_icons: 0,
            right_icons: 0,
        }),
        Command::ClockBeep { units } => Some(Request::Text {
            text: String::new(),
            // 64 ms units, near enough 62.5.
            beep: *units,
            left_icons: 0,
            right_icons: 0,
        }),
        Command::ClockEndText => Some(Request::EndText),
        Command::ClockSetAndRun { left, right, mode, .. } => {
            let (left_run, right_run) = match mode {
                RunMode::Paused => (Run::Stop, Run::Stop),
                RunMode::LeftRunning => (Run::Down, Run::Stop),
                RunMode::RightRunning => (Run::Stop, Run::Down),
            };
            Some(Request::SetAndRun { left: *left, left_run, right: *right, right_run })
        }
        _ => None,
    }
}

/// The button a clock on the cable would report, for a DGT Pi clock
/// event.
pub fn button(e: ClockEvent) -> Option<ClockButton> {
    let ClockEvent::Buttons(held) = e else { return None };
    match held {
        buttons::BACK => Some(ClockButton::Button(0)),
        buttons::MINUS => Some(ClockButton::Button(1)),
        buttons::PLAY_PAUSE => Some(ClockButton::Button(2)),
        buttons::PLUS => Some(ClockButton::Button(3)),
        buttons::FORWARD => Some(ClockButton::Button(4)),
        b if b == buttons::BACK | buttons::FORWARD => Some(ClockButton::ZeroAndFour),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_commands_become_dgt_pi_requests() {
        let set = Command::ClockSetAndRun {
            left: (0, 5, 0),
            right: (0, 4, 30),
            mode: RunMode::RightRunning,
            lever_toggles: false,
        };
        assert!(is_clock(&set));
        assert_eq!(
            request(&set),
            Some(Request::SetAndRun {
                left: (0, 5, 0),
                left_run: Run::Stop,
                right: (0, 4, 30),
                right_run: Run::Down
            })
        );
        assert_eq!(
            request(&Command::ClockText { text: "e4".into(), beep: 2 }),
            Some(Request::Text { text: "e4".into(), beep: 3, left_icons: 0, right_icons: 0 })
        );
        assert_eq!(request(&Command::ClockEndText), Some(Request::EndText));
        assert!(!is_clock(&Command::LightsOff));
        assert!(!is_clock(&Command::SendBoard));
    }

    #[test]
    fn buttons_number_from_the_left() {
        assert_eq!(button(ClockEvent::Buttons(buttons::BACK)), Some(ClockButton::Button(0)));
        assert_eq!(button(ClockEvent::Buttons(buttons::PLUS)), Some(ClockButton::Button(3)));
        assert_eq!(
            button(ClockEvent::Buttons(buttons::BACK | buttons::FORWARD)),
            Some(ClockButton::ZeroAndFour)
        );
        assert_eq!(button(ClockEvent::Buttons(buttons::PLUS | buttons::MINUS)), None);
        assert_eq!(button(ClockEvent::Lever(true)), None);
    }
}
