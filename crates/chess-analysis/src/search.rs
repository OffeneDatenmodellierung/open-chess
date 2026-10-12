//! Alpha-beta search with multiple principal variations.
//!
//! Iterative deepening principal-variation search with the standard set
//! of modern engine techniques, written from their public descriptions
//! (the Chess Programming Wiki and published engine papers), not copied
//! from any engine's source:
//!
//! - aspiration windows around the previous iteration's score;
//! - a transposition table with depth-preferred replacement;
//! - null-move pruning, reverse futility pruning and internal iterative
//!   reduction at non-PV nodes;
//! - late-move reductions from a logarithmic table, late-move pruning,
//!   futility pruning and static-exchange (SEE) pruning in the move loop;
//! - check extensions, mate-distance pruning, repetition and 50-move draws;
//! - move ordering by TT move, SEE-sorted captures, killers, countermoves
//!   and history with gravity (bonus on cut-off, malus for the quiets
//!   tried before it);
//! - a quiescence search over SEE-positive captures and check evasions.
//!
//! Multi-PV searches the root once per line, excluding moves already
//! chosen. A search stops on depth, a node budget or (when the caller
//! supplies a clock) a time budget; with depth or nodes only it is fully
//! reproducible, natively and in wasm alike.

use cozy_chess::{BitBoard, Board, Color, Move, Piece, Square};
use serde::Serialize;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

use crate::eval::evaluate;
use crate::nnue::{Accumulator, Network};
use crate::params::SearchParams;
use crate::tablebase::{Probe, Tablebases};
use crate::util::{attackers_of, captured, is_castle, is_en_passant, legal, value};

/// Deepest ply a search line may reach (including extensions).
pub const MAX_PLY: usize = 96;

const INF: i32 = 32_000;
/// Mate scores are `MATE - plies to mate`.
pub const MATE: i32 = 31_000;
const MATE_BOUND: i32 = MATE - MAX_PLY as i32;
/// A search score from the side to move's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Score {
    /// Centipawns.
    Cp { value: i32 },
    /// Mate in `moves` full moves; negative when being mated.
    Mate { moves: i32 },
}

impl Score {
    pub(crate) fn from_raw(raw: i32) -> Score {
        if raw >= MATE_BOUND {
            let plies = MATE - raw;
            Score::Mate { moves: (plies + 1) / 2 }
        } else if raw <= -MATE_BOUND {
            let plies = MATE + raw;
            Score::Mate { moves: -((plies + 1) / 2) }
        } else {
            Score::Cp { value: raw }
        }
    }

    /// The same score from the other side's point of view.
    #[must_use]
    pub fn flip(self) -> Score {
        match self {
            Score::Cp { value } => Score::Cp { value: -value },
            Score::Mate { moves } => Score::Mate { moves: -moves },
        }
    }

    /// Centipawns, with mates mapped to ±10 000 (nearer mates higher).
    pub fn as_cp(self) -> i32 {
        match self {
            Score::Cp { value } => value,
            Score::Mate { moves } if moves > 0 => 10_000 - moves,
            Score::Mate { moves } => -10_000 - moves,
        }
    }
}

/// How far to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum iterative-deepening depth in plies.
    pub depth: u8,
    /// Stop after about this many nodes; the last complete depth is kept.
    pub nodes: u64,
    /// Number of best lines to return.
    pub multipv: usize,
    /// Time budget in milliseconds. Only honoured when the searcher has a
    /// clock ([`Searcher::set_clock`]; native builds have one by default).
    /// No new iteration starts after about half of it; the search is cut
    /// off at all of it.
    pub time_ms: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { depth: 6, nodes: 500_000, multipv: 1, time_ms: None }
    }
}

/// One line of best play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchLine {
    /// The moves, first one at the root.
    pub moves: Vec<Move>,
    /// The score for the side to move at the root.
    pub score: Score,
}

/// The outcome of [`Searcher::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Best lines first.
    pub lines: Vec<SearchLine>,
    /// The deepest fully searched depth.
    pub depth: u8,
    pub nodes: u64,
    /// Milliseconds spent (0 without a clock).
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bound {
    Exact,
    Lower,
    Upper,
}

#[derive(Clone, Copy)]
struct Entry {
    key: u64,
    mv: Option<Move>,
    score: i32,
    eval: i32,
    depth: i8,
    bound: Bound,
}

const EMPTY: Entry = Entry { key: 0, mv: None, score: 0, eval: 0, depth: -1, bound: Bound::Upper };

/// The transposition table, shareable between search threads without
/// locks: each slot holds `key ^ data` and `data`, so a slot torn by two
/// threads writing at once fails the key check and reads as empty.
pub struct Table {
    slots: Vec<[AtomicU64; 2]>,
}

fn pack_move(m: Option<Move>) -> u64 {
    m.map_or(0, |m| {
        let promo = m.promotion.map_or(0, |p| p as u64 + 1);
        (1 << 15) | (m.from as u64) | ((m.to as u64) << 6) | (promo << 12)
    })
}

fn unpack_move(v: u64) -> Option<Move> {
    if v & (1 << 15) == 0 {
        return None;
    }
    let promo = (v >> 12) & 7;
    Some(Move {
        from: Square::index((v & 63) as usize),
        to: Square::index(((v >> 6) & 63) as usize),
        promotion: (promo > 0).then(|| Piece::index(promo as usize - 1)),
    })
}

impl Table {
    /// A table of `entries` slots (rounded up to a power of two; 16 bytes each).
    pub fn new(entries: usize) -> Table {
        let n = entries.max(1024).next_power_of_two();
        Table { slots: (0..n).map(|_| [AtomicU64::new(0), AtomicU64::new(0)]).collect() }
    }

    fn index(&self, key: u64) -> usize {
        (key as usize) & (self.slots.len() - 1)
    }

