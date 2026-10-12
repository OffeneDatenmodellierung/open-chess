//! Lichess's speed classes, from a clock's initial time and increment.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Speed {
    UltraBullet,
    Bullet,
    Blitz,
    Rapid,
    Classical,
    Correspondence,
}

impl Speed {
    /// Lichess's rule: estimated game time = limit + 40 × increment
    /// (seconds), then < 30 ultraBullet, < 180 bullet, < 480 blitz,
    /// < 1500 rapid, otherwise classical.
    pub fn from_clock(limit_secs: u32, increment_secs: u32) -> Speed {
        let estimate = u64::from(limit_secs) + 40 * u64::from(increment_secs);
        match estimate {
            0..30 => Speed::UltraBullet,
            30..180 => Speed::Bullet,
            180..480 => Speed::Blitz,
            480..1500 => Speed::Rapid,
            _ => Speed::Classical,
        }
    }

    /// The API key (`blitz`) — also the rating pool ("perf") name.
    pub fn key(self) -> &'static str {
        match self {
            Speed::UltraBullet => "ultraBullet",
            Speed::Bullet => "bullet",
            Speed::Blitz => "blitz",
            Speed::Rapid => "rapid",
            Speed::Classical => "classical",
            Speed::Correspondence => "correspondence",
        }
    }

    /// The display name (`Blitz`, `UltraBullet`).
    pub fn display(self) -> &'static str {
        match self {
            Speed::UltraBullet => "UltraBullet",
            Speed::Bullet => "Bullet",
            Speed::Blitz => "Blitz",
            Speed::Rapid => "Rapid",
            Speed::Classical => "Classical",
            Speed::Correspondence => "Correspondence",
        }
    }

    pub fn from_key(key: &str) -> Option<Speed> {
        [
            Speed::UltraBullet,
            Speed::Bullet,
            Speed::Blitz,
            Speed::Rapid,
            Speed::Classical,
            Speed::Correspondence,
        ]
        .into_iter()
        .find(|s| s.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lichess_boundaries() {
        assert_eq!(Speed::from_clock(15, 0), Speed::UltraBullet);
        assert_eq!(Speed::from_clock(60, 0), Speed::Bullet);
        assert_eq!(Speed::from_clock(120, 1), Speed::Bullet); // 160
        assert_eq!(Speed::from_clock(180, 0), Speed::Blitz);
        assert_eq!(Speed::from_clock(180, 2), Speed::Blitz); // 260
        assert_eq!(Speed::from_clock(300, 3), Speed::Blitz); // 420
        assert_eq!(Speed::from_clock(480, 0), Speed::Rapid);
        assert_eq!(Speed::from_clock(600, 0), Speed::Rapid);
        assert_eq!(Speed::from_clock(900, 10), Speed::Rapid); // 1300
        assert_eq!(Speed::from_clock(1500, 0), Speed::Classical);
        assert_eq!(Speed::from_clock(1800, 20), Speed::Classical);
    }

    #[test]
    fn keys_round_trip() {
        for s in [Speed::Bullet, Speed::Blitz, Speed::Classical, Speed::UltraBullet] {
            assert_eq!(Speed::from_key(s.key()), Some(s));
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.key()));
        }
    }
}
