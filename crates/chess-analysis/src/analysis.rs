//! Everything the training view needs for one position, serialisable.

use chess_core::{Game, Side};
use cozy_chess::{Board, Color, Move};
use serde::Serialize;

use crate::eval::phase;
use crate::explain::{MoveExplanation, explain_move};
use crate::openings::{Opening, opening};
use crate::patterns::{Pattern, detect};
use crate::search::{Limits, Score, Searcher};
use crate::util::{after, history_hashes, legal};
use crate::wdl::{Params, Wdl};

/// How hard to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyseOptions {
    pub depth: u8,
    pub nodes: u64,
    /// How many candidate lines (the "top five").
    pub lines: usize,
    /// How many plies of each line to return.
    pub line_plies: usize,
    /// Time budget in milliseconds (needs a clock: native builds have
    /// one, wasm builds get one from `chess-wasm`).
    pub time_ms: Option<u64>,
}

impl Default for AnalyseOptions {
    fn default() -> Self {
        AnalyseOptions { depth: 6, nodes: 600_000, lines: 5, line_plies: 10, time_ms: None }
    }
}

/// One move inside a line, ready to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineMove {
    /// "12." or "12…" before the SAN.
    pub number: String,
    pub side: &'static str,
    #[serde(flatten)]
    pub explanation: MoveExplanation,
    pub patterns: Vec<Pattern>,
    /// Past the main search's horizon: extended by quick follow-up
    /// searches, so less reliable than the moves before it.
    pub projected: bool,
}

/// A candidate line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub rank: usize,
    /// From White's point of view.
    pub score: Score,
    pub wdl: Wdl,
    pub moves: Vec<LineMove>,
}

/// One legal move with its explanation and motifs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveInfo {
    #[serde(flatten)]
    pub explanation: MoveExplanation,
    pub patterns: Vec<Pattern>,
}

/// The analysis of a position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub fen: String,
    pub turn: &'static str,
    /// Best score found, from White's point of view (None if the game is over).
    pub score: Option<Score>,
    pub wdl: Wdl,
    /// White's expected score, 0–1.
    pub expected_white: f64,
    pub depth: u8,
    pub nodes: u64,
    pub lines: Vec<Line>,
    pub opening: Option<Opening>,
    /// Set when the game has ended ("mate", "stalemate", "draw"…).
    pub result: Option<String>,
    /// The exact result from the built-in tablebases (three pieces),
    /// for the side to move.
    pub tablebase: Option<crate::tablebase::Probe>,
}

fn white_view(score: Score, stm: Color) -> Score {
    if stm == Color::White { score } else { score.flip() }
}

/// Analyse the game's current position with a fresh searcher.
pub fn analyse(game: &Game, opts: AnalyseOptions) -> Analysis {
    analyse_with(&mut Searcher::default(), game, opts)
}

/// Analyse with a caller-owned searcher (keeps its table between moves).
pub fn analyse_with(searcher: &mut Searcher, game: &Game, opts: AnalyseOptions) -> Analysis {
    let board = game.board();
    let stm = board.side_to_move();
    let params = Params::for_phase(phase(board));
    let opening = opening(&game.uci_moves());
    let base = Analysis {
        fen: game.fen(),
        turn: if stm == Color::White { "white" } else { "black" },
        score: None,
        wdl: Wdl::certain(None),
        expected_white: 0.5,
        depth: 0,
        nodes: 0,
        lines: Vec::new(),
        opening,
        result: None,
        tablebase: None,
    };
    if let Some(o) = game.outcome() {
        let wdl = Wdl::certain(o.winner.map(|w| w == Side::White));
        return Analysis {
            expected_white: wdl.expected_white(),
            wdl,
            result: Some(o.status.name().to_string()),
            ..base
        };
    }

    let result = searcher.search(
        board,
        &history_hashes(game),
        Limits {
            depth: opts.depth.max(1),
            nodes: opts.nodes,
            multipv: opts.lines.max(1),
            time_ms: opts.time_ms,
        },
    );
    let mut ext = Searcher::new(1 << 14);
    let lines: Vec<Line> = result
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let score = white_view(l.score, stm);
            Line {
                rank: i + 1,
                score,
                wdl: Wdl::from_score(score, params),
                moves: describe_line(
                    extend_line(&mut ext, board, &l.moves, opts.line_plies),
                    board,
                    l.moves.len(),
                    opts.line_plies,
                ),
            }
        })
        .collect();
    let score = lines.first().map(|l| l.score);
    let wdl = lines.first().map_or(Wdl::certain(None), |l| l.wdl);
    Analysis {
        score,
        wdl,
        expected_white: wdl.expected_white(),
        depth: result.depth,
        nodes: result.nodes,
        lines,
        tablebase: searcher.probe(board),
        ..base
    }
}

