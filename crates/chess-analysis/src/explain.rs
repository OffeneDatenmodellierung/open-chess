//! Chess notation decoded into plain English.
//!
//! [`explain_move`] turns a legal move into a sentence ("Knight from f3
//! takes the pawn on e5, giving check") and breaks its SAN into tokens
//! with their meaning, so a learner can see what each character of
//! "Nxe5+" stands for. [`explain_san`] does the same starting from SAN a
//! user typed, and [`glossary`] is the notation dictionary.

use chess_core::san;
use cozy_chess::{Board, Move, Piece};
use serde::Serialize;

use crate::util::{after, captured, is_castle, is_en_passant, legal, no_moves, piece_word, std_uci};

/// One piece of a SAN string and what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Token {
    pub text: String,
    pub meaning: String,
}

/// A move in every form a learner might meet it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveExplanation {
    /// Standard UCI, e.g. "g1f3" or "e1g1".
    pub uci: String,
    /// Standard Algebraic Notation, e.g. "Nf3".
    pub san: String,
    /// "knight", "pawn"…
    pub piece: String,
    pub from: String,
    pub to: String,
    pub captured: Option<String>,
    pub promotion: Option<String>,
    pub check: bool,
    pub checkmate: bool,
    pub castle: Option<String>,
    pub en_passant: bool,
    /// The whole move as a sentence.
    pub text: String,
    /// The SAN split into its parts.
    pub tokens: Vec<Token>,
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

/// The name of the piece a SAN letter stands for.
pub fn letter_name(c: char) -> Option<&'static str> {
    match c {
        'K' => Some("king"),
        'Q' => Some("queen"),
        'R' => Some("rook"),
        'B' => Some("bishop"),
        'N' => Some("knight"),
        _ => None,
    }
}

/// Explain a legal move on `board`.
pub fn explain_move(board: &Board, mv: Move) -> MoveExplanation {
    let san = san(board, mv);
    let piece = board.piece_on(mv.from).unwrap_or(Piece::Pawn);
    let next = after(board, mv);
    let check = !next.checkers().is_empty();
    let checkmate = check && no_moves(&next);
    let castle = is_castle(board, mv)
        .then(|| if mv.to.file() > mv.from.file() { "kingside" } else { "queenside" }.to_string());
    let en_passant = is_en_passant(board, mv);
    let taken = captured(board, mv);
    let uci = std_uci(board, mv);
    let to_sq = uci[2..4].to_string();

    let mut text = if let Some(side) = &castle {
        let (king_to, rook_from, rook_to) = match (side.as_str(), mv.from.rank() as u8) {
            ("kingside", r) => (format!("g{}", r + 1), format!("h{}", r + 1), format!("f{}", r + 1)),
            (_, r) => (format!("c{}", r + 1), format!("a{}", r + 1), format!("d{}", r + 1)),
        };
        format!(
            "Castles {side}: the king goes from {} to {king_to} and the rook from {rook_from} to {rook_to}",
            mv.from
        )
    } else {
        let who = cap(piece_word(piece));
        match taken {
            Some(_) if en_passant => {
                format!(
                    "{who} on {} takes the pawn on {}{} en passant, landing on {to_sq}",
                    mv.from,
                    mv.to.file(),
                    mv.from.rank()
                )
            }
            Some(t) => format!("{who} on {} takes the {} on {to_sq}", mv.from, piece_word(t)),
            None => format!("{who} from {} to {to_sq}", mv.from),
        }
    };
    if let Some(p) = mv.promotion {
        text = format!("{text} and promotes to a {}", piece_word(p));
    }
    if checkmate {
        text.push_str(", giving checkmate");
    } else if check {
        let double = next.checkers().len() > 1;
        text.push_str(if double { ", giving double check" } else { ", giving check" });
    }
    text.push('.');

    MoveExplanation {
        uci,
        tokens: tokenize(&san, piece, taken, castle.as_deref()),
        san,
        piece: piece_word(piece).into(),
        from: mv.from.to_string(),
        to: to_sq,
        captured: taken.map(|t| piece_word(t).into()),
        promotion: mv.promotion.map(|p| piece_word(p).into()),
        check,
        checkmate,
        castle,
        en_passant,
        text,
    }
}

