//! From board readings to moves.
//!
//! An e-board reports pieces, not moves: a player lifts a piece, puts it
//! down, takes a captured piece off, and the board reports each square as
//! it changes. Once the board has settled (the caller decides when: a
//! short pause with no field updates, then a fresh board dump), the
//! reading is compared with the game: one legal move leads to it, or it is
//! an earlier position (a takeback), or the starting position (a new game,
//! and which way round the board is), or nothing yet (a piece in the hand,
//! a capture half done).

use chess_core::Game;

use crate::board::{Orientation, Placement, STARTING_PLACEMENT};

/// How far back a takeback is recognised, in plies.
const TAKEBACK_PLIES: usize = 10;

/// What a settled reading means for the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The board shows the current position.
    Unchanged,
    /// A legal move, now played in the game (standard UCI and SAN).
    Move { uci: String, san: String },
    /// The board went back this many plies; the game has been taken back.
    Takeback { plies: usize },
    /// The starting position appeared: a new game has begun.
    NewGame,
    /// The board now shows the move the other side made (see
    /// [`Follower::play`]): it is in step with the game again.
    Caught,
    /// Not a position the game can reach in one step: a move in progress,
    /// a knocked-over piece, or a different position being set up.
    Waiting,
}

/// Follows a game on a physical board.
#[derive(Debug, Clone)]
pub struct Follower {
    game: Game,
    orientation: Orientation,
    /// A move was played for the other side (an engine, an online
    /// opponent) that the player still has to make on the board.
    awaiting_board: bool,
}

impl Default for Follower {
    fn default() -> Self {
        Follower::new()
    }
}

impl Follower {
    /// A new game from the starting position, the board the normal way round.
    pub fn new() -> Follower {
        Follower::from_game(Game::new(None).expect("the starting position is legal"))
    }

    pub fn from_game(game: Game) -> Follower {
        Follower { game, orientation: Orientation::Normal, awaiting_board: false }
    }

