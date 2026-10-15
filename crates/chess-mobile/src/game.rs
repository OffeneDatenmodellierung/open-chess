//! A game on the device: the moves, the bot, the training analysis, what
//! help was used, and the record a finished offline game is uploaded as.

use std::sync::{Mutex, MutexGuard};

use chess_analysis::search::Searcher;
use chess_analysis::{
    AnalyseOptions, Level, analyse_with, best_move, bot_move, explain_san, legal_moves, openings,
};
use chess_core::{Game, PgnGame, PgnPlayer, STARTPOS_FEN, Side, Status};
use serde::Serialize;

use crate::{MobileError, json};

/// Who the game is against, which decides what help is allowed and whether
/// it can be rated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GameMode {
    /// The bot at a level, 1–8, on the device.
    Computer { level: u8 },
    /// Two players on one device.
    PassAndPlay,
    /// A game on open-chess or Lichess, followed here. Rated games allow
    /// no analysis, hints or move explanations.
    Online { rated: bool },
}

/// The position and the moves so far.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GameState {
    pub fen: String,
    pub initial_fen: String,
    /// "white" or "black".
    pub turn: String,
    pub ply: u32,
    pub uci: Vec<String>,
    pub san: Vec<String>,
    /// Lichess's status names: "started", "mate", "resign", "stalemate", "draw"…
    pub status: String,
    /// "white", "black", or none (a draw, or still playing).
    pub winner: Option<String>,
    pub check: bool,
    pub last: Option<String>,
    /// The named opening, like "Italian Game: Giuoco Piano".
    pub opening: Option<String>,
    pub eco: Option<String>,
}

/// A move that was played.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PlayedMove {
    pub uci: String,
    pub san: String,
    pub capture: bool,
    pub check: bool,
}

/// A move the bot chose (not yet played).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BotMove {
    pub uci: String,
    pub san: String,
}

/// The help used in this game. Any of it makes the game unrated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, uniffi::Record)]
pub struct Assistance {
    /// Hints asked for (the best move shown).
    pub hints: u32,
    /// Moves taken back.
    pub takebacks: u32,
    /// Whether the analysis (lines, odds, explanations) was looked at.
    pub analysed: bool,
}

impl Assistance {
    fn any(self) -> bool {
        self.hints > 0 || self.takebacks > 0 || self.analysed
    }
}

struct Inner {
    game: Game,
    mode: GameMode,
    analysis: Searcher,
    bot: Searcher,
    /// When each move was played (Unix ms), one per ply.
    move_times_ms: Vec<u64>,
    /// A result decided off the board: resignation or an agreed draw.
    ended: Option<(Status, Option<Side>)>,
    help: Assistance,
}

impl Inner {
    fn result(&self) -> (Status, Option<Side>) {
        if let Some(e) = self.ended {
            return e;
        }
        self.game.outcome().map_or((Status::Started, None), |o| (o.status, o.winner))
    }

    fn allow_help(&mut self, what: &str) -> Result<(), MobileError> {
        if self.mode == (GameMode::Online { rated: true }) {
            return Err(MobileError::NotAllowed { message: format!("{what} in a rated online game") });
        }
        Ok(())
    }
}

/// A game plus the engines that work on it.
#[derive(uniffi::Object)]
pub struct GameSession {
    inner: Mutex<Inner>,
}

fn start_fen(fen: Option<String>) -> Option<String> {
    fen.map(|f| f.trim().to_owned()).filter(|f| !f.is_empty() && f != STARTPOS_FEN)
}

fn invalid(e: impl std::fmt::Debug) -> MobileError {
    MobileError::InvalidPosition { message: format!("{e:?}") }
}

#[uniffi::export]
impl GameSession {
    /// A new game from the start, or from a FEN.
    #[uniffi::constructor]
    pub fn new(fen: Option<String>, mode: GameMode) -> Result<Self, MobileError> {
        let game = Game::new(start_fen(fen).as_deref()).map_err(invalid)?;
        Ok(GameSession {
            inner: Mutex::new(Inner {
                game,
                mode,
                analysis: Searcher::new(1 << 18),
                bot: Searcher::new(1 << 18),
                move_times_ms: Vec::new(),
                ended: None,
                help: Assistance::default(),
            }),
        })
    }

