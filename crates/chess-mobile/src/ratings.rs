//! Glicko-2, as the server computes it, for the provisional ratings the
//! app shows between syncs. The server's figures replace them on the next
//! sync.

use chess_analysis::Level;
use chess_core::{GameScore, Rating};

/// A Glicko-2 rating.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct RatingValue {
    pub rating: f64,
    pub deviation: f64,
    pub volatility: f64,
}

impl From<Rating> for RatingValue {
    fn from(r: Rating) -> Self {
        RatingValue { rating: r.rating, deviation: r.deviation, volatility: r.volatility }
    }
}

impl From<RatingValue> for Rating {
    fn from(r: RatingValue) -> Self {
        Rating { rating: r.rating, deviation: r.deviation, volatility: r.volatility }
    }
}

/// How a game went for the player being rated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Score {
    Win,
    Draw,
    Loss,
}

/// A new player's rating: the starting point for someone with no history.
#[uniffi::export]
pub fn new_rating() -> RatingValue {
    Rating::default().into()
}

/// `player`'s rating after one game against `opponent`.
#[uniffi::export]
pub fn rating_after(player: RatingValue, opponent: RatingValue, score: Score) -> RatingValue {
    let score = match score {
        Score::Win => GameScore::Win,
        Score::Draw => GameScore::Draw,
        Score::Loss => GameScore::Loss,
    };
    Rating::from(player).after_game(&opponent.into(), score).into()
}

/// The bot's fixed rating at a level, as an opponent (it doesn't move, so
/// its deviation is the smallest there is).
#[uniffi::export]
pub fn bot_rating(level: u8) -> RatingValue {
    RatingValue {
        rating: f64::from(Level::new(level).rating()),
        deviation: Rating::MIN_DEVIATION,
        volatility: Rating::default().volatility,
    }
}

/// Still settling: shown with a "?" until the deviation is low enough.
#[uniffi::export]
pub fn is_provisional(rating: RatingValue) -> bool {
    Rating::from(rating).provisional()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_player_beating_a_bot_goes_up() {
        let me = new_rating();
        assert!(is_provisional(me));
        let after = rating_after(me, bot_rating(4), Score::Win);
        assert!(after.rating > me.rating);
        assert!(after.deviation < me.deviation);
        assert!((bot_rating(4).rating - 1400.0).abs() < f64::EPSILON);
        assert!(rating_after(me, bot_rating(4), Score::Loss).rating < me.rating);
    }
}
