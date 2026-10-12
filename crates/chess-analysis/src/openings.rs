//! Opening names by move order.
//!
//! A compact table of the main openings and defences with their ECO
//! codes, matched as the longest prefix of the game's moves. Transpositions
//! are not followed: a game reaching a Queen's Gambit by an unusual move
//! order is named by its actual moves. The table is data, so a fuller book
//! (e.g. the Lichess openings TSV, CC0) can replace it later.

use serde::Serialize;

/// A named opening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opening {
    pub eco: &'static str,
    pub name: &'static str,
    /// The moves that define it, in UCI.
    pub moves: Vec<&'static str>,
}

const BOOK: &[(&str, &str, &str)] = &[
    ("B00", "King's Pawn Opening", "e2e4"),
    ("C20", "King's Pawn Game", "e2e4 e7e5"),
    ("C40", "King's Knight Opening", "e2e4 e7e5 g1f3"),
    ("C44", "King's Knight Opening: Normal Variation", "e2e4 e7e5 g1f3 b8c6"),
    ("C60", "Ruy Lopez", "e2e4 e7e5 g1f3 b8c6 f1b5"),
    ("C70", "Ruy Lopez: Morphy Defense", "e2e4 e7e5 g1f3 b8c6 f1b5 a7a6"),
    ("C68", "Ruy Lopez: Exchange Variation", "e2e4 e7e5 g1f3 b8c6 f1b5 a7a6 b5c6"),
    ("C65", "Ruy Lopez: Berlin Defense", "e2e4 e7e5 g1f3 b8c6 f1b5 g8f6"),
    ("C50", "Italian Game", "e2e4 e7e5 g1f3 b8c6 f1c4"),
    ("C50", "Italian Game: Giuoco Piano", "e2e4 e7e5 g1f3 b8c6 f1c4 f8c5"),
    ("C51", "Italian Game: Evans Gambit", "e2e4 e7e5 g1f3 b8c6 f1c4 f8c5 b2b4"),
    ("C55", "Italian Game: Two Knights Defense", "e2e4 e7e5 g1f3 b8c6 f1c4 g8f6"),
    (
        "C57",
        "Italian Game: Two Knights Defense, Fried Liver Attack",
        "e2e4 e7e5 g1f3 b8c6 f1c4 g8f6 f3g5 d7d5 e4d5 f6d5 g5f7",
    ),
    ("C44", "Scotch Game", "e2e4 e7e5 g1f3 b8c6 d2d4"),
    ("C47", "Four Knights Game", "e2e4 e7e5 g1f3 b8c6 b1c3 g8f6"),
    ("C42", "Petrov's Defense", "e2e4 e7e5 g1f3 g8f6"),
    ("C41", "Philidor Defense", "e2e4 e7e5 g1f3 d7d6"),
    ("C30", "King's Gambit", "e2e4 e7e5 f2f4"),
    ("C33", "King's Gambit Accepted", "e2e4 e7e5 f2f4 e5f4"),
    ("C25", "Vienna Game", "e2e4 e7e5 b1c3"),
    ("C23", "Bishop's Opening", "e2e4 e7e5 f1c4"),
    ("C21", "Center Game", "e2e4 e7e5 d2d4 e5d4"),
    ("B20", "Sicilian Defense", "e2e4 c7c5"),
    ("B22", "Sicilian Defense: Alapin Variation", "e2e4 c7c5 c2c3"),
    ("B23", "Sicilian Defense: Closed", "e2e4 c7c5 b1c3"),
    ("B21", "Sicilian Defense: Smith-Morra Gambit", "e2e4 c7c5 d2d4 c5d4 c2c3"),
    ("B27", "Sicilian Defense", "e2e4 c7c5 g1f3"),
    ("B50", "Sicilian Defense", "e2e4 c7c5 g1f3 d7d6"),
    ("B54", "Sicilian Defense: Open", "e2e4 c7c5 g1f3 d7d6 d2d4 c5d4 f3d4"),
    ("B90", "Sicilian Defense: Najdorf Variation", "e2e4 c7c5 g1f3 d7d6 d2d4 c5d4 f3d4 g8f6 b1c3 a7a6"),
    ("B70", "Sicilian Defense: Dragon Variation", "e2e4 c7c5 g1f3 d7d6 d2d4 c5d4 f3d4 g8f6 b1c3 g7g6"),
    ("B30", "Sicilian Defense: Old Sicilian", "e2e4 c7c5 g1f3 b8c6"),
    ("B33", "Sicilian Defense: Sveshnikov Variation", "e2e4 c7c5 g1f3 b8c6 d2d4 c5d4 f3d4 g8f6 b1c3 e7e5"),
    ("B40", "Sicilian Defense: French Variation", "e2e4 c7c5 g1f3 e7e6"),
    ("C00", "French Defense", "e2e4 e7e6"),
    ("C01", "French Defense: Exchange Variation", "e2e4 e7e6 d2d4 d7d5 e4d5"),
    ("C02", "French Defense: Advance Variation", "e2e4 e7e6 d2d4 d7d5 e4e5"),
    ("C03", "French Defense: Tarrasch Variation", "e2e4 e7e6 d2d4 d7d5 b1d2"),
    ("C11", "French Defense: Classical Variation", "e2e4 e7e6 d2d4 d7d5 b1c3 g8f6"),
    ("C15", "French Defense: Winawer Variation", "e2e4 e7e6 d2d4 d7d5 b1c3 f8b4"),
    ("B10", "Caro-Kann Defense", "e2e4 c7c6"),
    ("B12", "Caro-Kann Defense: Advance Variation", "e2e4 c7c6 d2d4 d7d5 e4e5"),
    ("B13", "Caro-Kann Defense: Exchange Variation", "e2e4 c7c6 d2d4 d7d5 e4d5"),
    ("B01", "Scandinavian Defense", "e2e4 d7d5"),
    ("B02", "Alekhine Defense", "e2e4 g8f6"),
    ("B06", "Modern Defense", "e2e4 g7g6"),
    ("B07", "Pirc Defense", "e2e4 d7d6 d2d4 g8f6"),
    ("A40", "Queen's Pawn Opening", "d2d4"),
    ("D00", "Queen's Pawn Game", "d2d4 d7d5"),
    ("D00", "London System", "d2d4 d7d5 c1f4"),
    ("D06", "Queen's Gambit", "d2d4 d7d5 c2c4"),
    ("D20", "Queen's Gambit Accepted", "d2d4 d7d5 c2c4 d5c4"),
    ("D30", "Queen's Gambit Declined", "d2d4 d7d5 c2c4 e7e6"),
    ("D10", "Slav Defense", "d2d4 d7d5 c2c4 c7c6"),
    ("D43", "Semi-Slav Defense", "d2d4 d7d5 c2c4 c7c6 g1f3 g8f6 b1c3 e7e6"),
    ("D08", "Queen's Gambit Declined: Albin Countergambit", "d2d4 d7d5 c2c4 e7e5"),
    ("A45", "Indian Defense", "d2d4 g8f6"),
    ("A46", "London System", "d2d4 g8f6 c1f4"),
    ("E60", "King's Indian Defense", "d2d4 g8f6 c2c4 g7g6"),
    ("D80", "Grünfeld Defense", "d2d4 g8f6 c2c4 g7g6 b1c3 d7d5"),
    ("E20", "Nimzo-Indian Defense", "d2d4 g8f6 c2c4 e7e6 b1c3 f8b4"),
    ("E12", "Queen's Indian Defense", "d2d4 g8f6 c2c4 e7e6 g1f3 b7b6"),
    ("E00", "Catalan Opening", "d2d4 g8f6 c2c4 e7e6 g2g3"),
    ("A56", "Benoni Defense", "d2d4 g8f6 c2c4 c7c5"),
    ("A57", "Benko Gambit", "d2d4 g8f6 c2c4 c7c5 d4d5 b7b5"),
    ("A80", "Dutch Defense", "d2d4 f7f5"),
    ("A10", "English Opening", "c2c4"),
    ("A20", "English Opening: King's English", "c2c4 e7e5"),
    ("A30", "English Opening: Symmetrical Variation", "c2c4 c7c5"),
    ("A04", "Réti Opening", "g1f3"),
    ("A00", "Polish Opening", "b2b4"),
    ("A01", "Nimzo-Larsen Attack", "b2b3"),
    ("A02", "Bird's Opening", "f2f4"),
];