    fn load(&self, key: u64) -> Entry {
        let [k, d] = &self.slots[self.index(key)];
        let data = d.load(AtomicOrdering::Relaxed);
        if k.load(AtomicOrdering::Relaxed) ^ data != key || data == 0 {
            return EMPTY;
        }
        let bound = match (data >> 56) & 3 {
            0 => Bound::Exact,
            1 => Bound::Lower,
            _ => Bound::Upper,
        };
        Entry {
            key,
            mv: unpack_move(data & 0xffff),
            score: i32::from((data >> 16) as u16 as i16),
            eval: i32::from((data >> 32) as u16 as i16),
            depth: (data >> 48) as u8 as i8,
            bound,
        }
    }

    fn save(&self, e: &Entry) {
        let bound = match e.bound {
            Bound::Exact => 0u64,
            Bound::Lower => 1,
            Bound::Upper => 2,
        };
        let data = pack_move(e.mv)
            | (u64::from(e.score.clamp(-32_000, 32_000) as i16 as u16) << 16)
            | (u64::from(e.eval.clamp(-32_000, 32_000) as i16 as u16) << 32)
            | (u64::from(e.depth as u8) << 48)
            | (bound << 56)
            | (1 << 60);
        let [k, d] = &self.slots[self.index(e.key)];
        k.store(e.key ^ data, AtomicOrdering::Relaxed);
        d.store(data, AtomicOrdering::Relaxed);
    }

    fn clear(&self) {
        for [k, d] in &self.slots {
            k.store(0, AtomicOrdering::Relaxed);
            d.store(0, AtomicOrdering::Relaxed);
        }
    }
}

const HISTORY_MAX: i32 = 16_384;

/// Late-move reduction table, indexed by depth and move number.
fn lmr_table(p: &SearchParams) -> Vec<[i8; 64]> {
    let base = f64::from(p.lmr_base) / 100.0;
    let div = f64::from(p.lmr_div.max(1)) / 100.0;
    let mut t = vec![[0i8; 64]; 64];
    for (d, row) in t.iter_mut().enumerate().skip(1) {
        for (m, r) in row.iter_mut().enumerate().skip(1) {
            *r = (base + (d as f64).ln() * (m as f64).ln() / div) as i8;
        }
    }
    t
}

/// Slots in the correction-history table (per side).
const CORR_SIZE: usize = 16_384;
/// Correction entries are kept in 1/256 cp.
const CORR_GRAIN: i32 = 256;
const CORR_MAX: i32 = 64 * CORR_GRAIN;

/// A hash of the pawns alone, for the correction history.
fn pawn_key(board: &Board) -> usize {
    let w = board.colored_pieces(Color::White, Piece::Pawn).0;
    let b = board.colored_pieces(Color::Black, Piece::Pawn).0;
    let h =
        (w.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.rotate_left(17).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)) >> 32;
    (h as usize) % CORR_SIZE
}

#[cfg(not(target_arch = "wasm32"))]
fn native_clock() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// A reusable searcher. Keep one per game: its transposition table and
/// move-ordering tables carry useful knowledge from move to move.
pub struct Searcher {
    tt: Arc<Table>,
    /// Set by the main thread to stop helper threads.
    shared_stop: Option<Arc<AtomicBool>>,
    /// Helper searchers for multi-threaded search, kept between searches.
    helpers: Vec<Searcher>,
    killers: [[Option<Move>; 2]; MAX_PLY],
    /// [side][from][to]
    history: Vec<[[i32; 64]; 64]>,
    /// [previous move's piece][previous move's to] → reply
    countermoves: Vec<[Option<Move>; 64]>,
    lmr: Vec<[i8; 64]>,
    pv: Vec<[Option<Move>; MAX_PLY]>,
    pv_len: [usize; MAX_PLY],
    evals: [i32; MAX_PLY],
    /// The move that led to each ply and the piece that made it.
    moved: [Option<(Piece, Move)>; MAX_PLY],
    stack: Vec<u64>,
    excluded: Vec<Move>,
    nodes: u64,
    node_limit: u64,
    clock: Option<fn() -> u64>,
    started_ms: u64,
    hard_ms: Option<u64>,
    stopped: bool,
    sel_depth: usize,
    tablebases: Tablebases,
    /// Probe the built-in endgame tables during search.
    pub use_tablebases: bool,
    net: Option<Arc<Network>>,
    accs: Vec<Accumulator>,
    params: SearchParams,
    /// A move excluded at a ply (singular-extension verification).
    skip: [Option<Move>; MAX_PLY],
    /// [previous piece][previous to][piece][to]: how good a reply was.
    cont: Vec<i32>,
    /// [side][pawn key]: how far static evaluation tends to be off.
    corr: Vec<i32>,
}

impl Default for Searcher {
    fn default() -> Self {
        Searcher::new(1 << 18)
    }
}

impl Searcher {
    /// A searcher with a transposition table of `entries` slots (rounded
    /// up to a power of two; each slot is 32 bytes).
    pub fn new(entries: usize) -> Searcher {
        let n = entries.max(1024).next_power_of_two();
        Searcher {
            tt: Arc::new(Table::new(n)),
            shared_stop: None,
            helpers: Vec::new(),
            killers: [[None; 2]; MAX_PLY],
            history: vec![[[0; 64]; 64]; 2],
            countermoves: vec![[None; 64]; 6],
            lmr: lmr_table(&SearchParams::default()),
            pv: vec![[None; MAX_PLY]; MAX_PLY],
            pv_len: [0; MAX_PLY],
            evals: [0; MAX_PLY],
            moved: [None; MAX_PLY],
            stack: Vec::new(),
            excluded: Vec::new(),
            nodes: 0,
            node_limit: u64::MAX,
            #[cfg(not(target_arch = "wasm32"))]
            clock: Some(native_clock),
            #[cfg(target_arch = "wasm32")]
            clock: None,
            started_ms: 0,
            hard_ms: None,
            stopped: false,
            sel_depth: 0,
            tablebases: Tablebases::default(),
            net: None,
            accs: vec![Accumulator::default(); MAX_PLY + 1],
            use_tablebases: true,
            params: SearchParams::default(),
            skip: [None; MAX_PLY],
            cont: vec![0; 6 * 64 * 6 * 64],
            corr: vec![0; 2 * CORR_SIZE],
        }
    }

