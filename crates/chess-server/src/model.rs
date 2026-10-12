//! Rows (accounts, games, challenges) and their Lichess JSON views.

use anyhow::{Context, Result};
use chess_core::{Rating, Side, Speed, Status};
use chess_proto as p;
use sha2::{Digest, Sha256};
use tokio_postgres::GenericClient;
use tokio_postgres::Row;

use crate::db::Db;

/// Lichess sends this for an unlimited game's clock.
pub const NO_CLOCK_MS: i64 = 2_147_483_647;

// ─── accounts ───

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub player_id: i64,
    pub username: String,
    pub bot: bool,
    pub marked: bool,
    pub created_ms: i64,
    pub seen_ms: Option<i64>,
}

impl Account {
    /// Lichess user ids are the lowercase username.
    pub fn id(&self) -> String {
        self.username.to_lowercase()
    }

    pub fn title(&self) -> Option<String> {
        self.bot.then(|| "BOT".to_owned())
    }

    fn from_row(r: &Row) -> Account {
        Account {
            player_id: r.get("player_id"),
            username: r.get("username"),
            bot: r.get::<_, String>("kind") == "bot",
            marked: r.get::<_, Option<chrono::DateTime<chrono::Utc>>>("marked_at").is_some(),
            created_ms: r.get::<_, chrono::DateTime<chrono::Utc>>("created_at").timestamp_millis(),
            seen_ms: r
                .get::<_, Option<chrono::DateTime<chrono::Utc>>>("seen_at")
                .map(|t| t.timestamp_millis()),
        }
    }
}

/// A Lichess-style username from a display name: letters, digits, `_`
/// and `-`, 2–20 characters.
pub fn username_from(name: &str) -> String {
    let mut u: String = name
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    while u.starts_with(['-', '_']) {
        u.remove(0);
    }
    u.truncate(20);
    if u.len() < 2 { format!("player{u}") } else { u }
}

const ACCOUNT_COLS: &str = "player_id, username, kind, created_at, seen_at, marked_at";

pub async fn account(db: &Db, c: &impl GenericClient, player: i64) -> Result<Option<Account>> {
    Ok(c.query_opt(
        &db.sql(&format!("SELECT {ACCOUNT_COLS} FROM chess.accounts WHERE player_id = $1")),
        &[&player],
    )
    .await?
    .map(|r| Account::from_row(&r)))
}

pub async fn account_by_name(db: &Db, c: &impl GenericClient, name: &str) -> Result<Option<Account>> {
    Ok(c.query_opt(
        &db.sql(&format!("SELECT {ACCOUNT_COLS} FROM chess.accounts WHERE lower(username) = lower($1)")),
        &[&name],
    )
    .await?
    .map(|r| Account::from_row(&r)))
}

/// The player's chess account, made on first use from their site name.
pub async fn ensure_account(db: &Db, c: &impl GenericClient, player: i64) -> Result<Account> {
    if let Some(a) = account(db, c, player).await? {
        return Ok(a);
    }
    let name: String = c
        .query_one(&db.sql("SELECT name FROM site.players WHERE id = $1"), &[&player])
        .await
        .context("no such player")?
        .get(0);
    let base = username_from(&name);
    for n in 0..1000 {
        let candidate = if n == 0 { base.clone() } else { format!("{base}{n}") };
        let r = c
            .query_opt(
                &db.sql(&format!(
                    "INSERT INTO chess.accounts (player_id, username) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING RETURNING {ACCOUNT_COLS}"
                )),
                &[&player, &candidate],
            )
            .await?;
        if let Some(r) = r {
            return Ok(Account::from_row(&r));
        }
        // Lost a race for our own row, or the name is taken.
        if let Some(a) = account(db, c, player).await? {
            return Ok(a);
        }
    }
    anyhow::bail!("couldn't find a free username for {name:?}")
}

pub async fn rating(db: &Db, c: &impl GenericClient, player: i64, perf: &str) -> Result<(Rating, i32, i32)> {
    let r = c
        .query_opt(
            &db.sql(
                "SELECT rating, deviation, volatility, games, prog FROM chess.ratings WHERE player_id = $1 AND perf = $2",
            ),
            &[&player, &perf],
        )
        .await?;
    Ok(r.map_or((Rating::default(), 0, 0), |r| {
        (Rating { rating: r.get(0), deviation: r.get(1), volatility: r.get(2) }, r.get(3), r.get(4))
    }))
}

