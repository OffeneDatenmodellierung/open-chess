//! Fischer clocks, in milliseconds.
//!
//! The server measures each move's time itself (from when the previous
//! move was stored to when this one is), so the clock here is plain
//! arithmetic: subtract what was used, give back a little for network lag,
//! add the increment unless the player flagged.

use crate::game::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub initial_ms: i64,
    pub increment_ms: i64,
    /// Time left, white then black.
    pub remaining_ms: [i64; 2],
}

/// What one move did to the mover's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveClock {
    /// Milliseconds charged for the move (after lag compensation).
    pub used_ms: i64,
    /// The mover ran out of time before the move arrived.
    pub flagged: bool,
}

impl Clock {
    pub fn new(limit_secs: u32, increment_secs: u32) -> Clock {
        let initial = i64::from(limit_secs) * 1000;
        Clock {
            initial_ms: initial,
            increment_ms: i64::from(increment_secs) * 1000,
            remaining_ms: [initial; 2],
        }
    }

    pub fn remaining(&self, side: Side) -> i64 {
        self.remaining_ms[side.index()]
    }

    /// Charge `side` for a move that took `elapsed_ms` of server time.
    /// `lag_comp_ms` is given back (at most what was used) for the round
    /// trip. A player who was out of time when the move arrived flags; the
    /// clock then reads 0 and no increment is added.
    pub fn apply_move(&mut self, side: Side, elapsed_ms: i64, lag_comp_ms: i64) -> MoveClock {
        let elapsed = elapsed_ms.max(0);
        let used = (elapsed - lag_comp_ms.clamp(0, elapsed)).max(0);
        let left = &mut self.remaining_ms[side.index()];
        if used >= *left {
            *left = 0;
            return MoveClock { used_ms: used, flagged: true };
        }
        *left = *left - used + self.increment_ms;
        MoveClock { used_ms: used, flagged: false }
    }

    /// Time left for `side` if `elapsed_ms` have passed on its move (for
    /// streams and flag checks; changes nothing).
    pub fn live_remaining(&self, side: Side, elapsed_ms: i64) -> i64 {
        (self.remaining(side) - elapsed_ms.max(0)).max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_is_added_after_the_move() {
        let mut c = Clock::new(180, 2);
        let m = c.apply_move(Side::White, 5_000, 0);
        assert_eq!(m, MoveClock { used_ms: 5_000, flagged: false });
        assert_eq!(c.remaining(Side::White), 180_000 - 5_000 + 2_000);
        assert_eq!(c.remaining(Side::Black), 180_000);
    }

    #[test]
    fn lag_compensation_is_capped_by_time_used() {
        let mut c = Clock::new(60, 0);
        let m = c.apply_move(Side::Black, 50, 200);
        assert_eq!(m.used_ms, 0, "never gives back more than was used");
        assert_eq!(c.remaining(Side::Black), 60_000);
        let m = c.apply_move(Side::Black, 1_000, 200);
        assert_eq!(m.used_ms, 800);
    }

    #[test]
    fn flagging_zeroes_the_clock_without_increment() {
        let mut c = Clock::new(1, 5);
        let m = c.apply_move(Side::White, 1_000, 0);
        assert!(m.flagged);
        assert_eq!(c.remaining(Side::White), 0);
        assert_eq!(c.live_remaining(Side::Black, 400), 600);
        assert_eq!(c.live_remaining(Side::Black, 4_000), 0);
    }

    #[test]
    fn negative_elapsed_counts_as_zero() {
        let mut c = Clock::new(10, 0);
        assert_eq!(c.apply_move(Side::White, -5, 0).used_ms, 0);
    }
}