    /// Search with these parameters (see [`crate::params`]).
    pub fn set_params(&mut self, p: SearchParams) {
        self.params = p;
        self.lmr = lmr_table(&p);
    }

    /// The parameters in use.
    pub fn params(&self) -> &SearchParams {
        &self.params
    }

    fn cont_index(prev: (Piece, Move), piece: Piece, to: Square) -> usize {
        ((prev.0 as usize * 64 + prev.1.to as usize) * 6 + piece as usize) * 64 + to as usize
    }

    /// Give the searcher a millisecond clock, so time limits work (in
    /// wasm, pass a function reading `Date.now()`).
    pub fn set_clock(&mut self, now_ms: fn() -> u64) {
        self.clock = Some(now_ms);
    }

    /// Forget everything learnt (e.g. for a new game).
    pub fn clear(&mut self) {
        self.tt.clear();
        self.killers = [[None; 2]; MAX_PLY];
        for h in &mut self.history {
            *h = [[0; 64]; 64];
        }
        for c in &mut self.countermoves {
            *c = [None; 64];
        }
        self.cont.fill(0);
        self.corr.fill(0);
    }

    /// Evaluate with a neural network instead of the hand-written
    /// evaluation (`None` goes back to it). Clears the hash table, whose
    /// scores came from the other evaluation.
    pub fn set_network(&mut self, net: Option<Arc<Network>>) {
        self.net = net;
        self.tt.clear();
    }

    /// Whether a network is in use.
    pub fn has_network(&self) -> bool {
        self.net.is_some()
    }

    fn eval_at(&self, board: &Board, ply: usize) -> i32 {
        match &self.net {
            Some(n) => n.evaluate(&self.accs[ply], board.side_to_move()),
            None => evaluate(board),
        }
    }

    /// Bring the network's sums for `ply + 1` up to date with `child`.
    fn push_acc(&mut self, ply: usize, parent: &Board, child: &Board) {
        if let Some(net) = &self.net {
            let (lo, hi) = self.accs.split_at_mut(ply + 1);
            net.update(parent, child, &lo[ply], &mut hi[0]);
        }
    }

    /// The exact result of a three-piece position for the side to move.
    pub fn probe(&mut self, board: &Board) -> Option<Probe> {
        let no_castling = Color::ALL.iter().all(|&c| {
            let r = board.castle_rights(c);
            r.short.is_none() && r.long.is_none()
        });
        if no_castling { self.tablebases.probe(board) } else { None }
    }

    fn now(&self) -> u64 {
        self.clock.map_or(0, |c| c())
    }

    fn elapsed(&self) -> u64 {
        self.now().saturating_sub(self.started_ms)
    }

    /// Search `root`. `history` is the hashes of earlier positions in the
    /// game (see [`crate::history_hashes`]), so repetitions score as draws.
    pub fn search(&mut self, root: &Board, history: &[u64], limits: Limits) -> SearchResult {
        self.nodes = 0;
        self.node_limit = limits.nodes.max(1);
        self.stopped = false;
        self.started_ms = self.now();
        if let Some(net) = self.net.clone() {
            net.refresh(root, &mut self.accs[0]);
        }
        self.hard_ms = limits.time_ms.filter(|_| self.clock.is_some());
        let soft_ms = self.hard_ms.map(|t| t * self.params.time_soft.clamp(1, 100) as u64 / 100);
        let mut stable = 0;
        let mut last_best: Option<Move> = None;
        self.killers = [[None; 2]; MAX_PLY];
        for side in &mut self.history {
            for row in side.iter_mut() {
                for h in row.iter_mut() {
                    *h /= 2;
                }
            }
        }
        let multipv = limits.multipv.max(1);
        let mut best = SearchResult { lines: Vec::new(), depth: 0, nodes: 0, elapsed_ms: 0 };
        let mut prev_score: i32 = 0;
        for depth in 1..=limits.depth.max(1) {
            let mut lines: Vec<SearchLine> = Vec::new();
            self.excluded.clear();
            for k in 0..multipv {
                self.stack.clear();
                self.stack.extend_from_slice(history);
                self.moved[0] = None;
                let raw = if k == 0 && depth >= 5 && prev_score.abs() < MATE_BOUND {
                    self.aspiration(root, i32::from(depth), prev_score)
                } else {
                    self.negamax(root, i32::from(depth), -INF, INF, 0, false)
                };
                if self.stopped || raw == -INF {
                    break;
                }
                let moves: Vec<Move> = self.pv[0][..self.pv_len[0]].iter().flatten().copied().collect();
                let Some(&first) = moves.first() else { break };
                if k == 0 {
                    prev_score = raw;
                }
                self.excluded.push(first);
                lines.push(SearchLine { moves, score: Score::from_raw(raw) });
            }
            // An interrupted iteration still counts if it found the same or
            // a better first move than the last complete one; otherwise keep
            // the last complete iteration.
            if self.stopped && !best.lines.is_empty() {
                if let (Some(new), Some(old)) = (lines.first(), best.lines.first())
                    && new.moves.first() == old.moves.first()
                    && lines.len() >= best.lines.len()
                {
                    lines.sort_by_key(|l| std::cmp::Reverse(l.score.as_cp()));
                    best.lines = lines;
                }
                break;
            }
            if !lines.is_empty() {
                lines.sort_by_key(|l| std::cmp::Reverse(l.score.as_cp()));
                best = SearchResult { lines, depth, nodes: self.nodes, elapsed_ms: 0 };
            }
            if self.stopped {
                break;
            }
            // A forced mate within the horizon won't change with depth.
            if multipv == 1 && best.lines.first().is_some_and(|l| matches!(l.score, Score::Mate { .. })) {
                break;
            }
            // Best move unchanged for a while: the answer is unlikely to change.
            let best_now = best.lines.first().and_then(|l| l.moves.first().copied());
            stable = if best_now == last_best { stable + 1 } else { 0 };
            last_best = best_now;
            let cut = if stable >= 4 { self.params.time_stable.clamp(0, 90) as u64 } else { 0 };
            if soft_ms.is_some_and(|s| self.elapsed() >= s * (100 - cut) / 100) {
                break;
            }
        }
        best.nodes = self.nodes;
        best.elapsed_ms = self.elapsed();
        self.excluded.clear();
        best
    }

