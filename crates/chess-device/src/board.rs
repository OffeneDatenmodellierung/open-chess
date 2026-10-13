//! The board, whichever kind: what the controller needs of it.
//!
//! The controller plays the game and speaks to the board and clock in DGT
//! terms (`chess_dgt::Command`: clock texts, the clock's times, lights).
//! A DGT board's session takes those as they are; a Centaur turns them into
//! its own lights and sounds (it has no clock, and its screen is the app's
//! for now); a Chessnut board into its lights.

use chess_core::{Game, GameError, Played};
use chess_dgt::{ClockButton, Command, Session};

pub trait Board {
    fn game(&self) -> &Game;
    /// A move played elsewhere still has to be made on the board.
    fn awaiting_board(&self) -> bool;
    /// Play a move made elsewhere (the engine's).
    fn play(&mut self, uci: &str) -> Result<Played, GameError>;
    /// Tell the board or clock something.
    fn send(&mut self, c: Command);
    /// Follow the game from this position.
    fn set_position(&mut self, fen: &str) -> Result<(), GameError>;
}

impl Board for Session {
    fn game(&self) -> &Game {
        self.follower().game()
    }

    fn awaiting_board(&self) -> bool {
        self.follower().awaiting_board()
    }

    fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        self.follower_mut().play(uci)
    }

    fn send(&mut self, c: Command) {
        Session::send(self, c);
    }

    fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        self.follower_mut().set_position(fen)
    }
}

impl Board for chess_centaur::Session {
    fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        self.follower_mut().set_position(fen)
    }

    fn game(&self) -> &Game {
        self.follower().game()
    }

    fn awaiting_board(&self) -> bool {
        self.follower().awaiting_board()
    }

    fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        self.follower_mut().play(uci)
    }

    fn send(&mut self, c: Command) {
        match c {
            Command::Lights { from, to } => self.lights(from.0, to.0),
            Command::LightsOff => self.lights_off(),
            // A move or result to see: a sound to look up.
            Command::ClockText { beep, .. } if beep > 0 => self.beep(chess_centaur::Sound::General),
            Command::ClockBeep { .. } => self.beep(chess_centaur::Sound::General),
            _ => {}
        }
    }
}

impl Board for chess_chessnut::Session {
    fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        self.follower_mut().set_position(fen)
    }

    fn game(&self) -> &Game {
        self.follower().game()
    }

    fn awaiting_board(&self) -> bool {
        self.follower().awaiting_board()
    }

    fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        chess_chessnut::Session::play(self, uci)
    }

    fn send(&mut self, c: Command) {
        match c {
            Command::Lights { from, to } => self.lights(&[from.0, to.0]),
            Command::LightsOff => self.lights_off(),
            _ => {}
        }
    }
}

/// The Centaur's buttons as the DGT3000's, so they do the same things:
/// back shows the engine's move again, down and up change the level,
/// tick is play (pause, or switch sides), play opens the menu.
pub fn centaur_button(b: chess_centaur::Button) -> Option<ClockButton> {
    use chess_centaur::Button;
    Some(match b {
        Button::Back => ClockButton::Button(0),
        Button::Down => ClockButton::Button(1),
        Button::Tick => ClockButton::Button(2),
        Button::Up => ClockButton::Button(3),
        Button::Play => ClockButton::Button(4),
        Button::Help => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_centaur::{Address, Follower, protocol as p};
    use chess_dgt::Square;
    use std::time::Instant;

    #[test]
    fn a_centaur_lights_the_engines_move_and_beeps() {
        let mut s = chess_centaur::Session::new(Follower::new());
        s.start(Instant::now());
        let a = Address(1, 2);
        s.receive(&p::from_controller(0x87, a, &[0]), Instant::now());
        s.take_writes();
        Board::send(&mut s, Command::Lights { from: Square(12), to: Square(28) });
        Board::send(&mut s, Command::ClockText { text: "e4".into(), beep: 2 });
        Board::send(&mut s, Command::ClockText { text: "thinking".into(), beep: 0 });
        Board::send(&mut s, Command::LightsOff);
        assert_eq!(
            s.take_writes(),
            [p::leds_from_to(a, 12, 28, 5), p::beep(a, p::Sound::General), p::leds_off(a)]
        );
        assert_eq!(centaur_button(chess_centaur::Button::Up), Some(ClockButton::Button(3)));
    }
}