pub async fn perfs(
    db: &Db,
    c: &impl GenericClient,
    player: i64,
) -> Result<std::collections::BTreeMap<String, p::Perf>> {
    let mut out = std::collections::BTreeMap::new();
    for r in c
        .query(
            &db.sql("SELECT perf, rating, deviation, games, prog FROM chess.ratings WHERE player_id = $1"),
            &[&player],
        )
        .await?
    {
        let rating = Rating { rating: r.get(1), deviation: r.get(2), volatility: 0.09 };
        out.insert(
            r.get::<_, String>(0),
            p::Perf {
                games: r.get(3),
                rating: rating.int(),
                rd: rating.deviation.round() as i32,
                prog: r.get(4),
                prov: rating.provisional().then_some(true),
            },
        );
    }
    Ok(out)
}

pub async fn user_json(db: &Db, c: &impl GenericClient, a: &Account, public_url: &str) -> Result<p::User> {
    let count = c
        .query_one(
            &db.sql(
                "SELECT count(*) FILTER (WHERE status NOT IN (10, 20, 25, 37)),
                        count(*) FILTER (WHERE rated AND status NOT IN (10, 20, 25, 37)),
                        count(*) FILTER (WHERE winner IS NOT NULL AND ((winner = 'white') = (white_id = $1))),
                        count(*) FILTER (WHERE winner IS NOT NULL AND ((winner = 'white') <> (white_id = $1))),
                        count(*) FILTER (WHERE winner IS NULL AND status NOT IN (10, 20, 25, 37)),
                        count(*) FILTER (WHERE status IN (10, 20))
                   FROM chess.games WHERE white_id = $1 OR black_id = $1",
            ),
            &[&a.player_id],
        )
        .await?;
    Ok(p::User {
        id: a.id(),
        username: a.username.clone(),
        perfs: perfs(db, c, a.player_id).await?,
        title: a.title(),
        created_at: a.created_ms,
        seen_at: a.seen_ms,
        tos_violation: a.marked.then_some(true),
        disabled: None,
        url: Some(format!("{public_url}/@/{}", a.username)),
        count: Some(p::Count {
            all: count.get(0),
            rated: count.get(1),
            win: count.get(2),
            loss: count.get(3),
            draw: count.get(4),
            playing: count.get(5),
            bookmark: 0,
            import: 0,
            me: 0,
        }),
    })
}

// ─── games ───

#[derive(Debug, Clone)]
pub struct GameRow {
    pub id: String,
    pub white: Account,
    pub black: Account,
    pub variant: String,
    pub initial_fen: Option<String>,
    pub rated: bool,
    pub source: String,
    pub speed: Speed,
    pub rules: Vec<String>,
    pub clock_initial_ms: Option<i64>,
    pub clock_increment_ms: Option<i64>,
    pub status: Status,
    pub winner: Option<Side>,
    pub moves: String,
    pub ply: i32,
    pub fen: String,
    pub white_ms: Option<i64>,
    pub black_ms: Option<i64>,
    pub move_ms: Vec<i64>,
    pub clock_ms: Vec<i64>,
    pub draw_offer: Option<Side>,
    pub takeback_offer: Option<Side>,
    pub white_rating: i32,
    pub black_rating: i32,
    pub white_provisional: bool,
    pub black_provisional: bool,
    pub white_rating_diff: Option<i32>,
    pub black_rating_diff: Option<i32>,
    pub created_ms: i64,
    pub last_move_ms: Option<i64>,
    pub finished_ms: Option<i64>,
    /// Milliseconds since the side to move's turn began (database clock).
    pub elapsed_ms: i64,
}