    /// A game replayed from its moves (space-separated UCI), as when a
    /// saved game is opened again. `move_times_ms` may be empty.
    #[uniffi::constructor]
    pub fn resume(
        fen: Option<String>,
        moves: String,
        move_times_ms: Vec<u64>,
        mode: GameMode,
        help: Assistance,
    ) -> Result<Self, MobileError> {
        let game = Game::from_moves(start_fen(fen).as_deref(), &moves).map_err(invalid)?;
        let mut times = move_times_ms;
        times.truncate(game.ply());
        Ok(GameSession {
            inner: Mutex::new(Inner {
                game,
                mode,
                analysis: Searcher::new(1 << 18),
                bot: Searcher::new(1 << 18),
                move_times_ms: times,
                ended: None,
                help,
            }),
        })
    }

    pub fn mode(&self) -> GameMode {
        self.lock().mode
    }

    /// The help used so far.
    pub fn assistance(&self) -> Assistance {
        self.lock().help
    }

    /// The position and moves so far.
    pub fn state(&self) -> GameState {
        let i = self.lock();
        let g = &i.game;
        let (status, winner) = i.result();
        let uci = g.uci_moves();
        let opening = openings::opening(&uci);
        GameState {
            fen: g.fen(),
            initial_fen: g.initial_fen_or_start(),
            turn: g.turn().name().into(),
            ply: u32::try_from(g.ply()).unwrap_or(u32::MAX),
            last: uci.last().cloned(),
            uci,
            san: g.sans().to_vec(),
            status: status.name().into(),
            winner: winner.map(|s| s.name().into()),
            check: !g.board().checkers().is_empty(),
            opening: opening.as_ref().map(|o| o.name.into()),
            eco: opening.map(|o| o.eco.into()),
        }
    }

    /// The legal moves in UCI, for move input (no explanations, so it is
    /// never counted as help).
    pub fn legal_uci(&self) -> Vec<String> {
        self.lock().game.legal_moves()
    }

    /// Play a move given as UCI ("e2e4") or SAN ("Nf3", "O-O"), made at
    /// `at_ms` (Unix ms; the upload carries every move's time).
    pub fn play(&self, mv: String, at_ms: u64) -> Result<PlayedMove, MobileError> {
        let mut i = self.lock();
        if i.ended.is_some() {
            return Err(MobileError::IllegalMove { message: "the game is over".into() });
        }
        let mv = mv.trim();
        let uci = if i.game.legal_moves().iter().any(|m| m == mv) {
            mv.to_owned()
        } else {
            explain_san(i.game.board(), mv)
                .map(|e| e.uci)
                .ok_or_else(|| MobileError::IllegalMove { message: mv.to_owned() })?
        };
        let p = i
            .game
            .play_uci(&uci)
            .map_err(|e| MobileError::IllegalMove { message: format!("{mv}: {e:?}") })?;
        i.move_times_ms.push(at_ms);
        Ok(PlayedMove { uci: p.uci, san: p.san, capture: p.capture, check: p.check })
    }

    /// Take back `n` plies. Counts as help.
    pub fn undo(&self, n: u32) {
        let mut i = self.lock();
        let n = usize::try_from(n).unwrap_or(usize::MAX).min(i.game.ply());
        if n == 0 {
            return;
        }
        i.game.undo(n);
        let ply = i.game.ply();
        i.move_times_ms.truncate(ply);
        i.ended = None;
        i.help.takebacks += u32::try_from(n).unwrap_or(u32::MAX);
    }

    /// `side` ("white" or "black") resigns.
    pub fn resign(&self, side: String) -> Result<(), MobileError> {
        let side =
            Side::from_name(&side).ok_or_else(|| MobileError::IllegalMove { message: side.clone() })?;
        self.lock().ended = Some((Status::Resign, Some(side.opposite())));
        Ok(())
    }

    /// `side` ("white" or "black") ran out of time. Lichess's rule: a draw
    /// when the other side couldn't have mated.
    pub fn flag(&self, side: String) -> Result<(), MobileError> {
        let side =
            Side::from_name(&side).ok_or_else(|| MobileError::IllegalMove { message: side.clone() })?;
        let mut i = self.lock();
        let winner = side.opposite();
        i.ended = Some(if i.game.cannot_win(winner) {
            (Status::Outoftime, None)
        } else {
            (Status::Outoftime, Some(winner))
        });
        Ok(())
    }