/// Split SAN into its parts with what each one means.
fn tokenize(san: &str, piece: Piece, taken: Option<Piece>, castle: Option<&str>) -> Vec<Token> {
    let tok = |text: &str, meaning: String| Token { text: text.into(), meaning };
    let mut out = Vec::new();
    let body = san.trim_end_matches(['+', '#']);
    let suffix = &san[body.len()..];
    if let Some(side) = castle {
        out.push(tok(
            body,
            format!("castling {side} ({})", if side == "kingside" { "short" } else { "long" }),
        ));
    } else {
        let mut rest = body;
        if let Some(c) = rest.chars().next().filter(char::is_ascii_uppercase) {
            out.push(tok(&c.to_string(), format!("the {} moves", letter_name(c).unwrap_or("piece"))));
            rest = &rest[1..];
        } else {
            out.push(Token { text: String::new(), meaning: "no letter: a pawn moves".into() });
        }
        let (main, promo) = match rest.find('=') {
            Some(i) => (&rest[..i], Some(&rest[i..])),
            None => (rest, None),
        };
        let (pre, target) = main.split_at(main.len().saturating_sub(2));
        let (disamb, x) = match pre.strip_suffix('x') {
            Some(d) => (d, true),
            None => (pre, false),
        };
        if !disamb.is_empty() {
            let what = if piece == Piece::Pawn {
                format!("the pawn from the {disamb}-file")
            } else if disamb.len() == 2 {
                format!("the one on {disamb} (others could also reach the square)")
            } else if disamb.chars().all(|c| c.is_ascii_digit()) {
                format!("the one on rank {disamb} (others could also reach the square)")
            } else {
                format!("the one on the {disamb}-file (others could also reach the square)")
            };
            out.push(tok(disamb, what));
        }
        if x {
            let what =
                taken.map_or("captures".to_string(), |t| format!("captures (takes the {})", piece_word(t)));
            out.push(tok("x", what));
        }
        out.push(tok(target, format!("destination square {target}")));
        if let Some(p) = promo {
            let name = p.chars().nth(1).and_then(letter_name).unwrap_or("piece");
            out.push(tok(p, format!("promotes to a {name}")));
        }
    }
    match suffix {
        "+" => out.push(tok("+", "check".into())),
        "#" => out.push(tok("#", "checkmate".into())),
        _ => {}
    }
    out
}

/// Explain a move given in SAN (e.g. what a user typed). Accepts
/// annotations ("!", "?"), 0-0 for O-O and a missing "x", "+" or "#".
pub fn explain_san(board: &Board, input: &str) -> Option<MoveExplanation> {
    let norm = |s: &str| -> String {
        s.trim()
            .trim_end_matches(['!', '?'])
            .replace('0', "O")
            .chars()
            .filter(|c| !matches!(c, 'x' | '+' | '#' | '='))
            .collect()
    };
    let want = norm(input);
    if want.is_empty() {
        return None;
    }
    legal(board)
        .into_iter()
        .find(|&m| norm(&san(board, m)) == want || std_uci(board, m) == input.trim())
        .map(|m| explain_move(board, m))
}

