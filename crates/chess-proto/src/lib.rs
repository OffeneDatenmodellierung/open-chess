//! The Lichess API's JSON, as Rust types.
//!
//! Field names, optional fields and string values follow Lichess's
//! OpenAPI spec (github.com/lichess-org/api, `doc/specs/schemas`), so the
//! same types serve the open-chess server (which emits them) and
//! any client or bot (which reads them, from our server or lichess.org).
//!
//! Streamed events carry their `type` as a field rather than through a
//! serde enum tag, because `gameFull` nests a `gameState` that keeps its
//! own `type`. [`BoardEvent::parse`] and [`StreamEvent::parse`] dispatch
//! on it when reading.

use std::collections::BTreeMap;

pub use chess_core::{Side, Speed, Status};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_false(b: &bool) -> bool {
    !*b
}

// ─── shared pieces ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    pub key: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
}

impl Variant {
    pub fn standard() -> Variant {
        Variant { key: "standard".into(), name: "Standard".into(), short: Some("Std".into()) }
    }

    pub fn from_position() -> Variant {
        Variant { key: "fromPosition".into(), name: "From Position".into(), short: Some("FEN".into()) }
    }

    /// Without `short`, as gameStart / gameFinish events send it.
    #[must_use]
    pub fn brief(mut self) -> Variant {
        self.short = None;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusRef {
    pub id: i16,
    pub name: Status,
}

impl From<Status> for StatusRef {
    fn from(s: Status) -> Self {
        StatusRef { id: s.id(), name: s }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compat {
    pub bot: bool,
    pub board: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ok {
    pub ok: bool,
}

pub const OK: Ok = Ok { ok: true };

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

// ─── board / bot game stream ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventPlayer {
    pub id: String,
    pub name: String,
    /// `null` when untitled (Lichess sends the key).
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating_diff: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_level: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameClock {
    /// Milliseconds.
    pub initial: i64,
    /// Milliseconds.
    pub increment: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerfName {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expiration {
    pub idle_millis: i64,
    pub millis_to_move: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameState {
    #[serde(rename = "type")]
    pub kind: String,
    /// UCI moves, space-separated (standard castling notation).
    pub moves: String,
    pub wtime: i64,
    pub btime: i64,
    pub winc: i64,
    pub binc: i64,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<Side>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wdraw: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bdraw: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wtakeback: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub btakeback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<Expiration>,
}

impl GameState {
    pub const TYPE: &'static str = "gameState";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameFull {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub variant: Variant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<GameClock>,
    pub speed: Speed,
    pub perf: PerfName,
    pub rated: bool,
    pub created_at: i64,
    pub white: GameEventPlayer,
    pub black: GameEventPlayer,
    pub initial_fen: String,
    pub state: GameState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_per_turn: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tournament_id: Option<String>,
}

impl GameFull {
    pub const TYPE: &'static str = "gameFull";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatLine {
    #[serde(rename = "type")]
    pub kind: String,
    pub room: String,
    pub username: String,
    pub text: String,
}

impl ChatLine {
    pub const TYPE: &'static str = "chatLine";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpponentGone {
    #[serde(rename = "type")]
    pub kind: String,
    pub gone: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_win_in_seconds: Option<i64>,
}

impl OpponentGone {
    pub const TYPE: &'static str = "opponentGone";
}

/// One line of `/api/board/game/stream/{id}` or `/api/bot/game/stream/{id}`.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardEvent {
    GameFull(Box<GameFull>),
    GameState(GameState),
    ChatLine(ChatLine),
    OpponentGone(OpponentGone),
    /// A type this crate doesn't know (kept for forward compatibility).
    Other(Value),
}

impl BoardEvent {
    pub fn parse(line: &[u8]) -> serde_json::Result<BoardEvent> {
        let v: Value = serde_json::from_slice(line)?;
        Ok(match v.get("type").and_then(Value::as_str) {
            Some(GameFull::TYPE) => BoardEvent::GameFull(Box::new(serde_json::from_value(v)?)),
            Some(GameState::TYPE) => BoardEvent::GameState(serde_json::from_value(v)?),
            Some(ChatLine::TYPE) => BoardEvent::ChatLine(serde_json::from_value(v)?),
            Some(OpponentGone::TYPE) => BoardEvent::OpponentGone(serde_json::from_value(v)?),
            _ => BoardEvent::Other(v),
        })
    }
}

// ─── the event stream (/api/stream/event) ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventOpponent {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub rating: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating_diff: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventInfo {
    pub full_id: String,
    pub game_id: String,
    pub fen: String,
    pub color: Side,
    pub last_move: String,
    pub source: String,
    pub status: StatusRef,
    pub variant: Variant,
    pub speed: Speed,
    pub perf: String,
    pub rated: bool,
    pub has_moved: bool,
    pub opponent: GameEventOpponent,
    pub is_my_turn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_left: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating_diff: Option<i32>,
    pub compat: Compat,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub game: GameEventInfo,
}

impl GameEvent {
    pub const START: &'static str = "gameStart";
    pub const FINISH: &'static str = "gameFinish";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub challenge: Challenge,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}

impl ChallengeEvent {
    pub const CREATED: &'static str = "challenge";
    pub const CANCELED: &'static str = "challengeCanceled";
    pub const DECLINED: &'static str = "challengeDeclined";
}

/// One line of `/api/stream/event`.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    GameStart(Box<GameEventInfo>),
    GameFinish(Box<GameEventInfo>),
    Challenge(Box<Challenge>),
    ChallengeCanceled(Box<Challenge>),
    ChallengeDeclined(Box<Challenge>),
    Other(Value),
}

impl StreamEvent {
    pub fn parse(line: &[u8]) -> serde_json::Result<StreamEvent> {
        let v: Value = serde_json::from_slice(line)?;
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
        let game = |v: Value| -> serde_json::Result<Box<GameEventInfo>> {
            Ok(Box::new(serde_json::from_value::<GameEvent>(v)?.game))
        };
        let challenge = |v: Value| -> serde_json::Result<Box<Challenge>> {
            Ok(Box::new(serde_json::from_value::<ChallengeEvent>(v)?.challenge))
        };
        Ok(match kind.as_str() {
            GameEvent::START => StreamEvent::GameStart(game(v)?),
            GameEvent::FINISH => StreamEvent::GameFinish(game(v)?),
            ChallengeEvent::CREATED => StreamEvent::Challenge(challenge(v)?),
            ChallengeEvent::CANCELED => StreamEvent::ChallengeCanceled(challenge(v)?),
            ChallengeEvent::DECLINED => StreamEvent::ChallengeDeclined(challenge(v)?),
            _ => StreamEvent::Other(v),
        })
    }
}

// ─── challenges ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeUser {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TimeControl {
    Clock {
        limit: u32,
        increment: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        show: Option<String>,
    },
    Correspondence {
        #[serde(rename = "daysPerTurn")]
        days_per_turn: u32,
    },
    Unlimited,
}

impl TimeControl {
    /// `3+2`, `1/4+0` style, as Lichess shows it.
    pub fn clock(limit: u32, increment: u32) -> TimeControl {
        let mins = match limit {
            15 => "1/4".to_owned(),
            30 => "1/2".to_owned(),
            45 => "3/4".to_owned(),
            90 => "1.5".to_owned(),
            l => (l / 60).to_string(),
        };
        TimeControl::Clock { limit, increment, show: Some(format!("{mins}+{increment}")) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengePerf {
    pub icon: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub id: String,
    pub url: String,
    /// created | offline | canceled | declined | accepted
    pub status: String,
    pub challenger: ChallengeUser,
    pub dest_user: Option<ChallengeUser>,
    pub variant: Variant,
    pub rated: bool,
    pub speed: Speed,
    pub time_control: TimeControl,
    /// white | black | random
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_color: Option<Side>,
    pub perf: ChallengePerf,
    /// in | out, relative to whoever is asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_fen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline_reason_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeList {
    #[serde(rename = "in")]
    pub incoming: Vec<Challenge>,
    #[serde(rename = "out")]
    pub outgoing: Vec<Challenge>,
}

/// Lichess's decline reasons: (key, text).
pub const DECLINE_REASONS: &[(&str, &str)] = &[
    ("generic", "I'm not accepting challenges at the moment."),
    ("later", "This is not the right time for me, please ask again later."),
    ("toofast", "This time control is too fast for me, please challenge again with a slower game."),
    ("tooslow", "This time control is too slow for me, please challenge again with a faster game."),
    ("timecontrol", "I'm not accepting challenges with this time control."),
    ("rated", "Please send me a rated challenge instead."),
    ("casual", "Please send me a casual challenge instead."),
    ("standard", "I'm not accepting variant challenges right now."),
    ("variant", "I'm not willing to play this variant right now."),
    ("nobot", "I'm not accepting challenges from bots."),
    ("onlybot", "I'm only accepting challenges from bots."),
];

// ─── users ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Perf {
    pub games: i32,
    pub rating: i32,
    pub rd: i32,
    pub prog: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prov: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    pub all: i64,
    pub rated: i64,
    pub win: i64,
    pub loss: i64,
    pub draw: i64,
    pub playing: i64,
    #[serde(default)]
    pub bookmark: i64,
    #[serde(default)]
    pub import: i64,
    /// Games against the asker.
    #[serde(default)]
    pub me: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub perfs: BTreeMap<String, Perf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tos_violation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Count>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Email {
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub now_playing: Vec<GameEventInfo>,
    pub nb_my_turn: i64,
}

// ─── OAuth ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token_type: String,
    pub access_token: String,
    pub expires_in: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthError {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub user_id: String,
    /// Comma-separated.
    pub scopes: String,
    /// Unix milliseconds, or null for no expiry.
    pub expires: Option<i64>,
}

// ─── game export ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightUser {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerAnalysis {
    pub inaccuracy: i32,
    pub mistake: i32,
    pub blunder: i32,
    pub acpl: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePlayer {
    pub user: LightUser,
    pub rating: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating_diff: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<PlayerAnalysis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GamePlayers {
    pub white: GamePlayer,
    pub black: GamePlayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportClock {
    /// Seconds.
    pub initial: i64,
    /// Seconds.
    pub increment: i64,
    /// Seconds: initial + 40 × increment.
    pub total_time: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameJson {
    pub id: String,
    pub rated: bool,
    pub variant: String,
    pub speed: Speed,
    pub perf: String,
    pub created_at: i64,
    pub last_move_at: i64,
    pub status: Status,
    pub source: String,
    pub players: GamePlayers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_fen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<Side>,
    /// SAN, space-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moves: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ExportClock>,
    /// Clock after each ply, centiseconds (with `clocks=true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clocks: Option<Vec<i64>>,
}

/// `/api/stream/game/{id}` move line: FEN after the move, the move, and
/// clocks in seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveLine {
    pub fen: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lm: Option<String>,
    pub wc: i64,
    pub bc: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub text: String,
    pub user: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> GameState {
        GameState {
            kind: GameState::TYPE.into(),
            moves: "e2e4".into(),
            wtime: 180_000,
            btime: 180_000,
            winc: 2000,
            binc: 2000,
            status: Status::Started,
            winner: None,
            wdraw: false,
            bdraw: true,
            wtakeback: false,
            btakeback: false,
            expiration: None,
        }
    }

    #[test]
    fn game_state_omits_false_flags() {
        let v = serde_json::to_value(state()).unwrap();
        assert_eq!(
            v,
            json!({"type":"gameState","moves":"e2e4","wtime":180_000,"btime":180_000,"winc":2000,"binc":2000,
                   "status":"started","bdraw":true})
        );
    }

    #[test]
    fn board_events_parse_lichess_examples() {
        // From lichess-org/api doc/specs/examples.
        let full = br#"{"id":"xcoxhDvh","variant":{"key":"standard","name":"Standard","short":"Std"},
            "speed":"blitz","perf":{"name":"Blitz"},"rated":false,"createdAt":1789845859939,
            "white":{"id":"kenneth","name":"Kenneth","title":null,"rating":804},
            "black":{"id":"salma","name":"Salma","title":null,"rating":714},"initialFen":"startpos",
            "clock":{"initial":300000,"increment":0},"type":"gameFull",
            "state":{"type":"gameState","moves":"","wtime":300000,"btime":300000,"winc":0,"binc":0,"status":"started"}}"#;
        match BoardEvent::parse(full).unwrap() {
            BoardEvent::GameFull(g) => {
                assert_eq!(g.id, "xcoxhDvh");
                assert_eq!(g.white.title, None);
                assert_eq!(g.state.status, Status::Started);
                // Round trip keeps `title: null` (Lichess sends the key).
                let back = serde_json::to_value(&*g).unwrap();
                assert_eq!(back["white"]["title"], Value::Null);
                assert!(back["white"].as_object().unwrap().contains_key("title"));
            }
            other => panic!("{other:?}"),
        }
        let gone = br#"{"type":"opponentGone","gone":true,"claimWinInSeconds":8}"#;
        assert!(matches!(
            BoardEvent::parse(gone).unwrap(),
            BoardEvent::OpponentGone(OpponentGone { gone: true, claim_win_in_seconds: Some(8), .. })
        ));
        assert!(matches!(BoardEvent::parse(br#"{"type":"future"}"#).unwrap(), BoardEvent::Other(_)));
    }

    #[test]
    fn stream_events_parse_lichess_examples() {
        let start = br#"{"type":"gameStart","game":{"fullId":"wtRWmfWCSU1E","gameId":"wtRWmfWC",
            "fen":"rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1","color":"white","lastMove":"",
            "source":"friend","status":{"id":20,"name":"started"},"variant":{"key":"standard","name":"Standard"},
            "speed":"blitz","perf":"blitz","rated":true,"hasMoved":false,
            "opponent":{"id":"aaron","username":"Aaron","rating":734},"isMyTurn":true,"secondsLeft":300,
            "rating":695,"compat":{"bot":false,"board":true},"id":"wtRWmfWC"}}"#;
        match StreamEvent::parse(start).unwrap() {
            StreamEvent::GameStart(g) => {
                assert_eq!(g.game_id, "wtRWmfWC");
                assert_eq!(g.status, StatusRef { id: 20, name: Status::Started });
                assert!(g.is_my_turn);
            }
            other => panic!("{other:?}"),
        }
        let ch = br#"{"type":"challenge","challenge":{"id":"C8fNpisS","url":"https://lichess.org/C8fNpisS",
            "status":"created","challenger":{"name":"Bobby","id":"bobby","rating":1657},
            "destUser":{"name":"Mary","id":"mary","rating":1079},
            "variant":{"key":"standard","name":"Standard","short":"Std"},"rated":false,"speed":"correspondence",
            "timeControl":{"type":"unlimited"},"color":"random","finalColor":"black",
            "perf":{"icon":"","name":"Correspondence"},"direction":"in"}}"#;
        match StreamEvent::parse(ch).unwrap() {
            StreamEvent::Challenge(c) => {
                assert_eq!(c.time_control, TimeControl::Unlimited);
                assert_eq!(c.final_color, Some(Side::Black));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn time_control_shape() {
        let v = serde_json::to_value(TimeControl::clock(180, 2)).unwrap();
        assert_eq!(v, json!({"type":"clock","limit":180,"increment":2,"show":"3+2"}));
        let v = serde_json::to_value(TimeControl::clock(15, 0)).unwrap();
        assert_eq!(v["show"], "1/4+0");
    }

    #[test]
    fn status_ref_from_status() {
        let s: StatusRef = Status::Resign.into();
        assert_eq!(serde_json::to_value(s).unwrap(), json!({"id":31,"name":"resign"}));
    }
}