    /// Both players agreed a draw.
    pub fn agree_draw(&self) {
        self.lock().ended = Some((Status::Draw, None));
    }

    /// The bot's move at its level (the session's level when it is a game
    /// against the computer, else `level`). Not played; `seed` varies its
    /// human-looking slips.
    pub fn bot_move(&self, level: u8, seed: u64) -> Option<BotMove> {
        let mut i = self.lock();
        let level = match i.mode {
            GameMode::Computer { level } => level,
            _ => level,
        };
        let Inner { bot, game, .. } = &mut *i;
        bot_move(bot, game, Level::new(level), seed).map(|m| BotMove { uci: m.uci, san: m.san })
    }

    /// A hint: the best move found in `time_ms`. Counts as help.
    pub fn hint(&self, time_ms: u32) -> Result<Option<BotMove>, MobileError> {
        let mut i = self.lock();
        i.allow_help("hints")?;
        i.help.hints += 1;
        let Inner { analysis, game, .. } = &mut *i;
        Ok(best_move(analysis, game, u64::from(time_ms), 64).map(|m| BotMove { uci: m.uci, san: m.san }))
    }

    /// Win/draw/loss odds and the best `lines` lines, as the Chess lab's
    /// JSON. `time_ms` 0 means depth and nodes only. Counts as help.
    pub fn analyse_json(
        &self,
        depth: u8,
        nodes: u32,
        lines: u8,
        plies: u8,
        time_ms: u32,
    ) -> Result<String, MobileError> {
        let mut i = self.lock();
        i.allow_help("analysis")?;
        i.help.analysed = true;
        let opts = AnalyseOptions {
            depth,
            nodes: u64::from(nodes),
            lines: usize::from(lines),
            line_plies: usize::from(plies),
            time_ms: (time_ms > 0).then_some(u64::from(time_ms)),
        };
        let Inner { analysis, game, .. } = &mut *i;
        Ok(json(&analyse_with(analysis, game, opts)))
    }

    /// Every legal move explained, with the patterns it makes (JSON).
    /// Counts as help.
    pub fn legal_moves_json(&self) -> Result<String, MobileError> {
        let mut i = self.lock();
        i.allow_help("move explanations")?;
        i.help.analysed = true;
        Ok(json(&legal_moves(&i.game)))
    }

    /// A move explained in plain English without playing it (JSON).
    /// Explaining a move already played is allowed in any game; this one
    /// looks ahead, so it counts as help.
    pub fn explain_json(&self, mv: String) -> Result<String, MobileError> {
        let mut i = self.lock();
        i.allow_help("move explanations")?;
        i.help.analysed = true;
        Ok(json(&explain_san(i.game.board(), &mv)))
    }

    /// The exact result in a three-piece ending (JSON, or `null`). Counts
    /// as help.
    pub fn tablebase_json(&self) -> Result<String, MobileError> {
        let mut i = self.lock();
        i.allow_help("the tablebase")?;
        i.help.analysed = true;
        let board = i.game.board().clone();
        Ok(json(&i.analysis.probe(&board)))
    }

    /// Match a board read from a photo (FEN placement) against this
    /// position and every position one move away, best first (JSON, top
    /// five, or `null`). Updating a game from a photo is not help.
    pub fn match_photo_json(&self, placement: String) -> String {
        let i = self.lock();
        let Some(grid) = chess_analysis::photo::parse_placement(&placement) else {
            return "null".into();
        };
        let mut c = chess_analysis::photo::match_after_move(i.game.board(), &grid);
        c.truncate(5);
        json(&c)
    }

    /// Use an NNUE weight file for the analysis and the bot.
    pub fn load_network(&self, bytes: Vec<u8>) -> Result<(), MobileError> {
        let net = std::sync::Arc::new(chess_analysis::nnue::Network::from_bytes(&bytes).map_err(invalid)?);
        let mut i = self.lock();
        i.analysis.set_network(Some(net.clone()));
        i.bot.set_network(Some(net));
        Ok(())
    }

