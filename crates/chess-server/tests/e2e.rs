//! End-to-end tests against a real Postgres: two server replicas sharing
//! one database, driven through the Lichess API with the chess client.
//!
//! Set `CHESS_TEST_DATABASE_URL` (a database you can create schemas in) to
//! run them; without it they skip. Each test makes its own pair of schemas
//! and drops them afterwards.
//!
//!   CHESS_TEST_DATABASE_URL=postgres://dev:dev@127.0.0.1/chessdev?sslmode=disable \
//!     cargo test -p open-chess-server --test e2e

#![allow(clippy::many_single_char_names)] // short names for players keep the scenarios readable

use std::time::Duration;

use chess_client::proto::{BoardEvent, Side, Status, StreamEvent};
use chess_client::{Api, ChallengeRequest, Client, Ndjson};
use open_chess_server::app::{self, Ctx};
use open_chess_server::auth::{hash_key, random_token};
use open_chess_server::config::{AnalyseConfig, Config};
use open_chess_server::db::Db;
use tokio::sync::watch;

struct Env {
    site: String,
    chess: String,
    /// Two replicas on the same database.
    base: [String; 2],
    pg: tokio_postgres::Client,
    stop: watch::Sender<bool>,
    cfg: Config,
}

const SITE_SQL: &str = "
CREATE TABLE {s}.players (
    id bigserial PRIMARY KEY, name text NOT NULL, email text, key_hash text,
    role text NOT NULL DEFAULT 'player', roles text[] NOT NULL DEFAULT '{}', disabled_at timestamptz);
CREATE TABLE {s}.sessions (
    id bigserial PRIMARY KEY, token_hash text NOT NULL UNIQUE, player_id bigint NOT NULL REFERENCES {s}.players,
    created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL DEFAULT now(), user_agent text, revoked_at timestamptz);
CREATE TABLE {s}.api_tokens (
    id bigserial PRIMARY KEY, player_id bigint NOT NULL REFERENCES {s}.players ON DELETE CASCADE,
    name text NOT NULL, token_hash text NOT NULL UNIQUE, prefix text NOT NULL, scopes text[] NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL,
    last_used_at timestamptz, revoked_at timestamptz, revoked_by text);
";

/// The secret shared with the (pretend) host site.
const SECRET: &str = "a-secret-shared-with-the-site-0123456789";

async fn setup(tweak: impl FnOnce(&mut Config)) -> Option<Env> {
    setup_with("", tweak).await
}