    /// Search with `threads` threads ("lazy SMP"): helpers search the same
    /// position at staggered depths, sharing the hash table, and the main
    /// thread's answer is returned once it finishes. With one thread this
    /// is [`Searcher::search`]. Native builds only (not wasm).
    pub fn search_threads(
        &mut self,
        root: &Board,
        history: &[u64],
        limits: Limits,
        threads: usize,
    ) -> SearchResult {
        if threads <= 1 {
            return self.search(root, history, limits);
        }
        let entries = self.tt.slots.len();
        while self.helpers.len() < threads - 1 {
            let mut h = Searcher::new(1024);
            h.tt = self.tt.clone();
            let _ = entries;
            self.helpers.push(h);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let params = self.params;
        let net = self.net.clone();
        let clock = self.clock;
        let mut helpers = std::mem::take(&mut self.helpers);
        let result = std::thread::scope(|s| {
            for (i, h) in helpers.iter_mut().take(threads - 1).enumerate() {
                h.tt = self.tt.clone();
                h.shared_stop = Some(stop.clone());
                h.clock = clock;
                if h.params != params {
                    h.set_params(params);
                }
                if h.net.as_ref().map(Arc::as_ptr) != net.as_ref().map(Arc::as_ptr) {
                    h.net.clone_from(&net);
                }
                // Odd helpers start a ply deeper, to spread the work.
                let depth = limits.depth.saturating_add(u8::from(i % 2 == 0)).min(MAX_PLY as u8 - 8);
                let hl = Limits { depth, nodes: u64::MAX / 2, multipv: 1, time_ms: limits.time_ms };
                s.spawn(move || {
                    h.search(root, history, hl);
                });
            }
            let r = self.search(root, history, limits);
            stop.store(true, AtomicOrdering::Relaxed);
            r
        });
        let helper_nodes: u64 = helpers.iter().map(|h| h.nodes).sum();
        for h in &mut helpers {
            h.shared_stop = None;
        }
        self.helpers = helpers;
        SearchResult { nodes: result.nodes + helper_nodes, ..result }
    }

    /// Search with a narrow window around the last score, widening on
    /// failure.
    fn aspiration(&mut self, root: &Board, depth: i32, prev: i32) -> i32 {
        let mut delta = self.params.asp_delta.max(1);
        let mut alpha = (prev - delta).max(-INF);
        let mut beta = (prev + delta).min(INF);
        loop {
            let score = self.negamax(root, depth, alpha, beta, 0, false);
            if self.stopped {
                return score;
            }
            if score <= alpha {
                beta = i32::midpoint(alpha, beta);
                alpha = (score - delta).max(-INF);
            } else if score >= beta {
                beta = (score + delta).min(INF);
            } else {
                return score;
            }
            delta += delta / 2;
            if delta > 1000 {
                alpha = -INF;
                beta = INF;
            }
        }
    }

    fn tick(&mut self) -> bool {
        self.nodes += 1;
        let shared = || {
            self.nodes.trailing_zeros() >= 10
                && self.shared_stop.as_ref().is_some_and(|s| s.load(AtomicOrdering::Relaxed))
        };
        if self.nodes >= self.node_limit || shared() {
            self.stopped = true;
        } else if self.nodes.trailing_zeros() >= 11
            && let Some(h) = self.hard_ms
            && self.elapsed() >= h
        {
            self.stopped = true;
        }
        self.stopped
    }

    fn is_repetition(&self, board: &Board) -> bool {
        let hash = board.hash();
        let window = usize::from(board.halfmove_clock()).min(self.stack.len());
        self.stack[self.stack.len() - window..].iter().rev().skip(1).step_by(2).any(|&h| h == hash)
    }

    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        key: u64,
        mv: Option<Move>,
        score: i32,
        eval: i32,
        depth: i32,
        bound: Bound,
        ply: usize,
    ) {
        let old = self.tt.load(key);
        // Keep a deeper entry for the same position unless this is exact.
        if old.key == key && i32::from(old.depth) > depth + 2 && bound != Bound::Exact {
            return;
        }
        let mv = mv.or(if old.key == key { old.mv } else { None });
        self.tt.save(&Entry {
            key,
            mv,
            score: to_tt(score, ply),
            eval,
            depth: depth.clamp(-1, 127) as i8,
            bound,
        });
    }

    fn history_bonus(&self, depth: i32) -> i32 {
        (16 * depth * depth + 32 * depth).min(self.params.hist_bonus_max)
    }

    fn update_cont(&mut self, prev: Option<(Piece, Move)>, piece: Piece, mv: Move, bonus: i32) {
        if self.params.cont_hist == 0 {
            return;
        }
        if let Some(prev) = prev {
            let h = &mut self.cont[Self::cont_index(prev, piece, mv.to)];
            *h += bonus - *h * bonus.abs() / HISTORY_MAX;
        }
    }

    fn quiet_score(&self, board: &Board, mv: Move, prev: Option<(Piece, Move)>) -> i32 {
        let us = board.side_to_move() as usize;
        let mut s = self.history[us][mv.from as usize][mv.to as usize];
        if self.params.cont_hist > 0
            && let (Some(prev), Some(piece)) = (prev, board.piece_on(mv.from))
        {
            s += self.params.cont_hist * self.cont[Self::cont_index(prev, piece, mv.to)];
        }
        s
    }