    /// The game as PGN.
    pub fn pgn(&self, white: String, black: String, event: String) -> String {
        let i = self.lock();
        let (status, winner) = i.result();
        let first_fullmove = i
            .game
            .initial_fen()
            .and_then(|f| f.split_whitespace().nth(5))
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        chess_core::pgn(&PgnGame {
            event: &event,
            site: "open-chess".into(),
            game_id: "local",
            created_at_ms: i.move_times_ms.first().map_or(0, |&t| i64::try_from(t).unwrap_or(i64::MAX)),
            white: PgnPlayer { name: white, rating: None, rating_diff: None, title: None },
            black: PgnPlayer { name: black, rating: None, rating_diff: None, title: None },
            status,
            winner,
            clock: None,
            initial_fen: i.game.initial_fen(),
            sans: i.game.sans(),
            first_to_move: i.game.first_to_move(),
            first_fullmove,
            clocks_ms: &[],
        })
    }

    /// Whether the game is over and, by what this device knows, could be
    /// rated: finished, no help used, and not an online game (those are
    /// rated by the server they're played on). The server decides.
    pub fn ratable(&self) -> bool {
        let i = self.lock();
        let finished = i.result().0 != Status::Started;
        finished && !i.help.any() && !matches!(i.mode, GameMode::Online { .. })
    }

    /// The record a finished offline game is uploaded as
    /// (`POST /api/games/offline`), as JSON. `client_id` is the app's id
    /// for the game, so a repeated upload is recognised.
    pub fn offline_record_json(
        &self,
        client_id: String,
        white: String,
        black: String,
        speed: String,
        started_ms: u64,
    ) -> Result<String, MobileError> {
        let ratable = self.ratable();
        let i = self.lock();
        let (mode, bot_level) = match i.mode {
            GameMode::Computer { level } => ("computer", Some(Level::new(level).get())),
            GameMode::PassAndPlay => ("passAndPlay", None),
            GameMode::Online { .. } => {
                return Err(MobileError::NotAllowed {
                    message: "online games are recorded by their server".into(),
                });
            }
        };
        let (status, winner) = i.result();
        Ok(json(&OfflineRecord {
            client_id: &client_id,
            mode,
            bot_level,
            bot_rating: bot_level.map(|l| Level::new(l).rating()),
            white: &white,
            black: &black,
            speed: &speed,
            initial_fen: i.game.initial_fen(),
            moves: i.game.uci_moves(),
            move_times_ms: &i.move_times_ms,
            started_ms,
            status: status.name(),
            winner: winner.map(Side::name),
            assistance: i.help,
            ratable,
        }))
    }

    /// Start fresh analysis tables (a new game in the same session).
    pub fn reset_engines(&self) {
        let mut i = self.lock();
        i.analysis.clear();
        i.bot.clear();
    }
}

impl GameSession {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves a usable game: recover it.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OfflineRecord<'a> {
    client_id: &'a str,
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_rating: Option<i32>,
    white: &'a str,
    black: &'a str,
    speed: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_fen: Option<&'a str>,
    moves: Vec<String>,
    move_times_ms: &'a [u64],
    started_ms: u64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<&'static str>,
    assistance: Assistance,
    ratable: bool,
}

/// Every move of a game in plain English ("Knight from b1 to c3, check"),
/// from its start (a FEN, or the starting position) and its moves (space-
/// separated UCI). For showing moves in words next to SAN and for speaking
/// them; describing moves already played is never help. Stops at the first
/// move that isn't legal.
#[uniffi::export]
pub fn describe_moves(fen: Option<String>, moves: String) -> Vec<String> {
    let Ok(mut game) = Game::new(start_fen(fen).as_deref()) else { return Vec::new() };
    let mut out = Vec::new();
    for uci in moves.split_whitespace() {
        let Some(e) = explain_san(game.board(), uci) else { break };
        if game.play_uci(&e.uci).is_err() {
            break;
        }
        out.push(e.text);
    }
    out
}

/// Check a FEN typed or read from a photo: `None` if it's a legal
/// position, else what's wrong.
#[uniffi::export]
pub fn check_fen(fen: String) -> Option<String> {
    Game::new(Some(fen.trim())).err().map(|e| format!("{e:?}"))
}