/// `pre` runs before the server first migrates (`{site}` and `{chess}` are
/// the schemas), to set up what an older deployment had.
async fn setup_with(pre: &str, tweak: impl FnOnce(&mut Config)) -> Option<Env> {
    let Ok(url) = std::env::var("CHESS_TEST_DATABASE_URL") else {
        eprintln!("CHESS_TEST_DATABASE_URL not set: skipped");
        return None;
    };
    let (pg, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.expect("test database");
    tokio::spawn(conn);
    let tag = random_token(6).to_lowercase();
    let site = format!("t_site_{tag}");
    let chess = format!("t_chess_{tag}");
    pg.batch_execute(&format!("CREATE SCHEMA {site}; {}", SITE_SQL.replace("{s}", &site))).await.unwrap();
    if !pre.is_empty() {
        pg.batch_execute(&pre.replace("{site}", &site).replace("{chess}", &chess)).await.unwrap();
    }
    let mut cfg = Config {
        database_url: url,
        listen_database_url: None,
        site_schema: site.clone(),
        schema: chess.clone(),
        pool_size: 6,
        public_url: "http://chess.test".into(),
        site_url: "http://site.test".into(),
        esports_login: true,
        esports_secret: Some(SECRET.into()),
        signup: true,
        site_name: "open-chess".into(),
        host_name: Some("esports-lab".into()),
        admin_emails: vec!["boss@site.test".into()],
        staff_passkey_hours: 12,
        bind: "127.0.0.1:0".into(),
        metrics_bind: "127.0.0.1:0".into(),
        lag_comp_ms: 100,
        challenge_ttl_secs: 20,
        first_move_secs: 30,
        gone_after_secs: 2,
        claim_win_secs: 1,
        public_delay_plies: 3,
        fairplay: true,
        block_marked: true,
        sweep_ms: 200,
        vision_endpoint: String::new(),
        vision_model: "test".into(),
        vision_api_key: None,
        vision_local_model: String::new(),
        vision_min_confidence: 0.5,
        vision_review: true,
        vision_review_max: 5000,
        app_dir: None,
        app_theme: None,
        tablebase_url: String::new(),
        apns_key: None,
        apns_key_id: None,
        apns_team_id: None,
        apns_topic: "org.example.test".into(),
    };
    tweak(&mut cfg);
    let (stop, rx) = watch::channel(false);
    let mut base = [String::new(), String::new()];
    for b in &mut base {
        let db = Db::connect(&cfg).unwrap();
        db.migrate().await.unwrap();
        let ctx = Ctx::new(db, cfg.clone()).with_reader().unwrap();
        open_chess_server::spawn_background(&ctx, &rx);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        *b = format!("http://{}", l.local_addr().unwrap());
        let router = app::router(&ctx).unwrap();
        tokio::spawn(async move { axum::serve(l, router).await });
    }
    // Let both LISTEN connections come up.
    tokio::time::sleep(Duration::from_millis(400)).await;
    Some(Env { site, chess, base, pg, stop, cfg })
}

impl Env {
    /// A player with an API token carrying `scopes`.
    async fn player(&self, name: &str, scopes: &[&str]) -> (i64, String) {
        let id: i64 = self
            .pg
            .query_one(
                &format!("INSERT INTO {}.players (name) VALUES ($1) RETURNING id", self.chess),
                &[&name],
            )
            .await
            .unwrap()
            .get(0);
        let token = format!("ocp_{}", random_token(36));
        let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        self.pg
            .execute(
                &format!(
                    "INSERT INTO {}.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
                     VALUES ($1, 'test', $2, 'ocp_xx', $3, now() + interval '1 day')",
                    self.chess
                ),
                &[&id, &hash_key(&token), &scopes],
            )
            .await
            .unwrap();
        (id, token)
    }

    /// Make a player a manager (or admin) here, with their tokens' passkey
    /// check done.
    async fn staff(&self, id: i64, admin: bool) {
        let set = if admin { "role = 'admin'" } else { "roles = '{manager}'" };
        self.pg
            .execute(&format!("UPDATE {}.players SET {set} WHERE id = $1", self.chess), &[&id])
            .await
            .unwrap();
        self.pg
            .execute(
                &format!(
                    "UPDATE {}.api_tokens SET staff_verified_at = now() WHERE player_id = $1",
                    self.chess
                ),
                &[&id],
            )
            .await
            .unwrap();
    }

    fn client(&self, replica: usize, token: &str) -> Client {
        Client::new(&self.base[replica], Some(token)).unwrap()
    }

    async fn teardown(self) {
        let _ = self.stop.send(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.pg
            .batch_execute(&format!("DROP SCHEMA {} CASCADE; DROP SCHEMA {} CASCADE;", self.chess, self.site))
            .await
            .unwrap();
    }
}

const HUMAN: &[&str] = &["board:play", "challenge:read", "challenge:write", "email:read"];
const BOT: &[&str] = &["bot:play", "challenge:read", "challenge:write"];

async fn next_board(s: &mut Ndjson<BoardEvent>) -> BoardEvent {
    tokio::time::timeout(Duration::from_secs(10), s.next())
        .await
        .expect("stream timed out")
        .expect("stream ended")
        .unwrap()
}

async fn next_event(s: &mut Ndjson<StreamEvent>) -> StreamEvent {
    tokio::time::timeout(Duration::from_secs(10), s.next())
        .await
        .expect("stream timed out")
        .expect("stream ended")
        .unwrap()
}

/// Read game states until one satisfies `f`.
async fn until_state(
    s: &mut Ndjson<BoardEvent>,
    f: impl Fn(&chess_client::proto::GameState) -> bool,
) -> chess_client::proto::GameState {
    loop {
        match next_board(s).await {
            BoardEvent::GameState(g) if f(&g) => return g,
            BoardEvent::GameFull(g) if f(&g.state) => return g.state,
            _ => {}
        }
    }
}

/// Start a game the way a host site's Lichess backend does: the home
/// player challenges with the away player's token in `acceptByToken`.
async fn start(
    env: &Env,
    home: &str,
    away_name: &str,
    away_token: &str,
    clock: Option<(u32, u32)>,
) -> String {
    let c = env.client(0, home);
    let ch = c
        .challenge(
            away_name,
            &ChallengeRequest {
                limit_secs: clock.map(|c| c.0),
                increment_secs: clock.map(|c| c.1),
                accept_by_token: Some(away_token.to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(ch.status, "accepted");
    ch.id
}

/// (white token, black token) for a game.
async fn colours(env: &Env, id: &str, a: (&str, &str), b: (&str, &str)) -> (String, String) {
    let g = env.client(0, a.1).export_game(id).await.unwrap();
    if g.players.white.user.name == a.0 {
        (a.1.to_owned(), b.1.to_owned())
    } else {
        (b.1.to_owned(), a.1.to_owned())
    }
}

#[tokio::test]
async fn site_flow_across_replicas() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, alice) = env.player("Alice", HUMAN).await;
    let (_, bob) = env.player("Bob", HUMAN).await;
    let me = env.client(0, &alice).account().await.unwrap();
    assert_eq!((me.id.as_str(), me.username.as_str()), ("alice", "Alice"));
    env.client(1, &bob).account().await.unwrap();

    let mut events = env.client(1, &alice).stream_events().await.unwrap();
    let id = start(&env, &alice, "bob", &bob, Some((180, 2))).await;
    match next_event(&mut events).await {
        StreamEvent::GameStart(g) => assert_eq!(g.game_id, id),
        other => panic!("{other:?}"),
    }

    // Stream on replica 1, play on replica 0.
    let (white, black) = colours(&env, &id, ("Alice", &alice), ("Bob", &bob)).await;
    let mut stream = env.client(1, &white).stream_game(Api::Board, &id).await.unwrap();
    match next_board(&mut stream).await {
        BoardEvent::GameFull(g) => {
            assert_eq!(g.id, id);
            assert_eq!(g.initial_fen, "startpos");
            assert_eq!(g.clock.unwrap().initial, 180_000);
            assert_eq!(g.state.status, Status::Started);
        }
        other => panic!("{other:?}"),
    }
    let w = env.client(0, &white);
    let b = env.client(0, &black);
    assert!(b.make_move(Api::Board, &id, "e7e5", false).await.is_err(), "not black's turn");
    w.make_move(Api::Board, &id, "f2f3", false).await.unwrap();
    b.make_move(Api::Board, &id, "e7e5", false).await.unwrap();
    w.make_move(Api::Board, &id, "g2g4", false).await.unwrap();
    assert!(b.make_move(Api::Board, &id, "d8d1", false).await.is_err(), "illegal");
    b.make_move(Api::Board, &id, "d8h4", false).await.unwrap();
    let end = until_state(&mut stream, |s| s.status != Status::Started).await;
    assert_eq!(
        (end.status, end.winner, end.moves.as_str()),
        (Status::Mate, Some(Side::Black), "f2f3 e7e5 g2g4 d8h4")
    );

    match next_event(&mut events).await {
        StreamEvent::GameFinish(g) => {
            assert_eq!((g.game_id.as_str(), g.winner), (id.as_str(), Some(Side::Black)));
        }
        other => panic!("{other:?}"),
    }
    // What a host site reads when a game ends.
    let g = w.export_game(&id).await.unwrap();
    assert_eq!((g.status, g.winner), (Status::Mate, Some(Side::Black)));
    assert_eq!(g.moves.as_deref(), Some("f3 e5 g4 Qh4#"));
    assert!(g.pgn.unwrap().contains("1. f3 { [%clk 0:03:00] } 1... e5"));
    env.teardown().await;
}

#[tokio::test]
async fn bots_accept_challenges_and_apis_stay_separate() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, carol) = env.player("Carol", HUMAN).await;
    let (_, bot) = env.player("Botty", BOT).await;
    let botc = env.client(1, &bot);
    botc.upgrade_to_bot().await.unwrap();
    assert_eq!(botc.account().await.unwrap().title.as_deref(), Some("BOT"));
    let mut bot_events = botc.stream_events().await.unwrap();

    let ch = env
        .client(0, &carol)
        .challenge(
            "botty",
            &ChallengeRequest {
                rated: true,
                limit_secs: Some(60),
                increment_secs: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(ch.status, "created");
    let StreamEvent::Challenge(c) = next_event(&mut bot_events).await else { panic!("challenge expected") };
    assert_eq!((c.id.as_str(), c.challenger.id.as_str(), c.rated), (ch.id.as_str(), "carol", true));
    botc.accept_challenge(&c.id).await.unwrap();
    let StreamEvent::GameStart(g) = next_event(&mut bot_events).await else { panic!("gameStart expected") };
    let id = g.game_id;

    // Bots use the Bot API, people the Board API.
    assert!(botc.stream_game(Api::Board, &id).await.is_err());
    assert!(env.client(0, &carol).stream_game(Api::Bot, &id).await.is_err());
    let mut s = botc.stream_game(Api::Bot, &id).await.unwrap();
    let BoardEvent::GameFull(full) = next_board(&mut s).await else { panic!() };
    let bot_side = if full.white.id == "botty" { Api::Bot } else { Api::Board };
    let (first, second) = if bot_side == Api::Bot { (&bot, &carol) } else { (&carol, &bot) };
    let api_of = |t: &String| if *t == bot { Api::Bot } else { Api::Board };
    env.client(0, first).make_move(api_of(first), &id, "e2e4", false).await.unwrap();
    env.client(1, second).make_move(api_of(second), &id, "e7e5", false).await.unwrap();
    env.client(0, &carol).resign(Api::Board, &id).await.unwrap();
    let end = until_state(&mut s, |st| st.status != Status::Started).await;
    assert_eq!(end.status, Status::Resign);
    let carol_user = env.client(0, &carol).account().await.unwrap();
    let bullet = &carol_user.perfs["bullet"];
    assert_eq!(bullet.games, 1);
    assert!(bullet.rating < 1500, "a rated loss: {bullet:?}");
    assert_eq!(carol_user.count.unwrap().loss, 1);
    env.teardown().await;
}

#[tokio::test]
async fn clocks_flag_and_unplayed_games_abort() {
    let Some(env) = setup(|c| c.first_move_secs = 1).await else { return };
    let (_, a) = env.player("Ann", HUMAN).await;
    let (_, b) = env.player("Ben", HUMAN).await;
    env.client(0, &b).account().await.unwrap();
    env.client(0, &a).account().await.unwrap();

    // 0+1: both first moves are free, then white has one second.
    let id = start(&env, &a, "ben", &b, Some((0, 1))).await;
    let (white, black) = colours(&env, &id, ("Ann", &a), ("Ben", &b)).await;
    let mut s = env.client(1, &black).stream_game(Api::Board, &id).await.unwrap();
    env.client(0, &white).make_move(Api::Board, &id, "e2e4", false).await.unwrap();
    env.client(0, &black).make_move(Api::Board, &id, "e7e5", false).await.unwrap();
    let end = until_state(&mut s, |st| st.status != Status::Started).await;
    assert_eq!((end.status, end.winner), (Status::Outoftime, Some(Side::Black)));
    assert_eq!(end.wtime, 0);

    // Nobody moves: aborted once the first-move time runs out.
    let id = start(&env, &a, "ben", &b, Some((60, 0))).await;
    let mut s = env.client(1, &a).stream_game(Api::Board, &id).await.unwrap();
    let BoardEvent::GameFull(g) = next_board(&mut s).await else { panic!() };
    assert_eq!(g.state.expiration.unwrap().millis_to_move, 1000);
    let end = until_state(&mut s, |st| st.status != Status::Started).await;
    assert_eq!((end.status, end.winner), (Status::Aborted, None));
    env.teardown().await;
}

/// Two accounts on one board (the device's "two players here"): one
/// challenges the other, accepted at once with the other's token; each side
/// moves as its own account; a rated finish changes both ratings.
#[tokio::test]
async fn two_accounts_play_on_one_board() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, ann_token) = env.player("Ann", HUMAN).await;
    let (_, bob_token) = env.player("Bob", HUMAN).await;
    let (ann, bob) = (env.client(0, &ann_token), env.client(1, &bob_token));
    let them = bob.account().await.unwrap();
    let req = ChallengeRequest {
        rated: true,
        limit_secs: Some(600),
        increment_secs: Some(5),
        color: Some("white".into()),
        accept_by_token: Some(bob_token.clone()),
        ..Default::default()
    };
    let id = ann.challenge(&them.id, &req).await.unwrap().id;
    ann.make_move(Api::Board, &id, "f2f3", false).await.unwrap();
    bob.make_move(Api::Board, &id, "e7e5", false).await.unwrap();
    ann.make_move(Api::Board, &id, "g2g4", false).await.unwrap();
    bob.make_move(Api::Board, &id, "d8h4", false).await.unwrap();
    let mut g = ann.export_game(&id).await.unwrap();
    for _ in 0..40 {
        if g.players.white.rating_diff.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        g = ann.export_game(&id).await.unwrap();
    }
    assert_eq!((g.status, g.winner), (Status::Mate, Some(Side::Black)));
    let (w, b) = (g.players.white.rating_diff.unwrap(), g.players.black.rating_diff.unwrap());
    assert!(w < 0 && b > 0, "{w} {b}");
    assert_eq!(g.players.black.user.name, "Bob");
    // The type-ahead list finds them by the start of their name.
    let http = reqwest::Client::new();
    let found: serde_json::Value = http
        .get(format!("{}/api/player/autocomplete?term=bo&object=true", env.base[0]))
        .bearer_auth(&ann_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found["result"][0]["name"].as_str(), Some("Bob"), "{found}");
    let names: Vec<String> = http
        .get(format!("{}/api/player/autocomplete?term=nn", env.base[0]))
        .bearer_auth(&ann_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(names, ["Ann"]);
    env.teardown().await;
}

#[tokio::test]
async fn the_computer_plays_at_a_level() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, token) = env.player("Fay", &["board:play"]).await;
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{}/api/challenge/ai", env.base[0]))
        .bearer_auth(&token)
        .form(&[("level", "2"), ("color", "white"), ("clock.limit", "300"), ("clock.increment", "3")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let g: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        (g["player"].as_str(), g["source"].as_str(), g["aiLevel"].as_i64()),
        (Some("white"), Some("ai"), Some(2))
    );
    let id = g["id"].as_str().unwrap().to_owned();
    let me = env.client(0, &token);
    me.make_move(Api::Board, &id, "e2e4", false).await.unwrap();
    // The computer answers within a few seconds.
    let mut moves = String::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        moves = me.export_game(&id).await.unwrap().moves.unwrap_or_default();
        if moves.split(' ').count() >= 2 {
            break;
        }
    }
    assert_eq!(moves.split(' ').count(), 2, "the computer replied: {moves}");
    // Its account has the level's fixed rating; bad levels are refused.
    let u: serde_json::Value = http
        .get(format!("{}/api/user/ai-level-2", env.base[0]))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(u["perfs"]["rapid"]["rating"].as_i64(), Some(1000));
    let r = http
        .post(format!("{}/api/challenge/ai", env.base[0]))
        .bearer_auth(&token)
        .form(&[("level", "9")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    // The client's call (the device's "the site's computer"), and the game
    // among those in progress (the device's "resume").
    let req = ChallengeRequest {
        color: Some("black".into()),
        limit_secs: Some(600),
        increment_secs: Some(5),
        ..Default::default()
    };
    let second = me.challenge_ai(3, &req).await.unwrap();
    let playing = me.playing().await.unwrap().now_playing;
    let g = playing.iter().find(|g| g.game_id == second).expect("listed as in progress");
    assert_eq!((g.color, g.opponent.username.as_str()), (Side::Black, "ai-level-3"));
    env.teardown().await;
}

#[tokio::test]
async fn rated_games_against_the_computer_move_only_the_player() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, token) = env.player("Gil", &["board:play"]).await;
    let http = reqwest::Client::new();
    let g: serde_json::Value = http
        .post(format!("{}/api/challenge/ai", env.base[0]))
        .bearer_auth(&token)
        .form(&[
            ("level", "4"),
            ("color", "white"),
            ("clock.limit", "600"),
            ("clock.increment", "5"),
            ("rated", "true"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = g["id"].as_str().unwrap().to_owned();
    let me = env.client(0, &token);
    me.make_move(Api::Board, &id, "e2e4", false).await.unwrap();
    // Both sides move before a resignation counts (sooner, it's an abort).
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if me.export_game(&id).await.unwrap().moves.unwrap_or_default().split(' ').count() >= 2 {
            break;
        }
    }
    me.resign(Api::Board, &id).await.unwrap();
    let mut game = me.export_game(&id).await.unwrap();
    for _ in 0..40 {
        if game.players.white.rating_diff.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        game = me.export_game(&id).await.unwrap();
    }
    assert!(game.rated, "the game was rated");
    assert!(
        game.players.white.rating_diff.is_some_and(|d| d < 0),
        "the player lost points: {:?}",
        game.players.white
    );
    let rapid = |u: &serde_json::Value| u["perfs"]["rapid"]["rating"].as_i64();
    let ai: serde_json::Value = http
        .get(format!("{}/api/user/ai-level-4", env.base[0]))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rapid(&ai), Some(1400), "the computer keeps its level's rating");
    let gil: serde_json::Value = http
        .get(format!("{}/api/user/gil", env.base[0]))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rapid(&gil).is_some_and(|r| r < 1500), "{gil}");
    env.teardown().await;
}