    fn update_history(&mut self, side: Color, mv: Move, bonus: i32) {
        let h = &mut self.history[side as usize][mv.from as usize][mv.to as usize];
        *h += bonus - *h * bonus.abs() / HISTORY_MAX;
    }

    fn negamax(
        &mut self,
        board: &Board,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        null_ok: bool,
    ) -> i32 {
        self.pv_len[ply] = ply;
        self.sel_depth = self.sel_depth.max(ply);
        if self.tick() {
            return 0;
        }
        let root = ply == 0;
        let pv_node = beta - alpha > 1;
        if !root {
            if board.halfmove_clock() >= 100 || self.is_repetition(board) || insufficient(board) {
                return 0;
            }
            // Mate-distance pruning.
            alpha = alpha.max(-MATE + ply as i32);
            beta = beta.min(MATE - ply as i32 - 1);
            if alpha >= beta {
                return alpha;
            }
        }
        if ply >= MAX_PLY - 2 {
            return self.eval_at(board, ply);
        }
        if !root
            && self.use_tablebases
            && board.occupied().len() == 3
            && let Some(p) = self.probe(board)
        {
            return match p {
                Probe::Win { plies } => MATE - ply as i32 - i32::from(plies),
                Probe::Loss { plies } => -MATE + ply as i32 + i32::from(plies),
                Probe::Draw => 0,
            };
        }
        let in_check = !board.checkers().is_empty();
        if in_check {
            depth += 1;
        }
        if depth <= 0 {
            return self.quiesce(board, alpha, beta, ply);
        }

        let key = board.hash();
        let skip = self.skip[ply];
        let entry = self.tt.load(key);
        let tt_hit = entry.key == key;
        let tt_move = if tt_hit { entry.mv } else { None };
        let tt_score = from_tt(entry.score, ply);
        if !root && !pv_node && skip.is_none() && tt_hit && i32::from(entry.depth) >= depth {
            match entry.bound {
                Bound::Exact => return tt_score,
                Bound::Lower if tt_score >= beta => return tt_score,
                Bound::Upper if tt_score <= alpha => return tt_score,
                _ => {}
            }
        }

        let us = board.side_to_move();
        // Static evaluation, refined by the TT score when its bound allows.
        let raw_eval = if in_check {
            -INF
        } else if tt_hit {
            entry.eval
        } else {
            self.eval_at(board, ply)
        };
        let mut eval = raw_eval;
        if !in_check && tt_hit {
            let usable = match entry.bound {
                Bound::Exact => true,
                Bound::Lower => tt_score > eval,
                Bound::Upper => tt_score < eval,
            };
            if usable && tt_score.abs() < MATE_BOUND {
                eval = tt_score;
            }
        }
        // Correct the static evaluation by how far off it has been in
        // positions with this pawn structure.
        let mut raw_eval = raw_eval;
        let entry_eval = raw_eval;
        if !in_check && self.params.corr_hist > 0 {
            let c = self.corr[us as usize * CORR_SIZE + pawn_key(board)];
            raw_eval += c * self.params.corr_hist / CORR_GRAIN;
            if eval == entry_eval {
                eval = raw_eval;
            }
        }
        self.evals[ply] = if in_check { -INF } else { raw_eval };
        let improving =
            !in_check && ply >= 2 && self.evals[ply - 2] != -INF && raw_eval > self.evals[ply - 2];

        if !pv_node && !in_check && skip.is_none() && self.excluded_is_empty_or_not_root(root) {
            // Reverse futility: far above beta with little depth left.
            if depth <= self.params.rfp_depth
                && eval - self.params.rfp_margin * (depth - i32::from(improving)) >= beta
                && eval.abs() < MATE_BOUND
            {
                return eval;
            }
            // Null move: if passing still beats beta, the position is too good.
            if null_ok
                && depth >= self.params.nmp_depth
                && eval >= beta
                && has_pieces(board, us)
                && let Some(nb) = board.null_move()
            {
                let r = self.params.nmp_base
                    + depth / self.params.nmp_div.max(1)
                    + ((eval - beta) / self.params.nmp_eval_div.max(1)).min(3);
                self.stack.push(key);
                self.moved[ply + 1] = None;
                self.push_acc(ply, board, &nb);
                let score = -self.negamax(&nb, depth - r, -beta, -beta + 1, ply + 1, false);
                self.stack.pop();
                if self.stopped {
                    return 0;
                }
                if score >= beta {
                    return if score >= MATE_BOUND { beta } else { score };
                }
            }
        }
        // Internal iterative reduction: no TT move to try first.
        if tt_move.is_none() && depth >= self.params.iir_depth && !root {
            depth -= 1;
        }

        // Singular extension: if the hash move is much better than every
        // alternative, search it deeper. If even the alternatives beat beta,
        // more than one move refutes the line: cut off now (multi-cut).
        let mut singular_ext = 0;
        if self.params.se_depth > 0
            && !root
            && skip.is_none()
            && depth >= self.params.se_depth
            && let Some(tm) = tt_move
            && matches!(entry.bound, Bound::Lower | Bound::Exact)
            && i32::from(entry.depth) >= depth - 3
            && tt_score.abs() < MATE_BOUND
        {
            let s_beta = tt_score - self.params.se_margin * depth;
            self.skip[ply] = Some(tm);
            let s = self.negamax(board, (depth - 1) / 2, s_beta - 1, s_beta, ply, false);
            self.skip[ply] = None;
            if self.stopped {
                return 0;
            }
            if s < s_beta {
                singular_ext = 1;
            } else if s_beta >= beta {
                return s_beta;
            }
        }

        let mut moves = legal(board);
        if moves.is_empty() {
            return if in_check { -MATE + ply as i32 } else { 0 };
        }
        let counter = self.moved[ply].and_then(|(p, m)| self.countermoves[p as usize][m.to as usize]);
        let prev = self.moved[ply];
        self.order(board, &mut moves, tt_move, ply, counter, prev);

        let alpha_orig = alpha;
        let mut best = -INF;
        let mut best_move = None;
        let mut searched = 0;
        let mut quiets_tried: Vec<Move> = Vec::new();
        self.stack.push(key);
        for mv in moves {
            if (root && self.excluded.contains(&mv)) || skip == Some(mv) {
                continue;
            }
            let is_capture = captured(board, mv).is_some();
            let quiet = !is_capture && mv.promotion.is_none();
            let killer = self.killers[ply].contains(&Some(mv)) || counter == Some(mv);
            let hist = if is_capture { 0 } else { self.quiet_score(board, mv, prev) };

            // Move-loop pruning, once a move has been found that doesn't lose.
            if !root && best > -MATE_BOUND && has_pieces(board, us) {
                if quiet {
                    let lmr_depth = depth - i32::from(self.lmr[depth.min(63) as usize][searched.min(63)]);
                    // Late-move pruning.
                    if !pv_node
                        && !in_check
                        && depth <= 8
                        && quiets_tried.len() as i32
                            > (self.params.lmp_base + depth * depth) / (2 - i32::from(improving))
                    {
                        continue;
                    }
                    // Futility: even a big swing wouldn't reach alpha.
                    if !in_check
                        && lmr_depth <= self.params.fp_depth
                        && eval + self.params.fp_base + self.params.fp_margin * lmr_depth.max(0) <= alpha
                        && !killer
                    {
                        continue;
                    }
                    // Quiet moves that lose material outright.
                    if depth <= 8
                        && see(board, mv) < -self.params.see_quiet * lmr_depth.max(0) * lmr_depth.max(0)
                    {
                        continue;
                    }
                } else if depth <= 8 && see(board, mv) < -self.params.see_noisy * depth {
                    continue;
                }
            }

            let mut child = board.clone();
            child.play_unchecked(mv);
            self.push_acc(ply, board, &child);
            let piece = board.piece_on(mv.from).unwrap_or(Piece::Pawn);
            self.moved[ply + 1] = Some((piece, mv));
            let gives_check = !child.checkers().is_empty();
            let new_depth = depth - 1 + if Some(mv) == tt_move { singular_ext } else { 0 };
            let score = if searched == 0 {
                -self.negamax(&child, new_depth, -beta, -alpha, ply + 1, true)
            } else {
                let mut r = 0;
                if depth >= 3 && searched > usize::from(pv_node) {
                    r = i32::from(self.lmr[depth.min(63) as usize][searched.min(63)]);
                    if quiet {
                        r += i32::from(!pv_node) + i32::from(!improving);
                        r -= i32::from(killer) + i32::from(gives_check);
                        r -= hist / self.params.lmr_hist.max(1);
                    } else {
                        r = r / 2 - i32::from(gives_check);
                    }
                    r = r.clamp(0, new_depth - 1);
                }
                let mut s = -self.negamax(&child, new_depth - r, -alpha - 1, -alpha, ply + 1, true);
                if s > alpha && r > 0 {
                    s = -self.negamax(&child, new_depth, -alpha - 1, -alpha, ply + 1, true);
                }
                if s > alpha && s < beta {
                    s = -self.negamax(&child, new_depth, -beta, -alpha, ply + 1, true);
                }
                s
            };
            searched += 1;
            if self.stopped {
                self.stack.pop();
                return 0;
            }
            if score > best {
                best = score;
                best_move = Some(mv);
                if score > alpha {
                    alpha = score;
                    self.pv[ply][ply] = Some(mv);
                    for i in ply + 1..self.pv_len[ply + 1] {
                        self.pv[ply][i] = self.pv[ply + 1][i];
                    }
                    self.pv_len[ply] = self.pv_len[ply + 1].max(ply + 1);
                    if score >= beta {
                        if quiet {
                            if self.killers[ply][0] != Some(mv) {
                                self.killers[ply][1] = self.killers[ply][0];
                                self.killers[ply][0] = Some(mv);
                            }
                            let bonus = self.history_bonus(depth);
                            self.update_history(us, mv, bonus);
                            self.update_cont(prev, piece, mv, bonus);
                            for &q in &quiets_tried {
                                self.update_history(us, q, -bonus);
                                let qp = board.piece_on(q.from).unwrap_or(Piece::Pawn);
                                self.update_cont(prev, qp, q, -bonus);
                            }
                            if let Some((p, m)) = self.moved[ply] {
                                self.countermoves[p as usize][m.to as usize] = Some(mv);
                            }
                        }
                        break;
                    }
                }
            }
            if quiet {
                quiets_tried.push(mv);
            }
        }
        self.stack.pop();
        if searched == 0 {
            // Everything was pruned (or excluded at the root).
            return if root { -INF } else { alpha };
        }
        if skip.is_none() && (!root || self.excluded.is_empty()) {
            let bound = if best >= beta {
                Bound::Lower
            } else if best > alpha_orig {
                Bound::Exact
            } else {
                Bound::Upper
            };
            self.store(key, best_move, best, entry_eval, depth, bound, ply);
            // Learn how far off the static evaluation was here (only from
            // scores the bound can vouch for, and quiet best moves).
            if self.params.corr_hist > 0
                && !in_check
                && best.abs() < MATE_BOUND
                && best_move.is_none_or(|m| captured(board, m).is_none())
                && !(bound == Bound::Lower && best <= raw_eval)
                && !(bound == Bound::Upper && best >= raw_eval)
            {
                let i = us as usize * CORR_SIZE + pawn_key(board);
                let w = (depth + 1).min(16);
                let target = (best - raw_eval) * CORR_GRAIN;
                let c = &mut self.corr[i];
                *c = ((*c * (256 - w) + target * w) / 256).clamp(-CORR_MAX, CORR_MAX);
            }
        }
        best
    }