/// The line's moves, extended past the search horizon to `want` plies
/// with shallow searches (stopping early if the game would end).
fn extend_line(ext: &mut Searcher, start: &Board, moves: &[Move], want: usize) -> Vec<Move> {
    let mut line: Vec<Move> = moves.iter().copied().take(want).collect();
    let mut b = start.clone();
    let mut seen = Vec::new();
    for &m in &line {
        seen.push(b.hash());
        b = after(&b, m);
    }
    while line.len() < want {
        let r = ext.search(&b, &seen, Limits { depth: 4, nodes: 30_000, multipv: 1, time_ms: None });
        let Some(&m) = r.lines.first().and_then(|l| l.moves.first()) else { break };
        line.push(m);
        seen.push(b.hash());
        b = after(&b, m);
    }
    line
}

fn describe_line(moves: Vec<Move>, start: &Board, searched: usize, max: usize) -> Vec<LineMove> {
    let mut b = start.clone();
    let mut out = Vec::new();
    for (i, mv) in moves.into_iter().take(max).enumerate() {
        let white = b.side_to_move() == Color::White;
        let n = b.fullmove_number();
        out.push(LineMove {
            number: if white { format!("{n}.") } else { format!("{n}…") },
            side: if white { "white" } else { "black" },
            explanation: explain_move(&b, mv),
            patterns: detect(&b, mv),
            projected: i >= searched,
        });
        b = after(&b, mv);
    }
    out
}

/// Every legal move in the position, explained, with its motifs: the
/// training dictionary for "what could I play here?".
pub fn legal_moves(game: &Game) -> Vec<MoveInfo> {
    if game.outcome().is_some() {
        return Vec::new();
    }
    let b = game.board();
    legal(b)
        .into_iter()
        .map(|m| MoveInfo { explanation: explain_move(b, m), patterns: detect(b, m) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_position_has_five_lines_and_balanced_odds() {
        let g = Game::new(None).unwrap();
        let a =
            analyse(&g, AnalyseOptions { depth: 4, nodes: 400_000, lines: 5, line_plies: 5, time_ms: None });
        assert_eq!(a.lines.len(), 5);
        assert!(
            a.lines.iter().all(|l| l.moves.len() == 5),
            "{:?}",
            a.lines.iter().map(|l| l.moves.len()).collect::<Vec<_>>()
        );
        assert!(a.lines.iter().all(|l| !l.moves[0].projected && l.moves[4].projected));
        assert!(a.wdl.white > 0.2 && a.wdl.black > 0.15, "{:?}", a.wdl);
        assert_eq!(a.lines[0].moves[0].number, "1.");
        assert_eq!(a.lines[0].moves[1].number, "1…");
        let json = serde_json::to_value(&a).unwrap();
        assert!(json["lines"][0]["moves"][0]["san"].is_string());
    }

    #[test]
    fn winning_side_is_favoured() {
        let g = Game::new(Some("4k3/8/8/8/8/8/8/3QK3 b - - 0 1")).unwrap();
        let a =
            analyse(&g, AnalyseOptions { depth: 3, nodes: 200_000, lines: 1, line_plies: 3, time_ms: None });
        assert!(a.wdl.white > 0.95, "{:?}", a.wdl);
        assert_eq!(a.turn, "black");
    }

    #[test]
    fn finished_game_is_certain() {
        let g = Game::from_moves(None, "f2f3 e7e5 g2g4 d8h4").unwrap();
        let a = analyse(&g, AnalyseOptions::default());
        assert_eq!(a.wdl, Wdl::certain(Some(false)));
        assert_eq!(a.result.as_deref(), Some("mate"));
        assert!(legal_moves(&g).is_empty());
    }

    #[test]
    fn legal_move_dictionary_covers_every_move() {
        let g = Game::new(None).unwrap();
        let moves = legal_moves(&g);
        assert_eq!(moves.len(), 20);
        assert!(
            moves.iter().any(|m| m.explanation.san == "Nc3" && m.explanation.text == "Knight from b1 to c3.")
        );
    }

    #[test]
    fn names_the_opening() {
        let g = Game::from_moves(None, "e2e4 e7e6 d2d4 d7d5").unwrap();
        let a =
            analyse(&g, AnalyseOptions { depth: 1, nodes: 10_000, lines: 1, line_plies: 1, time_ms: None });
        assert_eq!(a.opening.unwrap().name, "French Defense");
    }
}
