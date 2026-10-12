//! Tactical and strategic motifs a move creates.
//!
//! [`detect`] looks at one legal move and reports the named patterns a
//! coach would point out: forks, pins, skewers, discovered and double
//! checks, back-rank and smothered mates, winning a hanging piece, leaving
//! a piece en prise, castling, promotion, en passant and basic opening
//! principles. These are static, one-move pattern checks, not a search:
//! they say what a move *does*, and the search says whether it is good.

use cozy_chess::{BitBoard, Board, Color, Move, Piece, Rank, Square};
use serde::Serialize;

use crate::util::{
    after, attackers_of, attacks_from, captured, color_name, is_castle, is_en_passant, no_moves, piece_word,
    value,
};

/// The kinds of pattern [`detect`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Motif {
    Checkmate,
    BackRankMate,
    SmotheredMate,
    Check,
    DoubleCheck,
    DiscoveredCheck,
    DiscoveredAttack,
    Fork,
    Pin,
    Skewer,
    WinsHangingPiece,
    Trade,
    HangsPiece,
    Castling,
    Promotion,
    Underpromotion,
    EnPassant,
    CentralPawn,
    Development,
    Fianchetto,
    EarlyQueen,
}

impl Motif {
    /// A short display name.
    pub fn name(self) -> &'static str {
        match self {
            Motif::Checkmate => "Checkmate",
            Motif::BackRankMate => "Back-rank mate",
            Motif::SmotheredMate => "Smothered mate",
            Motif::Check => "Check",
            Motif::DoubleCheck => "Double check",
            Motif::DiscoveredCheck => "Discovered check",
            Motif::DiscoveredAttack => "Discovered attack",
            Motif::Fork => "Fork",
            Motif::Pin => "Pin",
            Motif::Skewer => "Skewer",
            Motif::WinsHangingPiece => "Wins a hanging piece",
            Motif::Trade => "Trade",
            Motif::HangsPiece => "Leaves a piece en prise",
            Motif::Castling => "Castling",
            Motif::Promotion => "Promotion",
            Motif::Underpromotion => "Underpromotion",
            Motif::EnPassant => "En passant",
            Motif::CentralPawn => "Central pawn",
            Motif::Development => "Development",
            Motif::Fianchetto => "Fianchetto",
            Motif::EarlyQueen => "Early queen move",
        }
    }

    /// What the pattern is, in general.
    pub fn definition(self) -> &'static str {
        match self {
            Motif::Checkmate => "The king is in check and has no legal way out: the game is over.",
            Motif::BackRankMate => "A rook or queen mates a king trapped on its back rank by its own pawns.",
            Motif::SmotheredMate => "A knight mates a king hemmed in by its own pieces.",
            Motif::Check => "The move attacks the enemy king, which must respond.",
            Motif::DoubleCheck => "Two pieces give check at once; only a king move can answer it.",
            Motif::DiscoveredCheck => "Moving one piece uncovers a check from another behind it.",
            Motif::DiscoveredAttack => "Moving one piece uncovers an attack by another behind it.",
            Motif::Fork => "One piece attacks two or more targets at once, so not all can be saved.",
            Motif::Pin => "A piece can't move without exposing the king behind it.",
            Motif::Skewer => "A valuable piece is attacked and, when it moves, the piece behind it falls.",
            Motif::WinsHangingPiece => "Captures a piece nobody was defending.",
            Motif::Trade => "Exchanges pieces of equal value.",
            Motif::HangsPiece => "The moved piece can be taken without enough compensation.",
            Motif::Castling => "Tucks the king away and brings a rook towards the centre in one move.",
            Motif::Promotion => "A pawn reaches the last rank and becomes a new piece.",
            Motif::Underpromotion => {
                "Promotes to something other than a queen, usually to avoid stalemate or to fork."
            }
            Motif::EnPassant => "A pawn captures an enemy pawn that just jumped two squares alongside it.",
            Motif::CentralPawn => "Puts a pawn in the centre, claiming space and squares for the pieces.",
            Motif::Development => "Brings a minor piece off the back rank in the opening.",
            Motif::Fianchetto => {
                "Develops a bishop to the long diagonal next to the king's or queen's knight pawn."
            }
            Motif::EarlyQueen => "Brings the queen out early, where it can be chased with tempo.",
        }
    }
}