pub const GAME_SELECT: &str = "SELECT g.*,
        (extract(epoch from (clock_timestamp() - g.turn_started_at)) * 1000)::bigint AS elapsed_ms,
        wa.player_id AS w_player_id, wa.username AS w_username, wa.kind AS w_kind, wa.created_at AS w_created_at,
        wa.seen_at AS w_seen_at, wa.marked_at AS w_marked_at,
        ba.player_id AS b_player_id, ba.username AS b_username, ba.kind AS b_kind, ba.created_at AS b_created_at,
        ba.seen_at AS b_seen_at, ba.marked_at AS b_marked_at
   FROM chess.games g
   JOIN chess.accounts wa ON wa.player_id = g.white_id
   JOIN chess.accounts ba ON ba.player_id = g.black_id";

fn side_opt(s: Option<&str>) -> Option<Side> {
    s.and_then(Side::from_name)
}

fn ms(t: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp_millis())
}

impl GameRow {
    pub fn from_row(r: &Row) -> GameRow {
        let acc = |p: &str| Account {
            player_id: r.get(format!("{p}_player_id").as_str()),
            username: r.get(format!("{p}_username").as_str()),
            bot: r.get::<_, String>(format!("{p}_kind").as_str()) == "bot",
            marked: r
                .get::<_, Option<chrono::DateTime<chrono::Utc>>>(format!("{p}_marked_at").as_str())
                .is_some(),
            created_ms: r
                .get::<_, chrono::DateTime<chrono::Utc>>(format!("{p}_created_at").as_str())
                .timestamp_millis(),
            seen_ms: ms(r.get(format!("{p}_seen_at").as_str())),
        };
        GameRow {
            id: r.get("id"),
            white: acc("w"),
            black: acc("b"),
            variant: r.get("variant"),
            initial_fen: r.get("initial_fen"),
            rated: r.get("rated"),
            source: r.get("source"),
            speed: Speed::from_key(r.get::<_, &str>("speed")).unwrap_or(Speed::Classical),
            rules: r.get("rules"),
            clock_initial_ms: r.get("clock_initial_ms"),
            clock_increment_ms: r.get("clock_increment_ms"),
            status: Status::from_id(r.get("status")).unwrap_or(Status::UnknownFinish),
            winner: side_opt(r.get("winner")),
            moves: r.get("moves"),
            ply: r.get("ply"),
            fen: r.get("fen"),
            white_ms: r.get("white_ms"),
            black_ms: r.get("black_ms"),
            move_ms: r.get("move_ms"),
            clock_ms: r.get("clock_ms"),
            draw_offer: side_opt(r.get("draw_offer")),
            takeback_offer: side_opt(r.get("takeback_offer")),
            white_rating: r.get("white_rating"),
            black_rating: r.get("black_rating"),
            white_provisional: r.get("white_provisional"),
            black_provisional: r.get("black_provisional"),
            white_rating_diff: r.get("white_rating_diff"),
            black_rating_diff: r.get("black_rating_diff"),
            created_ms: r.get::<_, chrono::DateTime<chrono::Utc>>("created_at").timestamp_millis(),
            last_move_ms: ms(r.get("last_move_at")),
            finished_ms: ms(r.get("finished_at")),
            elapsed_ms: r.get("elapsed_ms"),
        }
    }

    pub fn side_of(&self, player: i64) -> Option<Side> {
        if self.white.player_id == player {
            Some(Side::White)
        } else if self.black.player_id == player {
            Some(Side::Black)
        } else {
            None
        }
    }

    pub fn player(&self, side: Side) -> &Account {
        match side {
            Side::White => &self.white,
            Side::Black => &self.black,
        }
    }

    pub fn rating(&self, side: Side) -> (i32, bool, Option<i32>) {
        match side {
            Side::White => (self.white_rating, self.white_provisional, self.white_rating_diff),
            Side::Black => (self.black_rating, self.black_provisional, self.black_rating_diff),
        }
    }

    /// Side to move, from the FEN's active colour.
    pub fn turn(&self) -> Side {
        if self.fen.split_whitespace().nth(1) == Some("b") { Side::Black } else { Side::White }
    }

    pub fn has_clock(&self) -> bool {
        self.clock_initial_ms.is_some()
    }

    /// Clocks run once each side has made a first move.
    pub fn clock_running(&self) -> bool {
        self.has_clock() && self.ply >= 2 && self.status.is_ongoing()
    }