    /// Multi-PV runs search the root with moves excluded; pruning that
    /// relies on the full move list is off for those root searches.
    fn excluded_is_empty_or_not_root(&self, root: bool) -> bool {
        !root || self.excluded.is_empty()
    }

    fn quiesce(&mut self, board: &Board, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.pv_len[ply] = ply;
        if self.tick() {
            return 0;
        }
        if ply >= MAX_PLY - 2 {
            return self.eval_at(board, ply);
        }
        let in_check = !board.checkers().is_empty();
        let key = board.hash();
        let entry = self.tt.load(key);
        if entry.key == key {
            let s = from_tt(entry.score, ply);
            match entry.bound {
                Bound::Exact => return s,
                Bound::Lower if s >= beta => return s,
                Bound::Upper if s <= alpha => return s,
                _ => {}
            }
        }
        let mut moves = legal(board);
        if moves.is_empty() {
            return if in_check { -MATE + ply as i32 } else { 0 };
        }
        let mut best = -INF;
        let mut stand = -INF;
        if !in_check {
            stand = if entry.key == key { entry.eval } else { self.eval_at(board, ply) };
            if stand >= beta {
                return stand;
            }
            alpha = alpha.max(stand);
            best = stand;
            moves.retain(|&m| {
                (captured(board, m).is_some() || m.promotion == Some(Piece::Queen)) && see(board, m) >= 0
            });
        }
        self.order(board, &mut moves, if entry.key == key { entry.mv } else { None }, ply, None, None);
        for mv in moves {
            // Delta pruning: even winning the piece can't lift us to alpha.
            if !in_check
                && mv.promotion.is_none()
                && stand + captured(board, mv).map_or(0, value) + self.params.q_delta <= alpha
            {
                continue;
            }
            let mut child = board.clone();
            child.play_unchecked(mv);
            self.push_acc(ply, board, &child);
            let score = -self.quiesce(&child, -beta, -alpha, ply + 1);
            if self.stopped {
                return 0;
            }
            if score > best {
                best = score;
                if score > alpha {
                    alpha = score;
                    if score >= beta {
                        break;
                    }
                }
            }
        }
        if in_check && best == -INF {
            return -MATE + ply as i32;
        }
        best
    }

