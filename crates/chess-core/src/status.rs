//! Lichess's game status: the names and numeric ids its API uses
//! (`GameStatusName` and `GameStatusId` in the OpenAPI spec).

use serde::{Deserialize, Serialize};

/// Where a game stands. The numeric ids and names are Lichess's own, so
/// they go on the wire unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Created,
    Started,
    Aborted,
    Mate,
    Resign,
    Stalemate,
    /// The opponent left and the other side claimed the win.
    Timeout,
    Draw,
    Outoftime,
    Cheat,
    NoStart,
    UnknownFinish,
    InsufficientMaterialClaim,
    VariantEnd,
}

impl Status {
    pub const ALL: [Status; 14] = [
        Status::Created,
        Status::Started,
        Status::Aborted,
        Status::Mate,
        Status::Resign,
        Status::Stalemate,
        Status::Timeout,
        Status::Draw,
        Status::Outoftime,
        Status::Cheat,
        Status::NoStart,
        Status::UnknownFinish,
        Status::InsufficientMaterialClaim,
        Status::VariantEnd,
    ];

    /// Lichess's numeric id.
    pub fn id(self) -> i16 {
        match self {
            Status::Created => 10,
            Status::Started => 20,
            Status::Aborted => 25,
            Status::Mate => 30,
            Status::Resign => 31,
            Status::Stalemate => 32,
            Status::Timeout => 33,
            Status::Draw => 34,
            Status::Outoftime => 35,
            Status::Cheat => 36,
            Status::NoStart => 37,
            Status::UnknownFinish => 38,
            Status::InsufficientMaterialClaim => 39,
            Status::VariantEnd => 60,
        }
    }

    pub fn from_id(id: i16) -> Option<Status> {
        Status::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Lichess's name (`mate`, `outoftime`, `noStart`, …).
    pub fn name(self) -> &'static str {
        match self {
            Status::Created => "created",
            Status::Started => "started",
            Status::Aborted => "aborted",
            Status::Mate => "mate",
            Status::Resign => "resign",
            Status::Stalemate => "stalemate",
            Status::Timeout => "timeout",
            Status::Draw => "draw",
            Status::Outoftime => "outoftime",
            Status::Cheat => "cheat",
            Status::NoStart => "noStart",
            Status::UnknownFinish => "unknownFinish",
            Status::InsufficientMaterialClaim => "insufficientMaterialClaim",
            Status::VariantEnd => "variantEnd",
        }
    }

    pub fn from_name(name: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Still being played (or about to be)?
    pub fn is_ongoing(self) -> bool {
        matches!(self, Status::Created | Status::Started)
    }

    /// Over without a result counting (no rating change, no winner).
    pub fn is_void(self) -> bool {
        matches!(self, Status::Aborted | Status::NoStart | Status::UnknownFinish)
    }

    /// PGN `Termination` tag, as Lichess writes it.
    pub fn termination(self) -> &'static str {
        match self {
            Status::Created | Status::Started => "Unterminated",
            Status::Outoftime => "Time forfeit",
            Status::Timeout => "Abandoned",
            Status::Cheat => "Rules infraction",
            _ => "Normal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names_round_trip() {
        for s in Status::ALL {
            assert_eq!(Status::from_id(s.id()), Some(s));
            assert_eq!(Status::from_name(s.name()), Some(s));
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.name()), "serde name matches Lichess");
        }
        assert_eq!(Status::Mate.id(), 30);
        assert_eq!(Status::VariantEnd.id(), 60);
        assert_eq!(Status::from_id(99), None);
    }

    #[test]
    fn categories() {
        assert!(Status::Started.is_ongoing());
        assert!(!Status::Mate.is_ongoing());
        assert!(Status::Aborted.is_void());
        assert!(!Status::Resign.is_void());
        assert_eq!(Status::Outoftime.termination(), "Time forfeit");
        assert_eq!(Status::Mate.termination(), "Normal");
    }
}