/// A full FEN for a placement read from a photo (castling inferred), or
/// `None` if it can't be parsed.
#[uniffi::export]
pub fn complete_fen(placement: String, white_to_move: bool) -> Option<String> {
    let grid = chess_analysis::photo::parse_placement(&placement)?;
    Some(chess_analysis::photo::complete_fen(&grid, white_to_move))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_and_counts_help() {
        let s = GameSession::new(None, GameMode::Computer { level: 3 }).unwrap();
        assert_eq!(s.play("Nf3".into(), 1_000).unwrap().uci, "g1f3");
        let reply = s.bot_move(0, 7).unwrap();
        s.play(reply.uci, 2_000).unwrap();
        let st = s.state();
        assert_eq!(st.ply, 2);
        assert_eq!(st.turn, "white");
        assert_eq!(s.assistance(), Assistance::default());

        s.hint(20).unwrap().unwrap();
        s.undo(1);
        let help = s.assistance();
        assert_eq!((help.hints, help.takebacks), (1, 1));
        assert_eq!(s.state().ply, 1);
        assert!(s.play("e9e5".into(), 3_000).is_err());
    }

    #[test]
    fn rated_online_games_get_no_help() {
        let s = GameSession::new(None, GameMode::Online { rated: true }).unwrap();
        assert!(matches!(s.hint(10), Err(MobileError::NotAllowed { .. })));
        assert!(s.analyse_json(2, 1_000, 3, 3, 0).is_err());
        assert!(s.legal_moves_json().is_err());
        assert_eq!(s.assistance(), Assistance::default());
        let casual = GameSession::new(None, GameMode::Online { rated: false }).unwrap();
        assert!(casual.analyse_json(2, 1_000, 3, 3, 0).unwrap().contains("lines"));
    }

    #[test]
    fn offline_record() {
        let s = GameSession::new(None, GameMode::Computer { level: 4 }).unwrap();
        for (n, mv) in ["f3", "e5", "g4", "Qh4#"].into_iter().enumerate() {
            s.play(mv.into(), 1_000 * (n as u64 + 1)).unwrap();
        }
        assert_eq!(s.state().status, "mate");
        assert!(s.ratable());
        let r: serde_json::Value = serde_json::from_str(
            &s.offline_record_json("g1".into(), "Mark".into(), "ai-level-4".into(), "rapid".into(), 500)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["moves"].as_array().unwrap().len(), 4);
        assert_eq!(r["moveTimesMs"][3], 4_000);
        assert_eq!(r["botRating"], 1400);
        assert_eq!(r["winner"], "black");
        assert_eq!(r["ratable"], true);

        let p = GameSession::new(None, GameMode::PassAndPlay).unwrap();
        p.play("e4".into(), 1).unwrap();
        assert!(!p.ratable(), "not finished");
        p.resign("black".into()).unwrap();
        assert_eq!(p.state().winner.as_deref(), Some("white"));
        assert!(p.ratable());
        assert!(p.pgn("A".into(), "B".into(), "Casual".into()).contains("1. e4"));

        let f = GameSession::new(None, GameMode::PassAndPlay).unwrap();
        f.play("e4".into(), 1).unwrap();
        f.flag("black".into()).unwrap();
        assert_eq!((f.state().status.as_str(), f.state().winner.as_deref()), ("outoftime", Some("white")));
        let bare =
            GameSession::new(Some("8/8/8/4k3/8/8/8/4K3 w - - 0 1".into()), GameMode::PassAndPlay).unwrap();
        bare.flag("white".into()).unwrap();
        assert_eq!(bare.state().winner, None, "a lone king can't win on time");
        let online = GameSession::new(None, GameMode::Online { rated: false }).unwrap();
        assert!(online.offline_record_json("x".into(), "a".into(), "b".into(), "rapid".into(), 0).is_err());
    }

    #[test]
    fn describes_moves_in_words() {
        let d = describe_moves(None, "b1c3 e7e5 g1f3 nonsense".into());
        assert_eq!(d.len(), 3, "{d:?}");
        assert!(d[0].to_lowercase().contains("knight") && d[0].contains("c3"), "{d:?}");
        assert!(describe_moves(Some("bad".into()), "e2e4".into()).is_empty());
    }

    #[test]
    fn resumes_saved_games() {
        let s = GameSession::resume(
            None,
            "e2e4 e7e5".into(),
            vec![10, 20, 30],
            GameMode::PassAndPlay,
            Assistance { hints: 2, ..Assistance::default() },
        )
        .unwrap();
        assert_eq!(s.state().ply, 2);
        assert_eq!(s.assistance().hints, 2);
        assert!(check_fen(STARTPOS_FEN.into()).is_none());
        assert!(
            complete_fen("8/8/8/4k3/8/8/8/4K2R".into(), true).unwrap().starts_with("8/8/8/4k3/8/8/8/4K2R w ")
        );
    }
}