    /// Time left now (the side to move's clock is ticking).
    pub fn live_ms(&self, side: Side) -> i64 {
        if !self.has_clock() {
            return NO_CLOCK_MS;
        }
        let stored = match side {
            Side::White => self.white_ms,
            Side::Black => self.black_ms,
        }
        .unwrap_or(0);
        if self.clock_running() && self.turn() == side {
            (stored - self.elapsed_ms.max(0)).max(0)
        } else {
            stored
        }
    }

    pub fn last_move(&self) -> &str {
        self.moves.split_whitespace().next_back().unwrap_or("")
    }

    /// Has `side` moved yet (from the start of this game)?
    pub fn has_moved(&self, side: Side) -> bool {
        let first =
            chess_core::Game::new(self.initial_fen.as_deref()).map_or(Side::White, |g| g.first_to_move());
        let mine = if side == first { 1 } else { 2 };
        self.ply >= mine
    }

    pub fn variant(&self) -> p::Variant {
        if self.variant == "fromPosition" { p::Variant::from_position() } else { p::Variant::standard() }
    }

    pub fn perf_key(&self) -> &'static str {
        self.speed.key()
    }

    pub fn full_id(&self, player: i64) -> String {
        format!("{}{}", self.id, full_id_suffix(&self.id, player))
    }

    pub fn has_rule(&self, rule: &str) -> bool {
        self.rules.iter().any(|r| r == rule)
    }

    pub fn event_player(&self, side: Side) -> p::GameEventPlayer {
        let a = self.player(side);
        let (rating, prov, diff) = self.rating(side);
        p::GameEventPlayer {
            id: a.id(),
            name: a.username.clone(),
            title: a.title(),
            rating: Some(rating),
            rating_diff: diff,
            provisional: prov.then_some(true),
            ai_level: None,
        }
    }

    pub fn state(&self, first_move_ms: i64) -> p::GameState {
        let inc = self.clock_increment_ms.unwrap_or(0);
        let expiration = (self.status.is_ongoing() && self.ply < 2)
            .then_some(p::Expiration { idle_millis: self.elapsed_ms.max(0), millis_to_move: first_move_ms });
        p::GameState {
            kind: p::GameState::TYPE.into(),
            moves: self.moves.clone(),
            wtime: self.live_ms(Side::White),
            btime: self.live_ms(Side::Black),
            winc: inc,
            binc: inc,
            status: self.status,
            winner: self.winner,
            wdraw: self.draw_offer == Some(Side::White),
            bdraw: self.draw_offer == Some(Side::Black),
            wtakeback: self.takeback_offer == Some(Side::White),
            btakeback: self.takeback_offer == Some(Side::Black),
            expiration,
        }
    }

    pub fn full(&self, first_move_ms: i64) -> p::GameFull {
        p::GameFull {
            kind: p::GameFull::TYPE.into(),
            id: self.id.clone(),
            variant: self.variant(),
            clock: self
                .clock_initial_ms
                .map(|i| p::GameClock { initial: i, increment: self.clock_increment_ms.unwrap_or(0) }),
            speed: self.speed,
            perf: p::PerfName { name: self.speed.display().into() },
            rated: self.rated,
            created_at: self.created_ms,
            white: self.event_player(Side::White),
            black: self.event_player(Side::Black),
            initial_fen: self.initial_fen.clone().unwrap_or_else(|| "startpos".into()),
            state: self.state(first_move_ms),
            days_per_turn: None,
            tournament_id: None,
        }
    }

    /// gameStart / gameFinish / nowPlaying entry, from `player`'s side.
    pub fn event_info(&self, player: i64) -> Option<p::GameEventInfo> {
        let side = self.side_of(player)?;
        let opp = side.opposite();
        let (rating, _, diff) = self.rating(side);
        let (orating, _, odiff) = self.rating(opp);
        let o = self.player(opp);
        let ongoing = self.status.is_ongoing();
        Some(p::GameEventInfo {
            full_id: self.full_id(player),
            game_id: self.id.clone(),
            fen: self.fen.clone(),
            color: side,
            last_move: self.last_move().to_owned(),
            source: self.source.clone(),
            status: self.status.into(),
            variant: self.variant().brief(),
            speed: self.speed,
            perf: self.perf_key().into(),
            rated: self.rated,
            has_moved: self.has_moved(side),
            opponent: p::GameEventOpponent {
                id: o.id(),
                username: o.username.clone(),
                rating: orating,
                rating_diff: odiff,
            },
            is_my_turn: ongoing && self.turn() == side,
            seconds_left: self.has_clock().then(|| self.live_ms(side) / 1000),
            winner: self.winner,
            rating: Some(rating),
            rating_diff: diff,
            compat: p::Compat { bot: true, board: true },
            id: self.id.clone(),
        })
    }

    pub fn event(&self, player: i64, kind: &str) -> Option<serde_json::Value> {
        let game = self.event_info(player)?;
        serde_json::to_value(p::GameEvent { kind: kind.into(), game }).ok()
    }
}