/// A pattern found in a move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pattern {
    pub motif: Motif,
    pub name: &'static str,
    /// This instance, e.g. "Knight on c7 forks the king on e8 and the rook on a8".
    pub detail: String,
    pub definition: &'static str,
}

fn pattern(motif: Motif, detail: String) -> Pattern {
    Pattern { motif, name: motif.name(), detail, definition: motif.definition() }
}

fn named(board: &Board, sq: Square) -> String {
    board.piece_on(sq).map_or_else(|| sq.to_string(), |p| format!("{} on {sq}", piece_word(p)))
}

/// Every pattern `mv` (legal on `board`) shows.
pub fn detect(board: &Board, mv: Move) -> Vec<Pattern> {
    let mut out = Vec::new();
    let us = board.side_to_move();
    let them = !us;
    let Some(piece) = board.piece_on(mv.from) else {
        return out;
    };
    let next = after(board, mv);
    let castle = is_castle(board, mv);
    // Where the moved piece ends up (castling: the king's real square).
    let to = if castle {
        let file = if mv.to.file() > mv.from.file() { cozy_chess::File::G } else { cozy_chess::File::C };
        Square::new(file, mv.from.rank())
    } else {
        mv.to
    };
    let moved = mv.promotion.unwrap_or(piece);
    let occ = next.occupied();
    let checkers = next.checkers();
    let mate = !checkers.is_empty() && no_moves(&next);
    let their_king = next.king(them);

    // --- Mates and checks.
    if mate {
        let checker = checkers.next_square();
        let checker_piece = checker.and_then(|s| next.piece_on(s));
        let back = match them {
            Color::White => Rank::First,
            Color::Black => Rank::Eighth,
        };
        if their_king.rank() == back
            && checkers.len() == 1
            && matches!(checker_piece, Some(Piece::Rook | Piece::Queen))
            && checker.is_some_and(|s| s.rank() == back)
        {
            out.push(pattern(
                Motif::BackRankMate,
                format!("The {} king is trapped on its back rank", color_name(them).to_lowercase()),
            ));
        } else if checker_piece == Some(Piece::Knight)
            && (cozy_chess::get_king_moves(their_king) & !next.colors(them)).is_empty()
        {
            out.push(pattern(Motif::SmotheredMate, "The king is surrounded by its own pieces".into()));
        }
        out.push(pattern(Motif::Checkmate, format!("{} wins", color_name(us))));
    }
    if checkers.len() > 1 {
        out.push(pattern(
            Motif::DoubleCheck,
            format!(
                "{} and {} both give check",
                named(&next, checkers.iter().next().unwrap_or(to)),
                named(&next, checkers.iter().nth(1).unwrap_or(to))
            ),
        ));
    }
    if !castle && let Some(disc) = checkers.iter().find(|&s| s != to) {
        out.push(pattern(
            Motif::DiscoveredCheck,
            format!("Moving the {} uncovers check from the {}", piece_word(piece), named(&next, disc)),
        ));
    }
    if !checkers.is_empty() && !mate && checkers.len() == 1 && out.is_empty() {
        out.push(pattern(Motif::Check, format!("The {} checks the king on {their_king}", piece_word(moved))));
    }

    // --- Material.
    let taken = captured(board, mv);
    if let Some(t) = taken {
        let defenders = attackers_of(board, mv.to, them, board.occupied() ^ mv.from.bitboard());
        if defenders.is_empty() && !is_en_passant(board, mv) {
            out.push(pattern(
                Motif::WinsHangingPiece,
                format!("The {} on {} was undefended", piece_word(t), mv.to),
            ));
        } else if value(t) == value(piece)
            || (matches!(t, Piece::Knight | Piece::Bishop) && matches!(piece, Piece::Knight | Piece::Bishop))
        {
            out.push(pattern(Motif::Trade, format!("{} for {}", cap(piece_word(piece)), piece_word(t))));
        }
    }
    if !mate && piece != Piece::King {
        let attackers = attackers_of(&next, to, them, occ);
        if !attackers.is_empty() {
            let defenders = attackers_of(&next, to, us, occ);
            let cheapest =
                attackers.iter().filter_map(|s| next.piece_on(s)).map(value).min().unwrap_or(i32::MAX);
            let gain = taken.map_or(0, value);
            let at_risk = value(moved);
            let loses = (defenders.is_empty() && at_risk > gain) || cheapest + gain < at_risk;
            // Captures with check are often fine; only flag clear hangs.
            if loses && checkers.is_empty() {
                out.push(pattern(
                    Motif::HangsPiece,
                    format!("The {} on {to} can be taken", piece_word(moved)),
                ));
            }
        }
    }

    // --- Forks: the moved piece attacks two or more worthwhile targets.
    if !mate {
        let targets: Vec<Square> = attacks_from(moved, us, to, occ)
            .iter()
            .filter(|&s| next.color_on(s) == Some(them))
            .filter(|&s| {
                let p = next.piece_on(s).unwrap_or(Piece::Pawn);
                p == Piece::King
                    || value(p) > value(moved)
                    || (p != Piece::Pawn && attackers_of(&next, s, them, occ).is_empty())
            })
            .collect();
        if targets.len() >= 2 {
            let names: Vec<String> = targets.iter().map(|&s| named(&next, s)).collect();
            out.push(pattern(
                Motif::Fork,
                format!("The {} on {to} attacks the {}", piece_word(moved), join(&names)),
            ));
        }
    }

    // --- Pins against the enemy king created by this move.
    let pinned_before = board.null_move().map_or(BitBoard::EMPTY, |b| b.pinned());
    let new_pins = next.pinned() & next.colors(them) & !pinned_before;
    for s in new_pins {
        out.push(pattern(Motif::Pin, format!("The {} is pinned to its king", named(&next, s))));
    }

    // --- Skewers along the moved slider's lines.
    if matches!(moved, Piece::Bishop | Piece::Rook | Piece::Queen) {
        for (df, dr) in directions(moved) {
            let mut seen = Vec::new();
            let mut sq = to;
            while let Some(n) = sq.try_offset(df, dr) {
                sq = n;
                if next.piece_on(n).is_some() {
                    seen.push(n);
                    if seen.len() == 2 {
                        break;
                    }
                }
            }
            if let [front, back] = seen[..]
                && next.color_on(front) == Some(them)
                && next.color_on(back) == Some(them)
            {
                let (fp, bp) =
                    (next.piece_on(front).unwrap_or(Piece::Pawn), next.piece_on(back).unwrap_or(Piece::Pawn));
                if matches!(fp, Piece::King | Piece::Queen | Piece::Rook)
                    && value(fp) > value(bp)
                    && bp != Piece::Pawn
                {
                    out.push(pattern(
                        Motif::Skewer,
                        format!("The {} must move, exposing the {}", named(&next, front), named(&next, back)),
                    ));
                }
            }
        }
    }

    // --- Discovered attacks (not check) on valuable pieces.
    if checkers.is_empty() && !castle {
        let before_occ = board.occupied();
        for slider in (next.colors(us)
            & (next.pieces(Piece::Bishop) | next.pieces(Piece::Rook) | next.pieces(Piece::Queen)))
        .iter()
        .filter(|&s| s != to)
        {
            let p = next.piece_on(slider).unwrap_or(Piece::Queen);
            let gained = attacks_from(p, us, slider, occ)
                & !attacks_from(p, us, slider, before_occ)
                & next.colors(them);
            if let Some(target) = gained.iter().find(|&s| {
                next.piece_on(s).is_some_and(|tp| tp != Piece::Pawn && value(tp) > value(p).min(500))
            }) {
                out.push(pattern(
                    Motif::DiscoveredAttack,
                    format!(
                        "Moving the {} lets the {} attack the {}",
                        piece_word(piece),
                        named(&next, slider),
                        named(&next, target)
                    ),
                ));
                break;
            }
        }
    }

    // --- Special moves.
    if castle {
        let side = if mv.to.file() > mv.from.file() { "kingside" } else { "queenside" };
        out.push(pattern(Motif::Castling, format!("{} castles {side}", color_name(us))));
    }
    if let Some(p) = mv.promotion {
        let m = if p == Piece::Queen { Motif::Promotion } else { Motif::Underpromotion };
        out.push(pattern(m, format!("The pawn becomes a {}", piece_word(p))));
    }
    if is_en_passant(board, mv) {
        out.push(pattern(
            Motif::EnPassant,
            format!("Takes the pawn that just passed on {}{}", mv.to.file(), mv.from.rank()),
        ));
    }

    // --- Opening principles (first dozen moves).
    if board.fullmove_number() <= 12 {
        let rel_from = mv.from.relative_to(us);
        let rel_to = to.relative_to(us);
        match piece {
            Piece::Pawn
                if matches!(rel_to, Square::D4 | Square::E4)
                    || (rel_to == Square::C4 && rel_from == Square::C2) =>
            {
                out.push(pattern(Motif::CentralPawn, format!("The pawn on {to} controls the centre")));
            }
            Piece::Knight | Piece::Bishop if rel_from.rank() == Rank::First => {
                if piece == Piece::Bishop && matches!(rel_to, Square::G2 | Square::B2) {
                    out.push(pattern(
                        Motif::Fianchetto,
                        format!("The bishop on {to} eyes the long diagonal"),
                    ));
                } else {
                    out.push(pattern(
                        Motif::Development,
                        format!("The {} comes off the back rank", piece_word(piece)),
                    ));
                }
            }
            Piece::Queen if board.fullmove_number() <= 6 && rel_from == Square::D1 && taken.is_none() => {
                out.push(pattern(Motif::EarlyQueen, "The queen comes out before the minor pieces".into()));
            }
            _ => {}
        }
    }
    out
}