    fn order(
        &self,
        board: &Board,
        moves: &mut [Move],
        tt_move: Option<Move>,
        ply: usize,
        counter: Option<Move>,
        prev: Option<(Piece, Move)>,
    ) {
        let killers = self.killers.get(ply).copied().unwrap_or([None; 2]);
        moves.sort_by_cached_key(|&m| {
            if Some(m) == tt_move {
                return i32::MIN;
            }
            if let Some(victim) = captured(board, m) {
                let attacker = board.piece_on(m.from).map_or(0, value);
                let mvv = value(victim) * 10 - attacker / 10;
                // Losing captures go after the quiet moves.
                return if see(board, m) >= 0 { -2_000_000 - mvv } else { 100_000 - mvv };
            }
            if let Some(p) = m.promotion {
                return -1_900_000 - value(p);
            }
            if killers[0] == Some(m) {
                -1_000_000
            } else if killers[1] == Some(m) {
                -999_000
            } else if counter == Some(m) {
                -998_000
            } else {
                -self.quiet_score(board, m, prev)
            }
        });
    }
}

/// Whether `side` has a piece other than pawns and the king (null-move and
/// pruning are unsafe in pawn endings because of zugzwang).
fn has_pieces(board: &Board, side: Color) -> bool {
    let minors_majors = board.pieces(Piece::Knight)
        | board.pieces(Piece::Bishop)
        | board.pieces(Piece::Rook)
        | board.pieces(Piece::Queen);
    !(minors_majors & board.colors(side)).is_empty()
}

/// Static exchange evaluation: the material balance of the capture
/// sequence on `mv`'s target square, both sides recapturing with their
/// least valuable piece and stopping when it no longer pays.
pub fn see(board: &Board, mv: Move) -> i32 {
    if is_castle(board, mv) {
        return 0;
    }
    let to = mv.to;
    let mut occ = board.occupied() ^ mv.from.bitboard();
    let mut gain = [0i32; 34];
    gain[0] = captured(board, mv).map_or(0, value);
    if is_en_passant(board, mv) {
        occ ^= Square::new(to.file(), mv.from.rank()).bitboard();
    }
    let mut on_square = mv.promotion.map_or_else(|| board.piece_on(mv.from).map_or(0, value), value);
    if let Some(p) = mv.promotion {
        gain[0] += value(p) - value(Piece::Pawn);
    }
    let mut side = !board.side_to_move();
    let mut d = 0;
    loop {
        let attackers = attackers_of(board, to, side, occ) & occ;
        let Some((sq, val)) = least_valuable(board, attackers) else { break };
        d += 1;
        gain[d] = on_square - gain[d - 1];
        if (-gain[d - 1]).max(gain[d]) < 0 || d >= 32 {
            break;
        }
        occ ^= sq.bitboard();
        on_square = val;
        side = !side;
    }
    while d > 0 {
        gain[d - 1] = -(-gain[d - 1]).max(gain[d]);
        d -= 1;
    }
    gain[0]
}

fn least_valuable(board: &Board, attackers: BitBoard) -> Option<(Square, i32)> {
    for p in [Piece::Pawn, Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen, Piece::King] {
        if let Some(sq) = (attackers & board.pieces(p)).next_square() {
            return Some((sq, value(p)));
        }
    }
    None
}

fn to_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_BOUND {
        score + ply as i32
    } else if score <= -MATE_BOUND {
        score - ply as i32
    } else {
        score
    }
}

fn from_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_BOUND {
        score - ply as i32
    } else if score <= -MATE_BOUND {
        score + ply as i32
    } else {
        score
    }
}