/// The notation dictionary: symbol and meaning.
pub fn glossary() -> &'static [(&'static str, &'static str)] {
    &[
        ("K", "King"),
        ("Q", "Queen"),
        ("R", "Rook"),
        ("B", "Bishop"),
        ("N", "Knight (K is taken by the king)"),
        ("(no letter)", "Pawn: pawn moves are written with just the square, e.g. e4"),
        ("a–h", "Files: the columns, from White's left to right"),
        ("1–8", "Ranks: the rows, from White's side to Black's"),
        ("e4", "A square: file e, rank 4"),
        ("x", "Captures, e.g. Bxe5 = bishop takes on e5; exd5 = pawn on the e-file takes on d5"),
        ("+", "Check: the king is attacked"),
        ("++", "Double check (older notation; usually just +)"),
        ("#", "Checkmate: the king is attacked and cannot escape"),
        ("O-O", "Castling kingside (short)"),
        ("O-O-O", "Castling queenside (long)"),
        ("=Q", "Promotion: a pawn reaching the last rank becomes a queen (or =R, =B, =N)"),
        ("e.p.", "En passant: a pawn captures a pawn that just moved two squares past it"),
        ("Nbd2", "Disambiguation: the knight on the b-file goes to d2 (another knight could too)"),
        ("R1e2", "Disambiguation by rank: the rook on rank 1 goes to e2"),
        ("!", "A good move"),
        ("!!", "A brilliant move"),
        ("?", "A mistake"),
        ("??", "A blunder"),
        ("!?", "An interesting move"),
        ("?!", "A dubious move"),
        ("1-0", "White won"),
        ("0-1", "Black won"),
        ("½-½", "Draw"),
        ("1.", "Move number before White's move"),
        ("1...", "Move number before Black's move"),
        ("UCI e2e4", "Computer notation: from-square then to-square (e7e8q promotes)"),
        ("FEN", "A one-line description of a whole position"),
        ("PGN", "A whole game written out move by move, with headers"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(fen: &str, uci: &str) -> MoveExplanation {
        let b = Board::from_fen(fen, false).unwrap();
        let m = crate::util::parse_uci(&b, uci).unwrap();
        explain_move(&b, m)
    }

    #[test]
    fn simple_moves() {
        let e = ex(chess_core::STARTPOS_FEN, "g1f3");
        assert_eq!(e.san, "Nf3");
        assert_eq!(e.text, "Knight from g1 to f3.");
        assert_eq!(e.tokens[0].text, "N");
        let p = ex(chess_core::STARTPOS_FEN, "e2e4");
        assert_eq!(p.text, "Pawn from e2 to e4.");
    }

    #[test]
    fn captures_and_check() {
        let e = ex("4k3/8/8/4p3/8/5N2/8/4K2Q w - - 0 1", "f3e5");
        assert_eq!(e.text, "Knight on f3 takes the pawn on e5.");
        let c = ex("4k3/8/8/8/8/8/8/R3K3 w - - 0 1", "a1a8");
        assert_eq!(c.text, "Rook from a1 to a8, giving check.");
        assert!(c.tokens.iter().any(|t| t.text == "+" && t.meaning == "check"));
    }

    #[test]
    fn castling_mate_promotion_en_passant() {
        let c = ex("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1g1");
        assert_eq!(c.castle.as_deref(), Some("kingside"));
        assert!(
            c.text.starts_with("Castles kingside: the king goes from e1 to g1 and the rook from h1 to f1")
        );
        let m = ex("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", "a1a8");
        assert!(m.checkmate && m.text.ends_with("giving checkmate."));
        let p = ex("7k/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q");
        assert!(p.text.contains("promotes to a queen"));
        assert!(p.tokens.iter().any(|t| t.text == "=Q"));
        let ep = ex("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6");
        assert!(ep.en_passant);
        assert!(ep.text.contains("en passant, landing on d6"), "{}", ep.text);
    }

    #[test]
    fn disambiguation_token() {
        let e = ex("4k3/8/8/8/8/8/4K3/R6R w - - 0 1", "a1d1");
        assert_eq!(e.san, "Rad1");
        assert!(e.tokens.iter().any(|t| t.text == "a" && t.meaning.contains("a-file")));
    }

    #[test]
    fn san_input_is_forgiving() {
        let b = Board::default();
        assert_eq!(explain_san(&b, "Nf3!").unwrap().uci, "g1f3");
        assert_eq!(explain_san(&b, "e4").unwrap().uci, "e2e4");
        assert!(explain_san(&b, "Ke2").is_none());
        let c = Board::from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", false).unwrap();
        assert_eq!(explain_san(&c, "0-0").unwrap().uci, "e1g1");
        let x = Board::from_fen("4k3/8/8/4p3/8/5N2/8/4K3 w - - 0 1", false).unwrap();
        assert_eq!(explain_san(&x, "Ne5").unwrap().san, "Nxe5");
    }
}