#[tokio::test]
async fn draws_takebacks_and_claims() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, a) = env.player("Ann", HUMAN).await;
    let (_, b) = env.player("Ben", HUMAN).await;
    env.client(0, &b).account().await.unwrap();

    // Draw by agreement: an offer with a move, accepted.
    let id = start(&env, &a, "ben", &b, Some((300, 0))).await;
    let (w, bl) = colours(&env, &id, ("Ann", &a), ("Ben", &b)).await;
    let mut s = env.client(1, &w).stream_game(Api::Board, &id).await.unwrap();
    env.client(0, &w).make_move(Api::Board, &id, "e2e4", true).await.unwrap();
    let st = until_state(&mut s, |st| st.wdraw).await;
    assert_eq!(st.moves, "e2e4");
    env.client(0, &bl).draw(Api::Board, &id, true).await.unwrap();
    let end = until_state(&mut s, |st| st.status != Status::Started).await;
    assert_eq!((end.status, end.winner), (Status::Draw, None));

    // Takeback on the proposer's turn undoes two plies.
    let id = start(&env, &a, "ben", &b, Some((300, 0))).await;
    let (w, bl) = colours(&env, &id, ("Ann", &a), ("Ben", &b)).await;
    let mut s = env.client(1, &bl).stream_game(Api::Board, &id).await.unwrap();
    // The first two moves are free; the next two cost time.
    env.client(0, &w).make_move(Api::Board, &id, "d2d4", false).await.unwrap();
    env.client(0, &bl).make_move(Api::Board, &id, "d7d5", false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    env.client(0, &w).make_move(Api::Board, &id, "c2c4", false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    env.client(0, &bl).make_move(Api::Board, &id, "e7e6", false).await.unwrap();
    let st = until_state(&mut s, |st| st.moves.ends_with("e7e6")).await;
    assert!(st.btime < 300_000 - 300, "black was charged: {}", st.btime);
    env.client(0, &w).takeback(Api::Board, &id, true).await.unwrap();
    until_state(&mut s, |st| st.wtakeback).await;
    env.client(0, &bl).takeback(Api::Board, &id, true).await.unwrap();
    let st = until_state(&mut s, |st| st.moves == "d2d4 d7d5").await;
    assert!(!st.wtakeback && st.status == Status::Started);
    // The stored clocks, not the stream's live ones (white's is ticking):
    // both sides get back the time of the undone moves.
    let row = env
        .pg
        .query_one(&format!("SELECT white_ms, black_ms FROM {}.games WHERE id = $1", env.chess), &[&id])
        .await
        .unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (300_000, 300_000));

    // The opponent never connects: gone, then the win can be claimed.
    env.client(0, &w).make_move(Api::Board, &id, "e2e4", false).await.unwrap();
    env.client(0, &bl).make_move(Api::Board, &id, "e7e5", false).await.unwrap();
    drop(s);
    let mut s = env.client(1, &w).stream_game(Api::Board, &id).await.unwrap();
    assert!(env.client(0, &w).claim_victory(Api::Board, &id).await.is_err(), "too soon");
    let mut claimable = false;
    for _ in 0..20 {
        if let BoardEvent::OpponentGone(g) = next_board(&mut s).await
            && g.claim_win_in_seconds == Some(0)
        {
            claimable = true;
            break;
        }
    }
    assert!(claimable);
    env.client(0, &w).claim_victory(Api::Board, &id).await.unwrap();
    let g = env.client(0, &w).export_game(&id).await.unwrap();
    assert_eq!(g.status, Status::Timeout);
    env.teardown().await;
}

#[tokio::test]
async fn oauth_pkce_issues_site_tokens() {
    let Some(env) = setup(|_| {}).await else { return };
    let (pid, _) = env.player("Dana", &["board:play"]).await;
    let session = random_token(32);
    env.pg
        .execute(
            &format!("INSERT INTO {}.sessions (token_hash, player_id, expires_at) VALUES ($1, $2, now() + interval '1 day')", env.chess),
            &[&hash_key(&session), &pid],
        )
        .await
        .unwrap();
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let redirect = "http://app.test/callback";
    let q = [
        ("response_type", "code"),
        ("client_id", "test-app"),
        ("redirect_uri", redirect),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge),
        ("scope", "board:play challenge:write bot:play"),
        ("state", "s1"),
    ];
    // Not signed in: sent to sign in first, and back here after.
    let r = http.get(format!("{}/oauth", env.base[0])).query(&q).send().await.unwrap();
    assert!(r.status().is_redirection());
    let to = r.headers()["location"].to_str().unwrap().to_owned();
    assert!(to.starts_with("/login?return=%2Foauth%3F"), "{to}");
    let cookie = format!("oc_session={session}");
    let page = http
        .get(format!("{}/oauth", env.base[0]))
        .query(&q)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Authorize") && page.contains("board:play"));
    assert!(!page.contains("<code>bot:play</code>"), "bot:play is for managers");
    let csrf = page.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let form = [
        ("client_id", "test-app"),
        ("redirect_uri", redirect),
        ("code_challenge", challenge),
        ("scope", "board:play challenge:write"),
        ("state", "s1"),
        ("csrf", csrf.as_str()),
        ("decision", "approve"),
    ];
    let r = http
        .post(format!("{}/oauth", env.base[0]))
        .header("cookie", &cookie)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_redirection());
    let loc = r.headers()["location"].to_str().unwrap().to_owned();
    let u = url::Url::parse(&loc).unwrap();
    let code = u.query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
    assert_eq!(u.query_pairs().find(|(k, _)| k == "state").unwrap().1, "s1");
    let tok = |verifier: &str| {
        http.post(format!("{}/api/token", env.base[1])).form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("client_id", "test-app"),
        ])
    };
    let bad = tok("wrong-verifier-wrong-verifier-wrong-verifier-1").send().await.unwrap();
    assert_eq!(bad.status(), 400);
    // The code was used by the failed attempt: single use, as it should be.
    let again = tok(verifier).send().await.unwrap();
    assert_eq!(again.status(), 400);

    // A fresh code, the right verifier.
    let r = http
        .post(format!("{}/oauth", env.base[0]))
        .header("cookie", &cookie)
        .form(&form)
        .send()
        .await
        .unwrap();
    let loc = r.headers()["location"].to_str().unwrap().to_owned();
    let code2 =
        url::Url::parse(&loc).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
    let t: chess_client::proto::TokenResponse = http
        .post(format!("{}/api/token", env.base[1]))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code2.as_str()),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("client_id", "test-app"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(t.access_token.starts_with("ocp_"));
    let c = env.client(0, &t.access_token);
    assert_eq!(c.account().await.unwrap().username, "Dana");
    let scopes: Vec<String> = env
        .pg
        .query_one(
            &format!("SELECT scopes FROM {}.api_tokens WHERE name = 'OAuth: test-app'", env.chess),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(scopes, ["board:play", "challenge:write"]);
    let r =
        http.delete(format!("{}/api/token", env.base[0])).bearer_auth(&t.access_token).send().await.unwrap();
    assert_eq!(r.status(), 204);
    assert!(c.account().await.is_err(), "revoked");
    env.teardown().await;
}

/// Server-sent events from a response: (event, id, data) per event.
fn sse(r: reqwest::Response) -> tokio::sync::mpsc::Receiver<(String, Option<String>, serde_json::Value)> {
    use futures_util::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut body = r.bytes_stream();
        let mut buf = String::new();
        while let Some(Ok(chunk)) = body.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buf.find("\n\n") {
                let block: String = buf.drain(..end + 2).collect();
                let (mut event, mut id, mut data) = (String::from("message"), None, String::new());
                for l in block.lines() {
                    if let Some(v) = l.strip_prefix("event:") {
                        event = v.trim().to_owned();
                    } else if let Some(v) = l.strip_prefix("id:") {
                        id = Some(v.trim().to_owned());
                    } else if let Some(v) = l.strip_prefix("data:") {
                        data.push_str(v.trim_start());
                    }
                }
                if data.is_empty() {
                    continue;
                }
                if tx.send((event, id, serde_json::from_str(&data).unwrap())).await.is_err() {
                    return;
                }
            }
        }
    });
    rx
}

async fn next_sse(
    rx: &mut tokio::sync::mpsc::Receiver<(String, Option<String>, serde_json::Value)>,
) -> (String, Option<String>, serde_json::Value) {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("feed timed out")
        .expect("feed ended")
}

/// The live feed: a game's start, moves and end (with the rating
/// changes), and the finished games again for a client that comes back.
#[tokio::test]
async fn the_feed_follows_games_and_catches_up() {
    let Some(env) = setup(|c| c.public_delay_plies = 0).await else { return };
    let (_, ann_token) = env.player("Ann", HUMAN).await;
    let (_, bob_token) = env.player("Bob", HUMAN).await;
    let (ann, bob) = (env.client(0, &ann_token), env.client(1, &bob_token));
    let http = reqwest::Client::new();
    // Nobody reads it without a token: a player's, or a partner's.
    assert_eq!(http.get(format!("{}/api/feed", env.base[1])).send().await.unwrap().status(), 401);
    let mut feed =
        sse(http.get(format!("{}/api/feed", env.base[1])).bearer_auth(&ann_token).send().await.unwrap());
    let mut ends =
        sse(http.get(format!("{}/api/feed?only=end", env.base[0])).bearer_auth(SECRET).send().await.unwrap());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let req = ChallengeRequest {
        rated: true,
        limit_secs: Some(600),
        increment_secs: Some(5),
        color: Some("white".into()),
        accept_by_token: Some(bob_token.clone()),
        ..Default::default()
    };
    let them = bob.account().await.unwrap();
    let id = ann.challenge(&them.id, &req).await.unwrap().id;
    let (kind, _, v) = next_sse(&mut feed).await;
    assert_eq!((kind.as_str(), v["id"].as_str()), ("start", Some(id.as_str())));
    assert_eq!(v["players"]["white"]["user"]["name"], "Ann");
    for (who, m) in [(&ann, "f2f3"), (&bob, "e7e5"), (&ann, "g2g4"), (&bob, "d8h4")] {
        who.make_move(Api::Board, &id, m, false).await.unwrap();
    }
    let (sid, end) = loop {
        let (kind, sid, v) = next_sse(&mut feed).await;
        if kind == "end" {
            break (sid, v);
        }
        assert_eq!(kind, "move");
    };
    assert_eq!(end["status"], "mate");
    assert_eq!(end["winner"], "black");
    assert!(end["players"]["black"]["ratingDiff"].as_i64().unwrap() > 0, "{end}");
    assert_eq!(end["moves"], "f3 e5 g4 Qh4#");
    let (kind, _, only) = next_sse(&mut ends).await;
    assert_eq!((kind.as_str(), only["id"].as_str()), ("end", Some(id.as_str())), "only the end");
    // Coming back from just before it: the finished game again.
    let finished: i64 = sid.unwrap().parse().unwrap();
    let mut again = sse(http
        .get(format!("{}/api/feed", env.base[0]))
        .bearer_auth(&bob_token)
        .header("last-event-id", (finished - 1).to_string())
        .send()
        .await
        .unwrap());
    let (kind, _, v) = next_sse(&mut again).await;
    assert_eq!((kind.as_str(), v["id"].as_str()), ("end", Some(id.as_str())));
    let mut later = sse(http
        .get(format!("{}/api/feed?since={}", env.base[0], finished + 1))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap());
    assert!(tokio::time::timeout(Duration::from_millis(500), later.recv()).await.is_err(), "nothing newer");
    env.teardown().await;
}

/// The value of a Set-Cookie header's cookie.
fn set_cookie(r: &reqwest::Response, name: &str) -> Option<String> {
    r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
        let (kv, _) = v.split_once(';').unwrap_or((v, ""));
        let (k, val) = kv.split_once('=')?;
        (k == name && !val.is_empty()).then(|| val.to_owned())
    })
}