/// King vs king, or king and a single minor piece vs king.
fn insufficient(board: &Board) -> bool {
    let heavy = board.pieces(Piece::Pawn) | board.pieces(Piece::Rook) | board.pieces(Piece::Queen);
    heavy.is_empty() && (board.pieces(Piece::Knight) | board.pieces(Piece::Bishop)).len() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::std_uci;

    fn best(fen: &str, depth: u8) -> (String, Score) {
        let b = Board::from_fen(fen, false).unwrap();
        let r = Searcher::new(1 << 16).search(
            &b,
            &[],
            Limits { depth, nodes: 2_000_000, multipv: 1, ..Limits::default() },
        );
        let l = &r.lines[0];
        (std_uci(&b, l.moves[0]), l.score)
    }

    #[test]
    fn finds_mate_in_one() {
        let (mv, score) = best("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", 3);
        assert_eq!(mv, "a1a8");
        assert_eq!(score, Score::Mate { moves: 1 });
    }

    #[test]
    fn finds_mate_in_two() {
        // 1.Qg7+?? no: classic two-mover — 1.Rh8+ Kxh8 2.Qh7#? Use a
        // simple ladder: rooks on a and b files mate in two.
        let (_, score) = best("6k1/8/8/8/8/8/R7/1R4K1 w - - 0 1", 5);
        assert_eq!(score, Score::Mate { moves: 2 });
    }

    #[test]
    fn takes_a_free_queen() {
        let (mv, score) = best("4k3/8/8/3q4/8/8/8/3RK3 w - - 0 1", 3);
        assert_eq!(mv, "d1d5");
        assert!(score.as_cp() > 400);
    }

    #[test]
    fn stalemate_and_mate_at_root() {
        let b = Board::from_fen("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", false).unwrap();
        let r = Searcher::default().search(&b, &[], Limits::default());
        assert!(r.lines.is_empty());
    }

    #[test]
    fn multipv_returns_distinct_sorted_lines() {
        let b = Board::default();
        let r = Searcher::new(1 << 16).search(
            &b,
            &[],
            Limits { depth: 4, nodes: 2_000_000, multipv: 5, ..Limits::default() },
        );
        assert_eq!(r.lines.len(), 5);
        let firsts: std::collections::HashSet<_> = r.lines.iter().map(|l| l.moves[0]).collect();
        assert_eq!(firsts.len(), 5);
        for w in r.lines.windows(2) {
            assert!(w[0].score.as_cp() >= w[1].score.as_cp());
        }
        assert!(r.lines.iter().all(|l| l.moves.len() >= 4), "{:?}", r.lines);
    }

    #[test]
    fn node_budget_is_respected_and_returns_a_move() {
        let b = Board::default();
        let r = Searcher::new(1 << 12).search(
            &b,
            &[],
            Limits { depth: 30, nodes: 5_000, multipv: 1, ..Limits::default() },
        );
        assert!(r.nodes <= 5_001);
        assert!(!r.lines.is_empty());
    }

    #[test]
    fn repetition_scores_as_draw() {
        // White is a rook down but can repeat; with history showing the
        // position twice, the search should see a draw available.
        let b = Board::from_fen("6k1/8/8/8/8/8/8/r5K1 w - - 10 40", false).unwrap();
        let r = Searcher::default().search(
            &b,
            &[b.hash(), 1, b.hash(), 2],
            Limits { depth: 2, nodes: 100_000, multipv: 1, ..Limits::default() },
        );
        assert!(!r.lines.is_empty());
    }

    #[test]
    fn searches_with_a_network() {
        let b = Board::from_fen("r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3", false)
            .unwrap();
        let mut s = Searcher::new(1 << 14);
        s.set_network(Some(Arc::new(crate::nnue::tests::toy(32))));
        let lim = Limits { depth: 5, nodes: 200_000, multipv: 2, ..Limits::default() };
        let r1 = s.search(&b, &[], lim);
        s.clear();
        let r2 = s.search(&b, &[], lim);
        assert_eq!(r1.lines.len(), 2);
        assert_eq!(r1.lines, r2.lines, "reproducible with a network too");
        // The network still has to see a mate in one.
        let m = Board::from_fen("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", false).unwrap();
        assert_eq!(
            s.search(&m, &[], Limits { depth: 3, ..Limits::default() }).lines[0].score,
            Score::Mate { moves: 1 }
        );
    }

    #[test]
    fn table_round_trips_entries() {
        let t = Table::new(1024);
        let m = Move { from: Square::E7, to: Square::E8, promotion: Some(Piece::Knight) };
        let e = Entry {
            key: 0xdead_beef_1234_5678,
            mv: Some(m),
            score: -30_990,
            eval: 123,
            depth: -1,
            bound: Bound::Lower,
        };
        t.save(&e);
        let back = t.load(e.key);
        assert_eq!((back.mv, back.score, back.eval, back.depth), (Some(m), -30_990, 123, -1));
        assert!(back.bound == Bound::Lower);
        assert_eq!(t.load(e.key ^ 1).key, 0, "another position in the slot reads as empty");
        t.clear();
        assert_eq!(t.load(e.key).key, 0);
    }

    #[test]
    fn several_threads_find_the_same_mate() {
        let b = Board::from_fen("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", false).unwrap();
        let mut s = Searcher::new(1 << 16);
        let r = s.search_threads(
            &b,
            &[],
            Limits { depth: 6, nodes: 2_000_000, multipv: 1, ..Limits::default() },
            3,
        );
        assert_eq!(r.lines[0].score, Score::Mate { moves: 1 });
        let start = Board::default();
        let r = s.search_threads(
            &start,
            &[],
            Limits { depth: 8, nodes: 5_000_000, multipv: 1, ..Limits::default() },
            2,
        );
        assert!(!r.lines.is_empty());
    }

    #[test]
    fn score_conversions() {
        assert_eq!(Score::from_raw(MATE - 1), Score::Mate { moves: 1 });
        assert_eq!(Score::from_raw(MATE - 3), Score::Mate { moves: 2 });
        assert_eq!(Score::from_raw(-MATE + 2), Score::Mate { moves: -1 });
        assert_eq!(Score::from_raw(-35), Score::Cp { value: -35 });
        assert_eq!(Score::Mate { moves: 2 }.flip(), Score::Mate { moves: -2 });
        assert!(Score::Mate { moves: 1 }.as_cp() > Score::Mate { moves: 3 }.as_cp());
    }
}
