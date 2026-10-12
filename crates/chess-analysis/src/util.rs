//! Small board helpers shared by the modules.

use chess_core::Game;
use cozy_chess::{BitBoard, Board, Color, File, Move, Piece, Square};

/// Material value in centipawns, used for move ordering and motifs.
pub(crate) fn value(p: Piece) -> i32 {
    match p {
        Piece::Pawn => 100,
        Piece::Knight => 320,
        Piece::Bishop => 330,
        Piece::Rook => 500,
        Piece::Queen => 900,
        Piece::King => 20_000,
    }
}

pub(crate) fn piece_word(p: Piece) -> &'static str {
    match p {
        Piece::Pawn => "pawn",
        Piece::Knight => "knight",
        Piece::Bishop => "bishop",
        Piece::Rook => "rook",
        Piece::Queen => "queen",
        Piece::King => "king",
    }
}

/// True if `mv` is castling (cozy-chess encodes it as king-takes-rook).
pub(crate) fn is_castle(board: &Board, mv: Move) -> bool {
    board.piece_on(mv.from) == Some(Piece::King) && board.color_on(mv.to) == Some(board.side_to_move())
}

/// True if `mv` is an en-passant capture.
pub(crate) fn is_en_passant(board: &Board, mv: Move) -> bool {
    board.piece_on(mv.from) == Some(Piece::Pawn)
        && mv.from.file() != mv.to.file()
        && board.piece_on(mv.to).is_none()
}

/// The piece `mv` captures, if any (en passant counts as a pawn).
pub(crate) fn captured(board: &Board, mv: Move) -> Option<Piece> {
    if is_castle(board, mv) {
        return None;
    }
    if is_en_passant(board, mv) {
        return Some(Piece::Pawn);
    }
    board.piece_on(mv.to)
}

/// All legal moves.
pub(crate) fn legal(board: &Board) -> Vec<Move> {
    let mut out = Vec::with_capacity(48);
    board.generate_moves(|pm| {
        out.extend(pm);
        false
    });
    out
}

/// UCI in the standard notation (castling as e1g1, not cozy-chess's e1h1),
/// the form Lichess and `chess_core::Game::play_uci` use.
pub fn std_uci(board: &Board, mv: Move) -> String {
    if is_castle(board, mv) {
        let file = if mv.to.file() > mv.from.file() { File::G } else { File::C };
        return format!("{}{}", mv.from, Square::new(file, mv.from.rank()));
    }
    mv.to_string()
}

/// A legal move from standard UCI (`e1g1` or `e1h1` both castle).
pub fn parse_uci(board: &Board, uci: &str) -> Option<Move> {
    let uci = uci.trim();
    legal(board).into_iter().find(|&m| std_uci(board, m) == uci || m.to_string() == uci)
}

/// Zobrist hashes of every position in the game before the current one,
/// for repetition detection in search.
pub fn history_hashes(game: &Game) -> Vec<u64> {
    let Ok(mut replay) = Game::new(game.initial_fen()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(game.ply());
    for uci in game.uci_moves() {
        out.push(replay.board().hash());
        if replay.play_uci(&uci).is_err() {
            break;
        }
    }
    out
}

/// Every square attacked by `color` with the given occupancy.
pub(crate) fn attackers_of(board: &Board, sq: Square, color: Color, occupied: BitBoard) -> BitBoard {
    let theirs = board.colors(color) & occupied;
    let rooks = board.pieces(Piece::Rook) | board.pieces(Piece::Queen);
    let bishops = board.pieces(Piece::Bishop) | board.pieces(Piece::Queen);
    let mut a = BitBoard::EMPTY;
    a |= cozy_chess::get_knight_moves(sq) & board.pieces(Piece::Knight);
    a |= cozy_chess::get_king_moves(sq) & board.pieces(Piece::King);
    a |= cozy_chess::get_pawn_attacks(sq, !color) & board.pieces(Piece::Pawn);
    a |= cozy_chess::get_rook_moves(sq, occupied) & rooks;
    a |= cozy_chess::get_bishop_moves(sq, occupied) & bishops;
    a & theirs
}

/// The squares a piece on `sq` attacks.
pub(crate) fn attacks_from(piece: Piece, color: Color, sq: Square, occupied: BitBoard) -> BitBoard {
    match piece {
        Piece::Pawn => cozy_chess::get_pawn_attacks(sq, color),
        Piece::Knight => cozy_chess::get_knight_moves(sq),
        Piece::Bishop => cozy_chess::get_bishop_moves(sq, occupied),
        Piece::Rook => cozy_chess::get_rook_moves(sq, occupied),
        Piece::Queen => cozy_chess::get_rook_moves(sq, occupied) | cozy_chess::get_bishop_moves(sq, occupied),
        Piece::King => cozy_chess::get_king_moves(sq),
    }
}

/// "White" / "Black".
pub(crate) fn color_name(c: Color) -> &'static str {
    match c {
        Color::White => "White",
        Color::Black => "Black",
    }
}

/// The board after `mv`.
pub(crate) fn after(board: &Board, mv: Move) -> Board {
    let mut b = board.clone();
    b.play_unchecked(mv);
    b
}

/// Whether the side to move has no legal move.
pub(crate) fn no_moves(board: &Board) -> bool {
    !board.generate_moves(|_| true)
}