/// The most specific named opening the moves (standard UCI) start with.
pub fn opening<S: AsRef<str>>(moves: &[S]) -> Option<Opening> {
    BOOK.iter()
        .filter_map(|&(eco, name, line)| {
            let line: Vec<&'static str> = line.split(' ').collect();
            let matches = line.len() <= moves.len() && line.iter().zip(moves).all(|(a, b)| *a == b.as_ref());
            matches.then_some(Opening { eco, name, moves: line })
        })
        .max_by_key(|o| o.moves.len())
}

/// Every line in the book, e.g. to list in a training menu.
pub fn all() -> Vec<Opening> {
    BOOK.iter().map(|&(eco, name, line)| Opening { eco, name, moves: line.split(' ').collect() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let m = ["e2e4", "c7c5", "g1f3", "d7d6", "d2d4", "c5d4", "f3d4", "g8f6", "b1c3", "a7a6", "c1e3"];
        let o = opening(&m).unwrap();
        assert_eq!(o.name, "Sicilian Defense: Najdorf Variation");
        assert_eq!(o.eco, "B90");
        assert_eq!(
            opening(&["e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6"]).unwrap().name,
            "Ruy Lopez: Morphy Defense"
        );
        assert!(opening::<&str>(&[]).is_none());
        assert!(opening(&["h2h4"]).is_none());
    }

    #[test]
    fn every_book_line_is_legal() {
        for o in all() {
            let mut g = chess_core::Game::new(None).unwrap();
            for m in &o.moves {
                g.play_uci(m).unwrap_or_else(|e| panic!("{}: {m}: {e:?}", o.name));
            }
        }
    }
}