/// A stable 4-character suffix for a player's full game id.
pub fn full_id_suffix(game: &str, player: i64) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let h = Sha256::digest(format!("{game}:{player}").as_bytes());
    h.iter().take(4).map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).collect()
}

pub async fn game(db: &Db, c: &impl GenericClient, id: &str) -> Result<Option<GameRow>> {
    Ok(c.query_opt(&db.sql(&format!("{GAME_SELECT} WHERE g.id = $1")), &[&id])
        .await?
        .map(|r| GameRow::from_row(&r)))
}

/// Lock the game row for an update (inside a transaction).
pub async fn game_for_update(db: &Db, c: &impl GenericClient, id: &str) -> Result<Option<GameRow>> {
    Ok(c.query_opt(&db.sql(&format!("{GAME_SELECT} WHERE g.id = $1 FOR UPDATE OF g")), &[&id])
        .await?
        .map(|r| GameRow::from_row(&r)))
}

pub async fn ongoing_games(db: &Db, c: &impl GenericClient, player: i64) -> Result<Vec<GameRow>> {
    Ok(c.query(
        &db.sql(&format!(
            "{GAME_SELECT} WHERE (g.white_id = $1 OR g.black_id = $1) AND g.status IN (10, 20) ORDER BY g.created_at"
        )),
        &[&player],
    )
    .await?
    .iter()
    .map(GameRow::from_row)
    .collect())
}

// ─── challenges ───

#[derive(Debug, Clone)]
pub struct ChallengeRow {
    pub id: String,
    pub challenger: Account,
    pub dest: Account,
    pub status: String,
    pub variant: String,
    pub initial_fen: Option<String>,
    pub rated: bool,
    pub clock_limit: Option<i32>,
    pub clock_increment: Option<i32>,
    pub color: String,
    pub final_color: Side,
    pub rules: Vec<String>,
    pub decline_reason: Option<String>,
}

pub const CHALLENGE_SELECT: &str = "SELECT c.*,
        ca.player_id AS c_player_id, ca.username AS c_username, ca.kind AS c_kind, ca.created_at AS c_created_at,
        ca.seen_at AS c_seen_at, ca.marked_at AS c_marked_at,
        da.player_id AS d_player_id, da.username AS d_username, da.kind AS d_kind, da.created_at AS d_created_at,
        da.seen_at AS d_seen_at, da.marked_at AS d_marked_at
   FROM chess.challenges c
   JOIN chess.accounts ca ON ca.player_id = c.challenger_id
   JOIN chess.accounts da ON da.player_id = c.dest_id";

impl ChallengeRow {
    pub fn from_row(r: &Row) -> ChallengeRow {
        let acc = |p: &str| Account {
            player_id: r.get(format!("{p}_player_id").as_str()),
            username: r.get(format!("{p}_username").as_str()),
            bot: r.get::<_, String>(format!("{p}_kind").as_str()) == "bot",
            marked: r
                .get::<_, Option<chrono::DateTime<chrono::Utc>>>(format!("{p}_marked_at").as_str())
                .is_some(),
            created_ms: r
                .get::<_, chrono::DateTime<chrono::Utc>>(format!("{p}_created_at").as_str())
                .timestamp_millis(),
            seen_ms: ms(r.get(format!("{p}_seen_at").as_str())),
        };
        ChallengeRow {
            id: r.get("id"),
            challenger: acc("c"),
            dest: acc("d"),
            status: r.get("status"),
            variant: r.get("variant"),
            initial_fen: r.get("initial_fen"),
            rated: r.get("rated"),
            clock_limit: r.get("clock_limit"),
            clock_increment: r.get("clock_increment"),
            color: r.get("color"),
            final_color: side_opt(r.get("final_color")).unwrap_or(Side::White),
            rules: r.get("rules"),
            decline_reason: r.get("decline_reason"),
        }
    }