fn directions(p: Piece) -> Vec<(i8, i8)> {
    let rook = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let bishop = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
    match p {
        Piece::Rook => rook.to_vec(),
        Piece::Bishop => bishop.to_vec(),
        _ => rook.iter().chain(bishop.iter()).copied().collect(),
    }
}

fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motifs(fen: &str, uci: &str) -> Vec<Motif> {
        let b = Board::from_fen(fen, false).unwrap();
        let m = crate::util::parse_uci(&b, uci).unwrap();
        detect(&b, m).into_iter().map(|p| p.motif).collect()
    }

    #[test]
    fn knight_fork_of_king_and_rook() {
        let m = motifs("r3k3/8/8/1N6/8/8/8/4K3 w - - 0 1", "b5c7");
        assert!(m.contains(&Motif::Fork), "{m:?}");
        assert!(m.contains(&Motif::Check), "{m:?}");
    }

    #[test]
    fn back_rank_mate() {
        let m = motifs("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", "a1a8");
        assert!(m.contains(&Motif::Checkmate) && m.contains(&Motif::BackRankMate), "{m:?}");
    }

    #[test]
    fn smothered_mate() {
        let m = motifs("6rk/6pp/8/6N1/8/8/8/6K1 w - - 0 1", "g5f7");
        assert!(m.contains(&Motif::SmotheredMate), "{m:?}");
    }

    #[test]
    fn pin_and_skewer() {
        // Bb5 pins the c6 knight to the e8 king.
        let pin = motifs("4k3/8/2n5/8/8/8/8/4KB2 w - - 0 1", "f1b5");
        assert!(pin.contains(&Motif::Pin), "{pin:?}");
        // Rook checks along the 8th: king must move, queen behind falls.
        let sk = motifs("q3k3/8/8/8/8/8/8/4K2R w - - 0 1", "h1h8");
        assert!(sk.contains(&Motif::Skewer), "{sk:?}");
    }

    #[test]
    fn discovered_check() {
        let m = motifs("4k3/8/8/8/4N3/8/8/4RK2 w - - 0 1", "e4c5");
        assert!(m.contains(&Motif::DiscoveredCheck), "{m:?}");
    }

    #[test]
    fn hanging_pieces_both_ways() {
        let win = motifs("4k3/8/8/3n4/8/8/8/3RK3 w - - 0 1", "d1d5");
        assert!(win.contains(&Motif::WinsHangingPiece), "{win:?}");
        let hang = motifs("4k3/8/3p4/8/8/8/8/2Q1K3 w - - 0 1", "c1c5");
        assert!(hang.contains(&Motif::HangsPiece), "{hang:?}");
    }

    #[test]
    fn opening_principles_and_specials() {
        let s = chess_core::STARTPOS_FEN;
        assert!(motifs(s, "e2e4").contains(&Motif::CentralPawn));
        assert!(motifs(s, "g1f3").contains(&Motif::Development));
        assert!(motifs("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1c1").contains(&Motif::Castling));
        assert!(motifs("7k/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8n").contains(&Motif::Underpromotion));
        assert!(motifs("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6").contains(&Motif::EnPassant));
    }

    #[test]
    fn quiet_move_has_no_tactics() {
        let m = motifs("4k3/8/8/8/8/8/P7/4K3 w - - 0 30", "a2a3");
        assert!(m.is_empty(), "{m:?}");
    }
}