/// The server's own accounts: sign up, sign in, make a token and use it.
#[tokio::test]
async fn own_accounts_sign_up_sign_in_and_make_tokens() {
    let Some(env) = setup(|_| {}).await else { return };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let about: serde_json::Value = http
        .get(&env.base[0])
        .header("accept", "application/json")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (about["login"].as_bool(), about["signup"].as_bool(), about["esports"].as_bool()),
        (Some(true), Some(true), Some(true))
    );
    let short = http
        .post(format!("{}/signup", env.base[0]))
        .form(&[("name", "Gwen"), ("password", "short"), ("return", "/account")])
        .send()
        .await
        .unwrap();
    assert_eq!(short.status(), 400);
    let r = http
        .post(format!("{}/signup", env.base[0]))
        .form(&[
            ("name", "Gwen"),
            ("email", "gwen@example.org"),
            ("password", "a long enough password"),
            ("return", "/account"),
        ])
        .send()
        .await
        .unwrap();
    assert!(r.status().is_redirection(), "{}", r.status());
    assert_eq!(r.headers()["location"], "/account");
    let session = set_cookie(&r, "oc_session").expect("signed in");
    let duplicate = http
        .post(format!("{}/signup", env.base[1]))
        .form(&[("name", "gwen"), ("password", "a long enough password")])
        .send()
        .await
        .unwrap();
    assert!(duplicate.text().await.unwrap().contains("taken"));
    // The account page makes a token, shown once.
    let page = http
        .get(format!("{}/account", env.base[0]))
        .header("cookie", format!("oc_session={session}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Signed in as <b>Gwen</b>"), "{page}");
    let tokens_form = page.split("action=\"/account/tokens\">").nth(1).unwrap();
    let csrf =
        tokens_form.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let page = http
        .post(format!("{}/account/tokens", env.base[1]))
        .header("cookie", format!("oc_session={session}"))
        .form(&[
            ("csrf", csrf.as_str()),
            ("label", "my board"),
            ("scope:board:play", "on"),
            ("scope:bot:play", "on"),
            ("days", "30"),
        ])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let token = page.split("<code class=token>").nth(1).unwrap().split('<').next().unwrap().to_owned();
    assert!(token.starts_with("ocp_"));
    let me = env.client(0, &token);
    assert_eq!(me.account().await.unwrap().username, "Gwen");
    let scopes: Vec<String> = env
        .pg
        .query_one(&format!("SELECT scopes FROM {}.api_tokens WHERE name = 'my board'", env.chess), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(scopes, ["board:play"], "bot:play is for staff");
    // Signing in by email, wrongly then rightly.
    let bad = http
        .post(format!("{}/login", env.base[0]))
        .form(&[("name", "gwen@example.org"), ("password", "not the password")])
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    let good = http
        .post(format!("{}/login", env.base[0]))
        .form(&[
            ("name", "GWEN@example.org"),
            ("password", "a long enough password"),
            ("return", "/oauth?x=1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(good.headers()["location"], "/oauth?x=1");
    assert!(set_cookie(&good, "oc_session").is_some());
    // Somewhere else isn't a place to be sent back to.
    let elsewhere = http
        .post(format!("{}/login", env.base[0]))
        .form(&[("name", "Gwen"), ("password", "a long enough password"), ("return", "//evil.test/")])
        .send()
        .await
        .unwrap();
    assert_eq!(elsewhere.headers()["location"], "/account");
    // A return path can't end the page's script and start another.
    let page = http
        .get(format!("{}/login", env.base[0]))
        .query(&[("return", "/x</script><script>alert(1)</script>")])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!page.contains("<script>alert"), "{page}");
    // Changing the password takes the current one (no passkey to confirm with).
    let pw_csrf = hash_key(&format!("{session}|passkeys"));
    let change = |current: &'static str| {
        http.post(format!("{}/account/password", env.base[0]))
            .header("cookie", format!("oc_session={session}"))
            .form(&[("csrf", pw_csrf.as_str()), ("current", current), ("password", "another long password")])
            .send()
    };
    assert_eq!(change("wrong guess").await.unwrap().status(), 403);
    assert!(change("a long enough password").await.unwrap().status().is_redirection());
    env.teardown().await;
}

/// "Sign in with the site": the site's signed answer signs in (and links)
/// its player; its server gets tokens for its players with the secret.
#[tokio::test]
async fn the_host_site_signs_players_in_and_gets_their_tokens() {
    use open_chess_server::http::web::sign;
    let Some(env) = setup(|_| {}).await else { return };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    // The sign-in page names the host site; the server keeps its own name.
    let page = http.get(format!("{}/login", env.base[0])).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("Sign in with esports-lab"), "{page}");
    let index: serde_json::Value = http
        .get(format!("{}/", env.base[0]))
        .header("accept", "application/json")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(index["site"], "open-chess");
    // Off to the site, with a nonce to come back with.
    let r = http.get(format!("{}/auth/esports?return=/account", env.base[0])).send().await.unwrap();
    let to = r.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        to.starts_with(
            "http://site.test/chess/sso?return=http%3A%2F%2Fchess.test%2Fauth%2Fesports%2Fback&nonce="
        ),
        "{to}"
    );
    let nonce = to.split("nonce=").nth(1).unwrap().to_owned();
    let sso = set_cookie(&r, "oc_sso").unwrap();
    let exp = chrono::Utc::now().timestamp() + 60;
    let answer = |nonce: &str, secret: &str| {
        sign(
            secret,
            &serde_json::json!({"sub": "77", "name": "Hal Jones", "email": "hal@site.test", "role": "player", "roles": ["manager"], "nonce": nonce, "exp": exp}),
        )
    };
    // A forged or replayed answer is refused.
    let forged = http
        .get(format!("{}/auth/esports/back", env.base[1]))
        .query(&[("a", answer(&nonce, &"x".repeat(40)))])
        .header("cookie", format!("oc_sso={sso}"))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 403);
    let wrong_nonce = http
        .get(format!("{}/auth/esports/back", env.base[1]))
        .query(&[("a", answer("other", SECRET))])
        .header("cookie", format!("oc_sso={sso}"))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_nonce.status(), 403);
    let back = http
        .get(format!("{}/auth/esports/back", env.base[1]))
        .query(&[("a", answer(&nonce, SECRET))])
        .header("cookie", format!("oc_sso={sso}"))
        .send()
        .await
        .unwrap();
    assert_eq!(back.headers()["location"], "/account");
    let session = set_cookie(&back, "oc_session").unwrap();
    let page = http
        .get(format!("{}/account", env.base[0]))
        .header("cookie", format!("oc_session={session}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Signed in as <b>Hal-Jones</b> (linked"), "{page}");
    // The site's server asks for a token: the same player, linked.
    let ask = |secret: &str| {
        http.post(format!("{}/api/esports/token", env.base[0])).bearer_auth(secret).form(&[
            ("sub", "77"),
            ("name", "Hal Jones"),
            ("roles", "manager"),
            ("scopes", "board:play chess:fairplay"),
        ])
    };
    assert_eq!(ask("wrong").send().await.unwrap().status(), 401);
    let t: serde_json::Value = ask(SECRET).send().await.unwrap().json().await.unwrap();
    assert_eq!(t["username"], "Hal-Jones");
    assert_eq!(t["scopes"], serde_json::json!(["board:play"]), "a manager on the site is a player here");
    let me = env.client(1, t["token"].as_str().unwrap());
    assert_eq!(me.account().await.unwrap().username, "Hal-Jones");
    // A game's tokens last hours, and go together when the site says.
    let before = chrono::Utc::now().timestamp_millis();
    let game: serde_json::Value = http
        .post(format!("{}/api/esports/token", env.base[0]))
        .bearer_auth(SECRET)
        .form(&[("sub", "77"), ("name", "Hal Jones"), ("label", "chess match 5"), ("hours", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let left = game["expires"].as_i64().unwrap() - before;
    assert!((7_100_000..=7_300_000).contains(&left), "{left}");
    let r: serde_json::Value = http
        .post(format!("{}/api/esports/revoke", env.base[1]))
        .bearer_auth(SECRET)
        .form(&[("label", "chess match 5")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["revoked"], 1);
    assert!(env.client(0, game["token"].as_str().unwrap()).account().await.is_err());
    assert_eq!(me.account().await.unwrap().username, "Hal-Jones", "other tokens stay");
    let linked: serde_json::Value = http
        .get(format!("{}/api/esports/players", env.base[1]))
        .bearer_auth(SECRET)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(linked["players"], serde_json::json!([{"sub": "77", "username": "Hal-Jones"}]));
    // A new site player gets a chess account on first use.
    let t: serde_json::Value = http
        .post(format!("{}/api/esports/token", env.base[0]))
        .bearer_auth(SECRET)
        .form(&[("sub", "78"), ("name", "Ivy")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((t["username"].as_str(), t["scopes"].as_array().map(Vec::len)), (Some("Ivy"), Some(3)));
    env.teardown().await;
}

/// A deployment that used the host site's players: they, their chess
/// tokens and our references to them come across once.
#[tokio::test]
async fn the_site_links_an_existing_account() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, token) = env.player("Own-Account", &["board:play"]).await;
    let me: serde_json::Value =
        env.client(0, &token).account().await.map(|u| serde_json::json!({"id": u.id})).unwrap();
    assert!(me["id"].is_string(), "the account exists once used");
    let http = reqwest::Client::new();
    let link = |sub: &'static str, username: &'static str| {
        http.post(format!("{}/api/esports/link", env.base[0]))
            .bearer_auth(SECRET)
            .form(&[("sub", sub), ("username", username)])
            .send()
    };
    let r = link("777", "own-account").await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["username"], "Own-Account");
    let players: serde_json::Value = http
        .get(format!("{}/api/esports/players", env.base[0]))
        .bearer_auth(SECRET)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        players["players"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["sub"] == "777" && p["username"] == "Own-Account")
    );
    // Another site player can't take it; an unknown name isn't there.
    assert_eq!(link("778", "Own-Account").await.unwrap().status(), 409);
    assert_eq!(link("777", "nobody-here").await.unwrap().status(), 404);
    // Linking again is fine; an empty name unlinks.
    assert_eq!(link("777", "Own-Account").await.unwrap().status(), 200);
    assert_eq!(link("777", "").await.unwrap().status(), 200);
    assert_eq!(link("778", "Own-Account").await.unwrap().status(), 200, "free again");
    // Without the secret: no.
    let r = http
        .post(format!("{}/api/esports/link", env.base[0]))
        .bearer_auth(&token)
        .form(&[("sub", "1"), ("username", "Own-Account")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    env.teardown().await;
}

#[tokio::test]
async fn the_site_players_are_imported_once() {
    let pre = "
        INSERT INTO {site}.players (id, name, email, roles) VALUES (7, 'Jo', 'jo@site.test', '{manager}'), (8, 'Kim', NULL, '{}'), (9, 'Lee', NULL, '{}');
        INSERT INTO {site}.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
          VALUES (8, 'chess', encode(sha256('esl_kims-token'), 'hex'), 'esl_ki', '{board:play,esports:read}', now() + interval '1 day'),
                 (9, 'site only', encode(sha256('esl_lees-token'), 'hex'), 'esl_le', '{esports:read}', now() + interval '1 day');
        CREATE SCHEMA {chess};
        CREATE TABLE {chess}.legacy (player_id bigint REFERENCES {site}.players ON DELETE CASCADE);
        INSERT INTO {chess}.legacy VALUES (7);";
    let Some(env) = setup_with(pre, |_| {}).await else { return };
    let rows = env
        .pg
        .query(&format!("SELECT id, name, esports_id, roles FROM {}.players ORDER BY id", env.chess), &[])
        .await
        .unwrap();
    let got: Vec<(i64, String, Option<String>, Vec<String>)> =
        rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect();
    assert_eq!(
        got,
        [(7, "Jo".into(), Some("7".into()), vec![]), (8, "Kim".into(), Some("8".into()), vec![])],
        "Jo (named in our table) and Kim (a chess token), not Lee; the site's roles stay the site's"
    );
    // Kim's token works here, with only its chess scope.
    let kim = env.client(0, "esl_kims-token");
    assert_eq!(kim.account().await.unwrap().username, "Kim");
    assert!(env.client(0, "esl_lees-token").account().await.is_err());
    // Our table now points at our players.
    let points: String = env
        .pg
        .query_one(
            &format!("SELECT confrelid::regclass::text FROM pg_constraint WHERE conrelid = '{}.legacy'::regclass AND contype = 'f'", env.chess),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(points, format!("{}.players", env.chess));
    // New players don't collide with imported ids; a second start imports nothing again.
    let (id, _) = env.player("New", &["board:play"]).await;
    assert!(id > 8, "{id}");
    let db = open_chess_server::db::Db::connect(&env.cfg).unwrap();
    db.migrate().await.unwrap();
    let n: i64 =
        env.pg.query_one(&format!("SELECT count(*) FROM {}.players", env.chess), &[]).await.unwrap().get(0);
    assert_eq!(n, 3);
    env.teardown().await;
}

#[tokio::test]
async fn public_stream_runs_behind_live_games() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, a) = env.player("Ann", HUMAN).await;
    let (_, b) = env.player("Ben", HUMAN).await;
    env.client(0, &b).account().await.unwrap();
    let id = start(&env, &a, "ben", &b, Some((300, 0))).await;
    let (w, bl) = colours(&env, &id, ("Ann", &a), ("Ben", &b)).await;
    for (t, m) in [(&w, "e2e4"), (&bl, "e7e5"), (&w, "g1f3"), (&bl, "b8c6"), (&w, "f1b5")] {
        env.client(0, t).make_move(Api::Board, &id, m, false).await.unwrap();
    }
    assert!(Client::new(&env.base[1], None).unwrap().stream_moves(&id).await.is_err(), "signed in only");
    let (_, watcher) = env.player("Wes", HUMAN).await;
    let mut s = env.client(1, &watcher).stream_moves(&id).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), s.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(first["id"], id.as_str());
    // The description is as delayed as the moves: five plies played, two shown.
    assert_eq!((first["turns"].as_i64(), first["lastMove"].as_str()), (Some(2), Some("e7e5")));
    let mut moves = Vec::new();
    while let Ok(Some(Ok(v))) = tokio::time::timeout(Duration::from_millis(800), s.next()).await {
        if let Some(m) = Client::move_line(&v) {
            moves.push(m.lm);
        }
    }
    assert_eq!(moves, [None, Some("e2e4".into()), Some("e7e5".into())], "five plies played, three held back");
    env.teardown().await;
}

#[tokio::test]
async fn fair_play_screening_and_review() {
    let Some(env) = setup(|_| {}).await else { return };
    let Some(sf) = chess_engine::find_stockfish() else {
        eprintln!("no Stockfish: skipped");
        env.teardown().await;
        return;
    };
    let (_, a) = env.player("Ann", HUMAN).await;
    let (_, b) = env.player("Ben", HUMAN).await;
    let (mid, m) = env.player("Mod", &["chess:fairplay"]).await;
    env.pg
        .execute(&format!("UPDATE {}.players SET roles = '{{manager}}' WHERE id = $1", env.chess), &[&mid])
        .await
        .unwrap();
    // A manager's rights wait on their passkey check.
    let r = reqwest::Client::new()
        .get(format!("{}/api/fairplay/queue", env.base[0]))
        .bearer_auth(&m)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    env.staff(mid, false).await;
    env.client(0, &b).account().await.unwrap();
    let id = start(&env, &a, "ben", &b, Some((600, 5))).await;
    let (w, bl) = colours(&env, &id, ("Ann", &a), ("Ben", &b)).await;
    // A real opening, 24 plies, then a resignation.
    let line = "e2e4 e7e5 g1f3 b8c6 f1b5 a7a6 b5a4 g8f6 e1g1 f8e7 f1e1 b7b5 a4b3 d7d6 c2c3 e8g8 h2h3 c6a5 b3c2 c7c5 d2d4 d8c7 b1d2 c5d4";
    let http = reqwest::Client::new();
    for (i, mv) in line.split_whitespace().enumerate() {
        let t = if i % 2 == 0 { &w } else { &bl };
        env.client(0, t).make_move(Api::Board, &id, mv, false).await.unwrap();
        if i == 10 {
            // White's app says its engine help went on: recorded as a label.
            let r = http
                .post(format!("{}/api/board/game/{id}/assist", env.base[0]))
                .bearer_auth(&w)
                .form(&[("on", "true")])
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            // While playing, the position dictionary is closed to you.
            let r = http
                .get(format!("{}/api/esl/position", env.base[0]))
                .query(&[("fen", "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")])
                .bearer_auth(&w)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 403);
        }
    }
    // Someone else's game: no assist marks.
    let r = http
        .post(format!("{}/api/board/game/{id}/assist", env.base[0]))
        .bearer_auth(&m)
        .form(&[("on", "true")])
        .send()
        .await
        .unwrap();
    assert!(r.status() == 403 || r.status() == 401, "{}", r.status());
    env.client(0, &w).resign(Api::Board, &id).await.unwrap();

    let db = Db::connect(&env.cfg).unwrap();
    let (_tx, rx) = watch::channel(false);
    let acfg = AnalyseConfig {
        stockfish: Some(sf),
        depth: 8,
        threads: 1,
        hash_mb: 16,
        skip_plies: 0,
        model: None,
        flag_score: None,
        min_moves: Some(5),
        app_depth: 3,
        app_nodes: 20_000,
        app_net: None,
        once: true,
    };
    open_chess_server::analyse::run(db, open_chess_server::metrics::Metrics::new(), acfg, rx).await.unwrap();

    let get =
        |path: String, token: &str| http.get(format!("{}{path}", env.base[0])).bearer_auth(token).send();
    assert_eq!(
        get(format!("/api/fairplay/games/{id}"), &a).await.unwrap().status(),
        403,
        "players can't review"
    );
    let report: serde_json::Value =
        get(format!("/api/fairplay/games/{id}"), &m).await.unwrap().json().await.unwrap();
    assert_eq!(report["analysis"], "done");
    let reports = report["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    assert!(
        reports
            .iter()
            .all(|r| r["moves"].as_i64().unwrap() >= 5 && r["evals"].as_array().unwrap().len() == 12)
    );
    let white_name = if w == a { "ann" } else { "ben" };
    for r in reports {
        assert!(r["appFingerprint"]["disagreements"].is_i64(), "{r}");
        assert!(r["evals"][0]["best"].is_string());
        let user = r["user"].as_str().unwrap().to_lowercase();
        assert_eq!(r["declaredAssist"].as_bool(), Some(user == white_name), "{r}");
    }
    // The screened positions are now in the position dictionary.
    let pos: serde_json::Value = http
        .get(format!("{}/api/esl/position", env.base[0]))
        .query(&[("fen", "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1")])
        .bearer_auth(&m)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pos["known"], true, "{pos}");
    assert!(pos["best"].is_string() && pos["wdl"]["white"].is_number(), "{pos}");
    // Photo reading isn't configured in tests.
    let r = http
        .post(format!("{}/api/esl/recognise", env.base[0]))
        .bearer_auth(&m)
        .json(&serde_json::json!({"image": "aGVsbG8="}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);

    // A verdict of engine use marks the account; it can't start games.
    let r = http
        .post(format!("{}/api/fairplay/reviews", env.base[0]))
        .bearer_auth(&m)
        .json(&serde_json::json!({"username": "ann", "gameId": id, "verdict": "engine", "note": "test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(env.client(0, &b).user("ann").await.unwrap().tos_violation, Some(true));
    let refused = env
        .client(0, &a)
        .challenge(
            "ben",
            &ChallengeRequest { limit_secs: Some(60), increment_secs: Some(0), ..Default::default() },
        )
        .await;
    assert!(matches!(refused, Err(chess_client::Error::Api { status: 403, .. })));
    let rec: serde_json::Value =
        get("/api/fairplay/players/ann".into(), &m).await.unwrap().json().await.unwrap();
    assert_eq!((rec["marked"].as_bool(), rec["screened"].as_i64()), (Some(true), Some(1)));
    env.teardown().await;
}

/// A 32×24 checkered PNG: not a chess board.
const SMALL_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAYCAIAAAAUMWhjAAAAO0lEQVR42mNkiNJggIETDAhggcSmRJyJgcaA5haw0CJYLIZXENEiWEZT0WgqGk1Fo6loNBWNpiK6BxEAHHogiE9/DK8AAAAASUVORK5CYII=";

#[tokio::test]
async fn unsure_photos_are_kept_for_review_and_confirmed() {
    // A tiny untrained network, so every reading is unsure.
    let cfg = chess_vision::ModelConfig {
        image_size: 42,
        encoder: chess_vision::config::EncoderConfig {
            dim: 32,
            depth: 2,
            heads: 2,
            mlp_dim: 64,
            patch: 14,
            registers: 0,
        },
        decoder: chess_vision::config::DecoderConfig { dim: 16, layers: 2, heads: 2, ffn_dim: 32 },
        mean: [0.485, 0.456, 0.406],
        std: [0.229, 0.224, 0.225],
    };
    let path = std::env::temp_dir().join(format!("esl-reader-{}.safetensors", random_token(8)));
    std::fs::write(
        &path,
        chess_vision::weights::random_model(&cfg, &chess_vision::ModelCard::default()).unwrap(),
    )
    .unwrap();
    let model = path.to_string_lossy().into_owned();
    let Some(env) = setup(|c| c.vision_local_model = model).await else { return };
    let (me, token) = env.player("photographer", &["board:play"]).await;
    let (_, other) = env.player("someone-else", &["board:play"]).await;
    let http = reqwest::Client::new();
    let post = |path: &str, token: &str, body: serde_json::Value| {
        http.post(format!("{}{path}", env.base[0])).bearer_auth(token).json(&body).send()
    };

    // Not kept until the sender accepts the photo licence.
    let r = post("/api/esl/recognise", &token, serde_json::json!({"image": SMALL_PNG})).await.unwrap();
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!((v["review"].as_bool(), v["termsNeeded"].as_bool()), (Some(false), Some(true)), "{v}");
    assert!(v["photoId"].is_null());
    let terms: serde_json::Value =
        post("/api/esl/photo-terms", &token, serde_json::json!({"accept": true, "public": true}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!((terms["accepted"].as_bool(), terms["public"].as_bool()), (Some(true), Some(true)));

    let r = post("/api/esl/recognise", &token, serde_json::json!({"image": SMALL_PNG})).await.unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["source"], "model");
    assert_eq!(v["squares"].as_array().map(Vec::len), Some(64));
    assert_eq!((v["review"].as_bool(), v["termsNeeded"].as_bool()), (Some(true), Some(false)), "{v}");
    let id = v["photoId"].as_i64().expect("kept for review");

    let row = env
        .pg
        .query_one(
            &format!(
                "SELECT player_id, reason, reading->>'placement', label, terms, public FROM {}.board_photos WHERE id = $1",
                env.chess
            ),
            &[&id],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>(0), Some(me));
    assert!(["unsure", "implausible"].contains(&row.get::<_, String>(1).as_str()));
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), v["placement"].as_str());
    assert_eq!(row.get::<_, Option<String>>(3), None);
    assert_eq!(row.get::<_, Option<String>>(4).as_deref(), Some("open-chess-photo-licence-1"));
    assert!(row.get::<_, bool>(5));

    // Only the player who sent it can confirm it, and only a board.
    let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let start_placement = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";
    let path = format!("/api/esl/photos/{id}/confirm");
    assert_eq!(post(&path, &other, serde_json::json!({"placement": start})).await.unwrap().status(), 404);
    assert_eq!(post(&path, &token, serde_json::json!({"placement": "8/8/8"})).await.unwrap().status(), 400);
    assert_eq!(
        post(&path, &token, serde_json::json!({"placement": start, "fen": start})).await.unwrap().status(),
        200
    );
    // The confirmed position joins the dictionary, waiting for the worker.
    let queued: serde_json::Value = http
        .get(format!("{}/api/esl/position", env.base[0]))
        .query(&[("fen", start)])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (queued["known"].as_bool(), queued["queued"].as_bool(), queued["photos"].as_i64()),
        (Some(false), Some(true), Some(1)),
        "{queued}"
    );
    let label = |id: i64| {
        let (pg, chess) = (&env.pg, env.chess.clone());
        async move {
            let r = pg
                .query_one(
                    &format!("SELECT label, label_source FROM {chess}.board_photos WHERE id = $1"),
                    &[&id],
                )
                .await
                .unwrap();
            (r.get::<_, Option<String>>(0), r.get::<_, Option<String>>(1))
        }
    };
    assert_eq!(label(id).await, (Some(start_placement.into()), Some("player".into())));

    // Anyone signed in reviews: the photo is in someone else's queue, not
    // its sender's, and they can see it.
    let queue = |token: String| {
        let http = http.clone();
        let base = env.base[1].clone();
        async move {
            let v: serde_json::Value = http
                .get(format!("{base}/api/esl/photos"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["photos"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect::<Vec<_>>()
        }
    };
    assert!(queue(other.clone()).await.contains(&id));
    assert!(!queue(token.clone()).await.contains(&id), "not your own photo");
    let img = http
        .get(format!("{}/api/esl/photos/{id}/image", env.base[1]))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    assert_eq!(img.status(), 200);
    assert!(!img.bytes().await.unwrap().is_empty());

    // Someone disagrees: still open. A third agrees with the sender: two
    // matching answers label it.
    let vote = |token: String, body: serde_json::Value| {
        let http = http.clone();
        let base = env.base[0].clone();
        async move {
            let r = http
                .post(format!("{base}/api/esl/photos/{id}/label"))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.json::<serde_json::Value>().await.unwrap()
        }
    };
    let v = vote(other.clone(), serde_json::json!({"placement": "4k3/8/8/8/8/8/8/4K3"})).await;
    assert_eq!(v["settled"], false, "{v}");
    assert!(!queue(other.clone()).await.contains(&id), "answered: out of their queue");
    let (_, third) = env.player("third-person", &["board:play"]).await;
    let v = vote(third.clone(), serde_json::json!({"placement": start})).await;
    assert_eq!(v["settled"], true, "{v}");
    assert_eq!(label(id).await, (Some(start_placement.into()), Some("consensus".into())));
    assert!(!queue(third.clone()).await.contains(&id));

    // An admin or manager settles it alone, and others can't outvote them.
    let t: serde_json::Value = http
        .post(format!("{}/api/esports/token", env.base[0]))
        .bearer_auth(SECRET)
        .form(&[("sub", "900"), ("name", "Reviewer"), ("roles", "manager"), ("scopes", "board:play")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let manager = t["token"].as_str().unwrap().to_owned();
    // The site's roles don't count here: managers are made here.
    let reviewer: i64 = env
        .pg
        .query_one(&format!("SELECT id FROM {}.players WHERE esports_id = '900'", env.chess), &[])
        .await
        .unwrap()
        .get(0);
    env.staff(reviewer, false).await;
    let list: serde_json::Value = http
        .get(format!("{}/api/esl/photos?all=true", env.base[1]))
        .bearer_auth(&manager)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["staff"], true);
    assert!(list["photos"].as_array().unwrap().iter().any(|p| p["id"] == id && p["votes"] == 3), "{list}");
    let v = vote(manager.clone(), serde_json::json!({"placement": "4k3/8/8/8/8/8/8/4K3 w - - 0 1"})).await;
    assert_eq!(v["settled"], true);
    assert_eq!(label(id).await, (Some("4k3/8/8/8/8/8/8/4K3".into()), Some("admin".into())));
    let (_, fourth) = env.player("fourth-person", &["board:play"]).await;
    vote(fourth, serde_json::json!({"placement": start})).await;
    assert_eq!(
        label(id).await,
        (Some("4k3/8/8/8/8/8/8/4K3".into()), Some("admin".into())),
        "the admin's word stands"
    );
    // Nor does the sender's confirmation overwrite it.
    assert_eq!(
        post(&path, &token, serde_json::json!({"placement": "8/8/8/8/8/8/8/8"})).await.unwrap().status(),
        404
    );

    env.teardown().await;
}

#[tokio::test]
async fn the_dictionary_takes_hints_and_scores_every_move() {
    let Some(env) = setup(|_| {}).await else { return };
    let (_, token) = env.player("analyst", &["board:play"]).await;
    let http = reqwest::Client::new();
    let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let hint = |body: serde_json::Value| {
        http.post(format!("{}/api/esl/position", env.base[0])).bearer_auth(&token).json(&body).send()
    };
    let get = |path: &str| {
        http.get(format!("{}{path}", env.base[0])).query(&[("fen", start)]).bearer_auth(&token).send()
    };
    // An app's analysis: legal moves only.
    let bad = serde_json::json!({"fen": start, "best": "e2e5", "scoreCp": 30, "depth": 8});
    assert_eq!(hint(bad).await.unwrap().status(), 400);
    let good = serde_json::json!({"fen": start, "best": "e2e4", "scoreCp": 30, "depth": 8, "engine": "test",
        "moves": [{"uci": "e2e4", "scoreCp": 30}, {"uci": "a2a3", "scoreCp": -10}]});
    assert_eq!(hint(good).await.unwrap().status(), 200);
    // A shallower one from the same player doesn't replace it.
    let shallow = serde_json::json!({"fen": start, "best": "a2a3", "scoreCp": -5, "depth": 2});
    assert_eq!(hint(shallow).await.unwrap().status(), 200);
    let pos: serde_json::Value = get("/api/esl/position").await.unwrap().json().await.unwrap();
    assert_eq!(pos["known"], false);
    assert_eq!(
        (pos["hint"]["best"].as_str(), pos["hint"]["depth"].as_i64()),
        (Some("e2e4"), Some(8)),
        "{pos}"
    );

    // The worker's evaluation of the position after 1.d4, from Black's view.
    env.pg
        .execute(
            &format!(
                "INSERT INTO {}.positions (key, best, score_cp, depth, engine) VALUES ($1, 'g8f6', -25, 20, 'sf')",
                env.chess
            ),
            &[&"rnbqkbnr/pppppppp/8/8/3P4/8/PPP1PPPP/RNBQKBNR b KQkq -"],
        )
        .await
        .unwrap();
    let all: serde_json::Value = get("/api/esl/position/moves").await.unwrap().json().await.unwrap();
    let moves = all["moves"].as_array().unwrap();
    assert_eq!(moves.len(), 20);
    let by = |uci: &str| moves.iter().find(|m| m["uci"] == uci).unwrap().clone();
    // Black's -25 after 1.d4 is +25 for White, the mover.
    assert_eq!((by("d2d4")["scoreCp"].as_i64(), by("d2d4")["source"].as_str()), (Some(25), Some("after")));
    assert_eq!(by("d2d4")["san"], "d4");
    assert_eq!((by("e2e4")["scoreCp"].as_i64(), by("e2e4")["source"].as_str()), (Some(30), Some("hint")));
    assert_eq!(by("g1f3")["source"], "unknown");
    assert!(by("g1f3")["scoreCp"].is_null());

    // With Stockfish, the worker evaluates a queued photo position, every move.
    if let Some(sf) = chess_engine::find_stockfish() {
        let key = "4k3/8/8/8/8/8/4P3/4K3 w - -";
        env.pg
            .execute(
                &format!("INSERT INTO {}.positions (key, score_cp, depth, engine, seen, photos) VALUES ($1, NULL, 0, NULL, 0, 1)", env.chess),
                &[&key],
            )
            .await
            .unwrap();
        let db = Db::connect(&env.cfg).unwrap();
        let (_tx, rx) = watch::channel(false);
        let acfg = AnalyseConfig {
            stockfish: Some(sf),
            depth: 8,
            threads: 1,
            hash_mb: 16,
            skip_plies: 0,
            model: None,
            flag_score: None,
            min_moves: Some(5),
            app_depth: 3,
            app_nodes: 20_000,
            app_net: None,
            once: true,
        };
        open_chess_server::analyse::run(db, open_chess_server::metrics::Metrics::new(), acfg, rx)
            .await
            .unwrap();
        let row = env
            .pg
            .query_one(
                &format!(
                    "SELECT score_cp, best, jsonb_array_length(moves) FROM {}.positions WHERE key = $1",
                    env.chess
                ),
                &[&key],
            )
            .await
            .unwrap();
        assert!(row.get::<_, Option<i32>>(0).unwrap() > 0, "White's pawn up");
        assert!(row.get::<_, Option<String>>(1).is_some());
        // King: 4 moves (d1, d2, f1, f2), pawn: 2.
        assert_eq!(row.get::<_, Option<i32>>(2), Some(6));
    } else {
        eprintln!("no Stockfish: the worker part skipped");
    }
    env.teardown().await;
}

#[tokio::test]
async fn serves_the_training_app() {
    let dir = std::env::temp_dir().join(format!("esl-app-{}", random_token(8)));
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><title>Chess lab</title>").unwrap();
    std::fs::write(dir.join("sw.js"), "// worker").unwrap();
    std::fs::write(dir.join("pkg/engine_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("docs/api.md"), "# The API").unwrap();
    let app_dir = dir.to_string_lossy().into_owned();
    let Some(env) = setup(|c| {
        c.app_dir = Some(app_dir);
        c.app_theme = Some(":root { --paper: #06114a; }".into());
    })
    .await
    else {
        return;
    };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let base = &env.base[0];

    let r = http.get(format!("{base}/app")).send().await.unwrap();
    assert_eq!(r.status(), 308);
    assert!(r.headers()["location"].to_str().unwrap().ends_with("/app/"));

    let r = http.get(format!("{base}/app/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
    assert_eq!(r.headers()["cache-control"], "no-cache");
    assert!(r.text().await.unwrap().contains("Chess lab"));

    let r = http.get(format!("{base}/app/pkg/engine_bg.wasm")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "application/wasm");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"\0asm\x01\0\0\0");
    let r = http.get(format!("{base}/app/sw.js")).send().await.unwrap();
    assert_eq!(r.headers()["service-worker-allowed"], "/app/");
    // The deployment's look, and the docs that ship with the app.
    let r = http.get(format!("{base}/app/theme.css")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "text/css; charset=utf-8");
    assert_eq!(r.text().await.unwrap(), ":root { --paper: #06114a; }");
    let r = http.get(format!("{base}/app/docs/api.md")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "text/markdown; charset=utf-8");

    // The guides open in the app; the logo is the favicon and on the pages.
    for (path, to) in
        [("/docs", "/app/#docs"), ("/docs/", "/app/#docs"), ("/docs/boards.md", "/app/#docs/boards")]
    {
        let r = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status(), 303, "{path}");
        assert!(r.headers()["location"].to_str().unwrap().ends_with(to), "{path}");
    }
    let r = http.get(format!("{base}/favicon.ico")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "image/svg+xml");
    assert!(r.text().await.unwrap().contains("<svg"));
    let login = http.get(format!("{base}/login")).send().await.unwrap().text().await.unwrap();
    assert!(login.contains("rel=icon") && login.contains("<svg width=\"36\""));

    for bad in ["/app/missing.js", "/app/..%2f..%2fetc%2fpasswd", "/app/.hidden"] {
        assert_eq!(http.get(format!("{base}{bad}")).send().await.unwrap().status(), 404, "{bad}");
    }
    // Browsers at the root go to the app; API clients still get JSON.
    let r = http.get(format!("{base}/")).header("accept", "text/html,*/*").send().await.unwrap();
    assert_eq!(r.status(), 303);
    let r = http.get(format!("{base}/")).send().await.unwrap();
    let info: serde_json::Value = r.json().await.unwrap();
    assert!(info["app"].as_str().unwrap().ends_with("/app/"));
    env.teardown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn tablebase_answers_are_fetched_once_and_kept() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A stand-in for tablebase.lichess.ovh that counts its requests.
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let mock = axum::Router::new().route(
        "/standard",
        axum::routing::get(move || {
            h.fetch_add(1, Ordering::SeqCst);
            async {
                axum::Json(serde_json::json!({"category": "win", "dtz": 1, "dtm": 27,
                    "moves": [{"uci": "h1h5", "san": "Rh5+", "category": "loss"}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

    let Some(env) = setup(|c| c.tablebase_url = mock_url).await else { return };
    let (_, t) = env.player("Tia", HUMAN).await;
    let http = reqwest::Client::new();
    let ask = |fen: &str| {
        http.get(format!("{}/api/esl/tablebase", env.base[0])).query(&[("fen", fen)]).bearer_auth(&t).send()
    };
    let krk = "8/8/8/4k3/8/8/8/4K2R w - - 0 1";
    let first: serde_json::Value = ask(krk).await.unwrap().json().await.unwrap();
    assert_eq!(first["category"], "win");
    assert!(first.get("cached").is_none());
    // The same position at another move number is the same entry.
    let again: serde_json::Value =
        ask("8/8/8/4k3/8/8/8/4K2R w - - 12 40").await.unwrap().json().await.unwrap();
    assert_eq!(again["cached"], true);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(ask(chess_core_start()).await.unwrap().status(), 400, "more than seven pieces");
    env.teardown().await;
}

fn chess_core_start() -> &'static str {
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
}

/// Admins here: made by CHESS_ADMIN_EMAILS (an email the host site vouches
/// for, not one typed into a sign-up), with rights only after a passkey
/// check. They manage roles and partners; a partner's token reads what its
/// scopes say, and stops when revoked.
#[tokio::test]
async fn admins_partners_and_the_staff_check() {
    let Some(env) = setup(|_| {}).await else { return };
    let http = reqwest::Client::new();
    let site_token = |sub: &'static str, name: &'static str, email: &'static str| {
        http.post(format!("{}/api/esports/token", env.base[0]))
            .bearer_auth(SECRET)
            .form(&[("sub", sub), ("name", name), ("email", email), ("scopes", "board:play")])
            .send()
    };
    let t: serde_json::Value =
        site_token("500", "Boss", "Boss@Site.test").await.unwrap().json().await.unwrap();
    let boss = t["token"].as_str().unwrap().to_owned();
    // Listed and vouched for, but no passkey yet: not an admin.
    let role: String = env
        .pg
        .query_one(&format!("SELECT role FROM {}.players WHERE esports_id = '500'", env.chess), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(role, "player");
    let passkey_for = |id_sql: String| {
        let (pg, chess) = (&env.pg, env.chess.clone());
        async move {
            pg.execute(
                &format!(
                    "INSERT INTO {chess}.passkeys (player_id, user_handle, credential_id, public_key, alg, name)
                     SELECT id, '\\x00', decode(md5(random()::text), 'hex'), '\\x00', -7, 'test'
                       FROM {chess}.players WHERE {id_sql}"
                ),
                &[],
            )
            .await
            .unwrap();
        }
    };
    passkey_for("esports_id = '500'".into()).await;
    site_token("500", "Boss", "Boss@Site.test").await.unwrap();
    let me = |token: String| {
        let http = http.clone();
        let url = format!("{}/api/account", env.base[1]);
        async move {
            http.get(url).bearer_auth(token).send().await.unwrap().json::<serde_json::Value>().await.unwrap()
        }
    };
    let v = me(boss.clone()).await;
    assert_eq!(
        (v["roles"].clone(), v["staff"].as_str()),
        (serde_json::json!(["admin"]), Some("pending")),
        "{v}"
    );
    // A sign-up naming the same email isn't vouched for.
    let r = http
        .post(format!("{}/signup", env.base[0]))
        .form(&[("name", "Faker"), ("email", "boss@site.test"), ("password", "a long enough password")])
        .send()
        .await
        .unwrap();
    assert!(r.text().await.unwrap().contains("already has that email"));
    let (gid, gwen) = env.player("Gwen", HUMAN).await;
    assert_eq!(me(gwen.clone()).await["username"], "Gwen");
    let admin_get =
        |path: &str, token: &str| http.get(format!("{}{path}", env.base[0])).bearer_auth(token).send();
    let r = admin_get("/api/admin/players", &boss).await.unwrap();
    assert_eq!(r.status(), 403);
    assert!(r.text().await.unwrap().contains("passkey"));
    assert_eq!(admin_get("/api/admin/players", &gwen).await.unwrap().status(), 403);
    // The passkey check done (passkeys have their own test).
    env.pg
        .execute(
            &format!("UPDATE {}.api_tokens SET staff_verified_at = now() WHERE token_hash = $1", env.chess),
            &[&hash_key(&boss)],
        )
        .await
        .unwrap();
    assert_eq!(me(boss.clone()).await["staff"], "checked");
    let list: serde_json::Value =
        admin_get("/api/admin/players?q=gw", &boss).await.unwrap().json().await.unwrap();
    assert_eq!(list["players"][0]["username"], "Gwen", "{list}");
    let post = |path: String, token: &str, body: serde_json::Value| {
        http.post(format!("{}{path}", env.base[1])).bearer_auth(token).json(&body).send()
    };
    let r =
        post("/api/admin/players/gwen".into(), &boss, serde_json::json!({"manager": true})).await.unwrap();
    assert_eq!(r.status(), 400, "no passkey, no role");
    passkey_for(format!("id = {gid}")).await;
    let r: serde_json::Value =
        post("/api/admin/players/gwen".into(), &boss, serde_json::json!({"manager": true}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!((r["manager"].as_bool(), r["admin"].as_bool()), (Some(true), Some(false)), "{r}");
    let v = me(gwen.clone()).await;
    assert_eq!((v["roles"].clone(), v["staff"].as_str()), (serde_json::json!(["manager"]), Some("pending")));
    let r = post("/api/admin/players/boss".into(), &boss, serde_json::json!({"admin": false})).await.unwrap();
    assert_eq!(r.status(), 400, "not yourself");

    // Partners: a token that reads, and nothing else.
    let p: serde_json::Value = post(
        "/api/admin/partners".into(),
        &boss,
        serde_json::json!({"name": "watcher", "scopes": ["read", "nope"]}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let partner = p["token"].as_str().unwrap().to_owned();
    assert!(partner.starts_with("ocs_"));
    assert_eq!(admin_get("/api/user/Gwen", &partner).await.unwrap().status(), 200);
    assert_eq!(admin_get("/api/user/Gwen", "ocs_nonsense").await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{}/api/user/Gwen", env.base[0])).send().await.unwrap().status(), 401);
    let r = http
        .post(format!("{}/api/esports/token", env.base[0]))
        .bearer_auth(&partner)
        .form(&[("sub", "501")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "not the host site");
    assert_eq!(
        admin_get("/api/admin/players", &partner).await.unwrap().status(),
        401,
        "no account behind it"
    );
    let listed: serde_json::Value =
        admin_get("/api/admin/partners", &boss).await.unwrap().json().await.unwrap();
    assert_eq!(listed["partners"][0]["scopes"], serde_json::json!(["read"]));
    let id = p["id"].as_i64().unwrap();
    post(format!("/api/admin/partners/{id}/revoke"), &boss, serde_json::json!({})).await.unwrap();
    assert_eq!(admin_get("/api/user/Gwen", &partner).await.unwrap().status(), 401);
    // The profile page lives in the Chess lab.
    let r = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{}/@/Gwen", env.base[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["location"], "/app/#@/Gwen");
    env.teardown().await;
}

/// A platform authenticator for the tests: a P-256 key.
struct Authenticator {
    key: aws_lc_rs::signature::EcdsaKeyPair,
    id: Vec<u8>,
    count: u32,
}

impl Authenticator {
    fn new() -> Authenticator {
        use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
        let rng = aws_lc_rs::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref()).unwrap();
        Authenticator { key, id: random_token(16).into_bytes(), count: 0 }
    }

    fn b64(b: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    fn client_data(kind: &str, options: &serde_json::Value) -> Vec<u8> {
        serde_json::json!({"type": kind, "challenge": options["public_key"]["challenge"], "origin": "http://chess.test"})
            .to_string()
            .into_bytes()
    }

    fn auth_data(&mut self, flags: u8, attested: &[u8]) -> Vec<u8> {
        use sha2::Digest as _;
        self.count += 1;
        [&sha2::Sha256::digest(b"chess.test")[..], &[flags], &self.count.to_be_bytes(), attested].concat()
    }

    /// `navigator.credentials.create`'s answer to these options.
    fn create(&mut self, options: &serde_json::Value) -> serde_json::Value {
        use aws_lc_rs::signature::KeyPair as _;
        use ciborium::Value as Cbor;
        let point = self.key.public_key().as_ref().to_vec();
        let i = |n: i64| Cbor::Integer(n.into());
        let cose = Cbor::Map(vec![
            (i(1), i(2)),
            (i(3), i(-7)),
            (i(-1), i(1)),
            (i(-2), Cbor::Bytes(point[1..33].to_vec())),
            (i(-3), Cbor::Bytes(point[33..].to_vec())),
        ]);
        let mut cose_bytes = Vec::new();
        ciborium::into_writer(&cose, &mut cose_bytes).unwrap();
        let len = u16::try_from(self.id.len()).unwrap().to_be_bytes();
        let attested = [&[0u8; 16][..], &len, &self.id, &cose_bytes].concat();
        let ad = self.auth_data(0x01 | 0x04 | 0x40, &attested);
        let mut att = Vec::new();
        ciborium::into_writer(
            &Cbor::Map(vec![
                (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
                (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
                (Cbor::Text("authData".into()), Cbor::Bytes(ad)),
            ]),
            &mut att,
        )
        .unwrap();
        serde_json::json!({
            "challenge_id": options["challenge_id"],
            "client_data_json": Self::b64(&Self::client_data("webauthn.create", options)),
            "attestation_object": Self::b64(&att),
        })
    }

    /// `navigator.credentials.get`'s answer to these options.
    fn get(&mut self, options: &serde_json::Value) -> serde_json::Value {
        use sha2::Digest as _;
        let cd = Self::client_data("webauthn.get", options);
        let ad = self.auth_data(0x01 | 0x04, &[]);
        let signed = [&ad[..], &sha2::Sha256::digest(&cd)[..]].concat();
        let sig = self.key.sign(&aws_lc_rs::rand::SystemRandom::new(), &signed).unwrap();
        serde_json::json!({
            "challenge_id": options["challenge_id"],
            "id": Self::b64(&self.id),
            "client_data_json": Self::b64(&cd),
            "authenticator_data": Self::b64(&ad),
            "signature": Self::b64(sig.as_ref()),
        })
    }
}

/// Passkeys instead of a password: an account made with one, signing in
/// with it, and the passkey check that turns on an admin's rights.
#[tokio::test]
async fn passkeys_make_accounts_sign_in_and_check_staff() {
    let Some(env) = setup(|_| {}).await else { return };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let post =
        |path: &str, body: serde_json::Value| http.post(format!("{}{path}", env.base[0])).json(&body).send();
    let mut key = Authenticator::new();
    let o: serde_json::Value = post("/signup/passkey/options", serde_json::json!({"name": "Pia"}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(o["public_key"]["rp"]["id"], "chess.test", "{o}");
    let mut made = key.create(&o);
    made["return"] = serde_json::json!("/account");
    let r = post("/signup/passkey", made.clone()).await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(set_cookie(&r, "oc_session").is_some());
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["to"], "/account");
    assert_eq!(post("/signup/passkey", made).await.unwrap().status(), 403, "a challenge is used once");
    let name_taken = post("/signup/passkey/options", serde_json::json!({"name": "pia"})).await.unwrap();
    assert_eq!(name_taken.status(), 400);
    // Signing in with it, without a name.
    let o: serde_json::Value =
        post("/auth/passkey/options", serde_json::json!({})).await.unwrap().json().await.unwrap();
    let r = post("/auth/passkey", key.get(&o)).await.unwrap();
    assert_eq!(r.status(), 200);
    let session = set_cookie(&r, "oc_session").unwrap();
    let page = http
        .get(format!("{}/account", env.base[1]))
        .header("cookie", format!("oc_session={session}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Signed in as <b>Pia</b>") && page.contains("Set a password"), "{page}");
    // A session without a passkey check (a stolen cookie, say) can't add a
    // passkey or set a password on an account that has passkeys.
    let bare = random_token(40);
    env.pg
        .execute(
            &format!(
                "INSERT INTO {}.sessions (token_hash, player_id, expires_at)
                 SELECT $1, player_id, now() + interval '1 day' FROM {}.accounts WHERE username = 'Pia'",
                env.chess, env.chess
            ),
            &[&hash_key(&bare)],
        )
        .await
        .unwrap();
    let r = http
        .post(format!("{}/account/passkeys/options", env.base[0]))
        .header("cookie", format!("oc_session={bare}"))
        .header("x-csrf", hash_key(&format!("{bare}|passkeys")))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = http
        .post(format!("{}/account/password", env.base[0]))
        .header("cookie", format!("oc_session={bare}"))
        .form(&[
            ("csrf", hash_key(&format!("{bare}|passkeys")).as_str()),
            ("password", "a stolen session's password"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // The passkey-signed session can.
    let r = http
        .post(format!("{}/account/passkeys/options", env.base[0]))
        .header("cookie", format!("oc_session={session}"))
        .header("x-csrf", hash_key(&format!("{session}|passkeys")))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Another authenticator's answer, or a stale challenge, doesn't sign in.
    let o: serde_json::Value =
        post("/auth/passkey/options", serde_json::json!({})).await.unwrap().json().await.unwrap();
    let mut other = Authenticator::new();
    assert_eq!(post("/auth/passkey", other.get(&o)).await.unwrap().status(), 403);
    // An admin's check, made with a token in an app.
    let pia: i64 = env
        .pg
        .query_one(&format!("SELECT player_id FROM {}.accounts WHERE username = 'Pia'", env.chess), &[])
        .await
        .unwrap()
        .get(0);
    env.pg
        .execute(&format!("UPDATE {}.players SET role = 'admin' WHERE id = $1", env.chess), &[&pia])
        .await
        .unwrap();
    let token = format!("ocp_{}", random_token(36));
    env.pg
        .execute(
            &format!(
                "INSERT INTO {}.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
                 VALUES ($1, 'app', $2, 'ocp_xx', '{{}}', now() + interval '1 day')",
                env.chess
            ),
            &[&pia, &hash_key(&token)],
        )
        .await
        .unwrap();
    let api = |path: &str| http.post(format!("{}{path}", env.base[1])).bearer_auth(&token);
    let admins = || http.get(format!("{}/api/admin/players", env.base[0])).bearer_auth(&token).send();
    assert_eq!(admins().await.unwrap().status(), 403);
    let o: serde_json::Value =
        api("/api/account/passkey/options").send().await.unwrap().json().await.unwrap();
    assert_eq!(o["public_key"]["allowCredentials"].as_array().map(Vec::len), Some(1), "{o}");
    let r = api("/api/account/passkey").json(&key.get(&o)).send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    assert_eq!(admins().await.unwrap().status(), 200);
    env.teardown().await;
}

/// What the iOS app needs: offline games (against the computer, and two
/// people on one device, confirmed by the other), the sync answer, the feed
/// catching late uploads, correspondence, photos, devices, closing an
/// account.
#[tokio::test]
async fn the_ios_app_uploads_syncs_and_closes_accounts() {
    let Some(env) = setup(|c| c.public_delay_plies = 0).await else { return };
    let (_, ann) = env.player("Ann", HUMAN).await;
    let (_, ben) = env.player("Ben", HUMAN).await;
    let http = reqwest::Client::new();
    let base = env.base[0].clone();
    let post = |path: &str, token: &str, body: serde_json::Value| {
        http.post(format!("{base}{path}")).bearer_auth(token).json(&body).send()
    };
    let get = |path: &str, token: &str| http.get(format!("{base}{path}")).bearer_auth(token).send();
    // Both have accounts (as after signing in to the app).
    for t in [&ann, &ben] {
        env.client(0, t).account().await.unwrap();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let three_days_ago = now - 3 * 86_400_000;
    let record = |client_id: &str, mode: &str, black: &str, hints: u32| {
        serde_json::json!({
            "clientId": client_id, "mode": mode, "botLevel": if mode == "computer" { Some(4) } else { None },
            "white": "Ann", "black": black, "speed": "rapid",
            "moves": ["f2f3", "e7e5", "g2g4", "d8h4"],
            "moveTimesMs": [three_days_ago + 1_000, three_days_ago + 2_000, three_days_ago + 3_000, three_days_ago + 4_000],
            "startedMs": three_days_ago, "status": "mate", "winner": "black",
            "assistance": {"hints": hints, "takebacks": 0, "analysed": false}, "ratable": hints == 0,
        })
    };

    // Against the computer, played three days ago: rated now, and in the
    // feed after a cursor taken just before the upload.
    let before_upload = now - 1;
    let r: serde_json::Value = post(
        "/api/games/offline",
        &ann,
        serde_json::json!({"games": [
            record("g1", "computer", "ai-level-4", 0),
            record("g2", "computer", "ai-level-4", 2),
            record("g3", "computer", "ai-level-9", 0),
        ]}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let res = r["results"].as_array().unwrap();
    assert_eq!(res[0]["status"], "rated", "{r}");
    assert!(res[0]["ratingDiff"].as_i64().unwrap() < 0, "Ann lost: {r}");
    assert_eq!(
        (res[1]["status"].as_str(), res[1]["reason"].as_str()),
        (Some("unrated"), Some("help was used"))
    );
    assert_eq!(res[2]["status"], "rejected");
    let g1 = res[0]["gameId"].as_str().unwrap().to_owned();
    let again: serde_json::Value = post(
        "/api/games/offline",
        &ann,
        serde_json::json!({"games": [record("g1", "computer", "ai-level-4", 0)]}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        (again["results"][0]["status"].as_str(), again["results"][0]["gameId"].as_str()),
        (Some("duplicate"), Some(g1.as_str()))
    );
    let exported = env.client(0, &ann).export_game(&g1).await.unwrap();
    assert!(exported.rated && exported.players.black.rating_diff == Some(0), "the computer keeps its rating");
    let mut feed = sse(http
        .get(format!("{base}/api/feed?only=end&since={before_upload}"))
        .bearer_auth(&ann)
        .send()
        .await
        .unwrap());
    let (kind, sid, v) = next_sse(&mut feed).await;
    assert_eq!(
        (kind.as_str(), v["id"].as_str()),
        ("end", Some(g1.as_str())),
        "a late upload still reaches the feed"
    );
    assert!(sid.unwrap().parse::<i64>().unwrap() >= before_upload, "its id is when it was recorded");
    assert!(v["lastMoveAt"].as_i64().unwrap() < before_upload, "though it was played days ago");

    // Two people on one device: waits for Ben, who confirms it.
    let r: serde_json::Value = post(
        "/api/games/offline",
        &ann,
        serde_json::json!({"games": [record("p1", "passAndPlay", "Ben", 0)]}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(r["results"][0]["status"], "pending", "{r}");
    let pending = r["results"][0]["gameId"].as_str().unwrap().to_owned();
    let s: serde_json::Value = get("/api/sync?since=0", &ben).await.unwrap().json().await.unwrap();
    assert_eq!(s["confirmations"][0]["id"].as_str(), Some(pending.as_str()), "{s}");
    assert_eq!(s["confirmations"][0]["uploadedBy"], "Ann");
    let r = post(&format!("/api/games/{pending}/confirm"), &ann, serde_json::json!({"accept": true}))
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "only Ben confirms");
    let c: serde_json::Value =
        post(&format!("/api/games/{pending}/confirm"), &ben, serde_json::json!({"accept": true}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(c["status"], "rated", "{c}");
    assert!(c["ratingDiff"].as_i64().unwrap() > 0, "Ben won: {c}");
    let r = post(&format!("/api/games/{pending}/confirm"), &ben, serde_json::json!({"accept": true}))
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "once");

    // Sync: ratings and finished games, gzipped when asked.
    let r = http
        .get(format!("{base}/api/sync?since={before_upload}"))
        .bearer_auth(&ann)
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers().get("content-encoding").and_then(|v| v.to_str().ok()), Some("gzip"));
    let s: serde_json::Value =
        get(&format!("/api/sync?since={before_upload}"), &ann).await.unwrap().json().await.unwrap();
    assert!(s["ratings"]["rapid"]["rating"].as_f64().unwrap() < 1500.0, "{s}");
    let finished: Vec<&str> =
        s["finished"].as_array().unwrap().iter().filter_map(|f| f["gameId"].as_str()).collect();
    assert!(finished.contains(&g1.as_str()) && finished.contains(&pending.as_str()), "{s}");

    // Correspondence: three days a move, the clock in days.
    let ann_c = env.client(0, &ann);
    let ben_id = env.client(0, &ben).account().await.unwrap().id;
    let r: serde_json::Value = http
        .post(format!("{base}/api/challenge/{ben_id}"))
        .bearer_auth(&ann)
        .form(&[("days", "3"), ("rated", "true"), ("color", "white"), ("acceptByToken", ben.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let corr = r["id"].as_str().or(r["game"]["id"].as_str()).expect("a game").to_owned();
    ann_c.make_move(Api::Board, &corr, "e2e4", false).await.unwrap();
    let s: serde_json::Value = get("/api/sync", &ben).await.unwrap().json().await.unwrap();
    let g = s["playing"].as_array().unwrap().iter().find(|g| g["gameId"] == corr.as_str()).expect("playing");
    assert_eq!(
        (g["daysPerTurn"].as_i64(), g["isMyTurn"].as_bool(), g["speed"].as_str()),
        (Some(3), Some(true), Some("correspondence"))
    );
    assert!(g["secondsLeft"].as_i64().unwrap() > 3 * 86_400 - 60, "{g}");

    // Photos: only under the licence, once per app id.
    let photo = serde_json::json!({"clientId": "ph1", "image": "/9j/4AAQSkZJRgABAQAAAQABAAD/2Q==",
                                   "placement": "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR", "confidence": 0.9, "model": "test"});
    assert_eq!(post("/api/photos", &ann, photo.clone()).await.unwrap().status(), 403);
    post("/api/esl/photo-terms", &ann, serde_json::json!({"accept": true})).await.unwrap();
    let a: serde_json::Value = post("/api/photos", &ann, photo.clone()).await.unwrap().json().await.unwrap();
    let b: serde_json::Value = post("/api/photos", &ann, photo).await.unwrap().json().await.unwrap();
    assert!(a["id"].is_i64() && a["id"] == b["id"], "{a} {b}");

    // Devices for push.
    let dev = "a".repeat(64);
    assert!(
        post("/api/push/devices", &ben, serde_json::json!({"token": dev, "sandbox": true}))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        post("/api/push/devices", &ben, serde_json::json!({"token": "nope"})).await.unwrap().status(),
        400
    );

    env.client(0, &ben).make_move(Api::Board, &corr, "e7e5", false).await.unwrap();
    // Closing Ben's account: his game in progress is resigned, his name
    // goes, his token stops working.
    let r = http.delete(format!("{base}/api/account")).bearer_auth(&ben).send().await.unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(get("/api/account", &ben).await.unwrap().status(), 401);
    assert_eq!(
        http.get(format!("{base}/api/user/Ben")).bearer_auth(&ann).send().await.unwrap().status(),
        404
    );
    let g = ann_c.export_game(&corr).await.unwrap();
    assert_eq!((g.status, g.winner), (Status::Resign, Some(Side::White)));
    env.teardown().await;
}

/// The app signs in with a reversed-domain redirect (RFC 8252).
#[tokio::test]
async fn native_apps_sign_in_with_their_own_scheme() {
    let Some(env) = setup(|_| {}).await else { return };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let r = http
        .get(format!(
            "{}/oauth?response_type=code&client_id=open-chess-ios&redirect_uri=org.offenedatenmodellierung.openchess:/oauth&code_challenge={challenge}&code_challenge_method=S256&scope=board:play",
            env.base[0]
        ))
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), 400, "accepted (sign-in or consent comes next)");
    env.teardown().await;
}