    pub fn speed(&self) -> Speed {
        match (self.clock_limit, self.clock_increment) {
            (Some(l), Some(i)) => Speed::from_clock(l as u32, i as u32),
            _ => Speed::Correspondence,
        }
    }

    pub fn json(&self, public_url: &str, viewer: Option<i64>, ratings: (i32, i32)) -> p::Challenge {
        let user = |a: &Account, rating: i32| p::ChallengeUser {
            id: a.id(),
            name: a.username.clone(),
            rating: Some(rating),
            title: a.title(),
            provisional: None,
            online: None,
        };
        let speed = self.speed();
        let decline_key = self.decline_reason.clone();
        let decline_text = decline_key.as_deref().map(|k| {
            p::DECLINE_REASONS
                .iter()
                .find(|(key, _)| *key == k)
                .map_or(p::DECLINE_REASONS[0].1, |(_, t)| *t)
                .to_owned()
        });
        p::Challenge {
            id: self.id.clone(),
            url: format!("{public_url}/{}", self.id),
            status: self.status.clone(),
            challenger: user(&self.challenger, ratings.0),
            dest_user: Some(user(&self.dest, ratings.1)),
            variant: if self.variant == "fromPosition" {
                p::Variant::from_position()
            } else {
                p::Variant::standard()
            },
            rated: self.rated,
            speed,
            time_control: match (self.clock_limit, self.clock_increment) {
                (Some(l), Some(i)) => p::TimeControl::clock(l as u32, i as u32),
                _ => p::TimeControl::Unlimited,
            },
            color: self.color.clone(),
            final_color: Some(self.final_color),
            perf: p::ChallengePerf { icon: String::new(), name: speed.display().into() },
            direction: viewer
                .map(|v| if v == self.challenger.player_id { "out".into() } else { "in".into() }),
            initial_fen: self.initial_fen.clone(),
            decline_reason: decline_text,
            decline_reason_key: decline_key,
        }
    }
}

pub async fn challenge(
    db: &Db,
    c: &impl GenericClient,
    id: &str,
    lock: bool,
) -> Result<Option<ChallengeRow>> {
    let q = format!("{CHALLENGE_SELECT} WHERE c.id = $1{}", if lock { " FOR UPDATE OF c" } else { "" });
    Ok(c.query_opt(&db.sql(&q), &[&id]).await?.map(|r| ChallengeRow::from_row(&r)))
}

/// Current ratings of the challenger and destination, for the challenge's
/// perf.
pub async fn challenge_ratings(db: &Db, c: &impl GenericClient, ch: &ChallengeRow) -> Result<(i32, i32)> {
    let perf = ch.speed().key();
    let a = rating(db, c, ch.challenger.player_id, perf).await?.0.int();
    let b = rating(db, c, ch.dest.player_id, perf).await?.0.int();
    Ok((a, b))
}

/// An 8-character id like Lichess's.
pub fn new_id() -> String {
    crate::auth::random_token(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert_eq!(username_from("Mark Olliver"), "Mark-Olliver");
        assert_eq!(username_from("  Émile_Z! "), "mile_Z");
        assert_eq!(username_from("x"), "playerx");
        assert_eq!(username_from("--bob"), "bob");
        assert_eq!(username_from("a very long display name indeed").len(), 20);
    }

    #[test]
    fn full_ids_are_stable_and_distinct() {
        let a = full_id_suffix("abcd1234", 1);
        assert_eq!(a.len(), 4);
        assert_eq!(a, full_id_suffix("abcd1234", 1));
        assert_ne!(a, full_id_suffix("abcd1234", 2));
    }
}