    pub fn game(&self) -> &Game {
        &self.game
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    pub fn set_orientation(&mut self, o: Orientation) {
        self.orientation = o;
    }

    /// Whether the board still has to show a move made with [`Follower::play`].
    pub fn awaiting_board(&self) -> bool {
        self.awaiting_board
    }

    /// The position the board should show, as it should read the current
    /// way round (what to light or display while waiting for it).
    pub fn expected(&self) -> Placement {
        placement(&self.game).oriented(self.orientation)
    }

    /// Play a move that didn't come from the board (the engine's, an online
    /// opponent's). Until the board shows it, readings are only checked
    /// against the new position.
    pub fn play(&mut self, uci: &str) -> Result<(), chess_core::GameError> {
        self.game.play_uci(uci)?;
        self.awaiting_board = true;
        Ok(())
    }

    /// Start again from a set-up position.
    pub fn set_position(&mut self, fen: &str) -> Result<(), chess_core::GameError> {
        self.game = Game::new(Some(fen))?;
        self.awaiting_board = false;
        Ok(())
    }

    /// What a settled reading, exactly as the board reported it, means.
    pub fn observe(&mut self, reading: &Placement) -> Observation {
        // The starting position, either way round, starts a game (and says
        // how the board is turned).
        let start = Placement::from_fen(STARTING_PLACEMENT).expect("valid");
        for o in [Orientation::Normal, Orientation::Rotated] {
            if *reading == start.oriented(o) {
                self.orientation = o;
                let fresh = self.game.ply() == 0 && self.game.initial_fen().is_none();
                if fresh && !self.awaiting_board {
                    return Observation::Unchanged;
                }
                self.game = Game::new(None).expect("the starting position is legal");
                self.awaiting_board = false;
                return Observation::NewGame;
            }
        }

        let seen = reading.oriented(self.orientation);
        let now = placement(&self.game);
        if seen == now {
            if self.awaiting_board {
                self.awaiting_board = false;
                return Observation::Caught;
            }
            return Observation::Unchanged;
        }
        if self.awaiting_board {
            return Observation::Waiting;
        }

        // One legal move from here.
        for uci in self.game.legal_moves() {
            let mut next = self.game.clone();
            let Ok(played) = next.play_uci(&uci) else { continue };
            if placement(&next) == seen {
                self.game = next;
                return Observation::Move { uci: played.uci, san: played.san };
            }
        }

        // An earlier position: a takeback.
        let fens = self.game.positions_and_moves();
        for plies in 1..=TAKEBACK_PLIES.min(fens.len()) {
            let (fen, _) = &fens[fens.len() - plies];
            if Placement::from_fen(fen).is_some_and(|p| p == seen) {
                self.game.undo(plies);
                return Observation::Takeback { plies };
            }
        }
        Observation::Waiting
    }
}

fn placement(game: &Game) -> Placement {
    Placement::from_fen(&game.fen()).expect("the game's FEN is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(moves: &str) -> Placement {
        let g = Game::from_moves(None, moves).unwrap();
        Placement::from_fen(&g.fen()).unwrap()
    }

    #[test]
    fn follows_moves_and_captures() {
        let mut f = Follower::new();
        assert_eq!(f.observe(&after("")), Observation::Unchanged);
        // Half a move (the pawn lifted): nothing yet.
        let mut lifted = after("");
        lifted.0[12] = None; // e2
        assert_eq!(f.observe(&lifted), Observation::Waiting);
        assert_eq!(f.observe(&after("e2e4")), Observation::Move { uci: "e2e4".into(), san: "e4".into() });
        f.observe(&after("e2e4 d7d5"));
        let o = f.observe(&after("e2e4 d7d5 e4d5"));
        assert_eq!(o, Observation::Move { uci: "e4d5".into(), san: "exd5".into() });
        assert_eq!(f.game().ply(), 3);
    }

    #[test]
    fn castling_is_standard_uci() {
        let moves = "e2e4 e7e5 g1f3 b8c6 f1c4 g8f6";
        let mut f = Follower::from_game(Game::from_moves(None, moves).unwrap());
        let o = f.observe(&after(&format!("{moves} e1g1")));
        assert_eq!(o, Observation::Move { uci: "e1g1".into(), san: "O-O".into() });
    }

    #[test]
    fn promotion_reads_the_piece_put_down() {
        let fen = "8/4P3/8/8/8/8/k7/4K3 w - - 0 1";
        let mut f = Follower::from_game(Game::new(Some(fen)).unwrap());
        let mut knight = Placement::from_fen(fen).unwrap();
        knight.0[52] = None; // e7
        knight.0[60] = crate::board::Piece::from_letter('N'); // e8
        assert_eq!(f.observe(&knight), Observation::Move { uci: "e7e8n".into(), san: "e8=N".into() });
    }

    #[test]
    fn takebacks() {
        let mut f = Follower::from_game(Game::from_moves(None, "e2e4 e7e5 g1f3").unwrap());
        assert_eq!(f.observe(&after("e2e4")), Observation::Takeback { plies: 2 });
        assert_eq!(f.game().uci_moves(), ["e2e4"]);
    }

    #[test]
    fn the_start_position_starts_a_game_and_sets_the_orientation() {
        let mut f = Follower::from_game(Game::from_moves(None, "e2e4").unwrap());
        let start = after("");
        assert_eq!(f.observe(&start.rotated()), Observation::NewGame);
        assert_eq!(f.orientation(), Orientation::Rotated);
        // Turned round, moves still read right.
        let o = f.observe(&after("d2d4").rotated());
        assert_eq!(o, Observation::Move { uci: "d2d4".into(), san: "d4".into() });
    }

    #[test]
    fn a_move_played_elsewhere_waits_for_the_board() {
        let mut f = Follower::from_game(Game::from_moves(None, "e2e4").unwrap());
        f.play("e7e5").unwrap();
        assert!(f.awaiting_board());
        // The board still shows 1.e4: not a takeback while we wait.
        assert_eq!(f.observe(&after("e2e4")), Observation::Waiting);
        assert_eq!(f.expected(), after("e2e4 e7e5"));
        assert_eq!(f.observe(&after("e2e4 e7e5")), Observation::Caught);
        assert_eq!(
            f.observe(&after("e2e4 e7e5 g1f3")),
            Observation::Move { uci: "g1f3".into(), san: "Nf3".into() }
        );
    }

    #[test]
    fn set_up_positions() {
        let mut f = Follower::new();
        f.set_position("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1").unwrap();
        let o = f.observe(&Placement::from_fen("4k3/8/8/8/4P3/8/8/4K3").unwrap());
        assert_eq!(o, Observation::Move { uci: "e2e4".into(), san: "e4".into() });
    }
}
