//! Check the chess server's JSON against Lichess's own OpenAPI spec.
//!
//! Drives a running open-chess server through a full Board API game, challenges
//! (accepted, declined, cancelled), the event, game and move streams,
//! exports, token test and bot listing, and validates every response and
//! stream line against the schemas in github.com/lichess-org/api
//! (`doc/specs`).
//!
//! ```sh
//! git clone --depth 1 https://github.com/lichess-org/api /tmp/lichess-api
//! cargo run -p open-chess-conformance -- --base http://127.0.0.1:8090 \
//!     --spec /tmp/lichess-api/doc/specs \
//!     --db postgres://dev:dev@127.0.0.1/chessdev --site-schema esports
//! ```
//!
//! It makes three throwaway players (with tokens) in the site schema, and
//! prints one line per check; exit status 1 if any fail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use futures_util::StreamExt;
use jsonschema::{Retrieve, Uri};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "chess-conformance", about = "Check an open-chess server against Lichess's OpenAPI spec")]
struct Args {
    /// The running chess server, e.g. http://127.0.0.1:8090.
    #[arg(long)]
    base: String,
    /// lichess-org/api's doc/specs folder.
    #[arg(long)]
    spec: PathBuf,
    /// The site's database (players and tokens are made there).
    #[arg(long)]
    db: String,
    #[arg(long, default_value = "esports")]
    site_schema: String,
}

/// YAML files as JSON, by `file://` URI, so `$ref`s between them resolve.
struct YamlFiles(Mutex<HashMap<String, Value>>);

fn load_yaml(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_norway::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

impl Retrieve for YamlFiles {
    fn retrieve(&self, uri: &Uri<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let s = uri.as_str();
        if let Some(v) = self.0.lock().map_err(|_| "lock")?.get(s) {
            return Ok(v.clone());
        }
        let path = s.strip_prefix("file://").ok_or_else(|| format!("only file:// refs: {s}"))?;
        let v = load_yaml(Path::new(path)).map_err(|e| e.to_string())?;
        self.0.lock().map_err(|_| "lock")?.insert(s.to_owned(), v.clone());
        Ok(v)
    }
}

struct Spec {
    root: PathBuf,
    fails: Vec<String>,
}

/// A schema and the file it came from (its `$ref`s are relative to it).
struct Schema(Value, PathBuf);

impl Spec {
    fn schema(&self, name: &str) -> Result<Schema> {
        let p = self.root.join("schemas").join(format!("{name}.yaml"));
        Ok(Schema(load_yaml(&p)?, p))
    }

    fn response(&self, tag_file: &str, method: &str, ctype: &str) -> Result<Schema> {
        let p = self.root.join("tags").join(tag_file);
        let op = load_yaml(&p)?;
        let s = op[method]["responses"]["200"]["content"][ctype]["schema"].clone();
        if s.is_null() {
            bail!("{tag_file}: no {method} 200 {ctype} schema");
        }
        Ok(Schema(s, p))
    }

    fn check(&mut self, label: &str, instance: &Value, schema: &Schema) {
        let base = format!("file://{}", schema.1.display());
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_base_uri(base)
            .with_retriever(YamlFiles(Mutex::new(HashMap::new())))
            .build(&schema.0);
        let errors: Vec<String> = match &validator {
            Ok(v) => {
                v.iter_errors(instance).take(5).map(|e| format!("at {}: {e}", e.instance_path())).collect()
            }
            Err(e) => vec![format!("the schema itself: {e}")],
        };
        if errors.is_empty() {
            println!("ok   {label}");
        } else {
            println!("FAIL {label}");
            for e in errors {
                println!("     {}", e.chars().take(200).collect::<String>());
            }
            self.fails.push(label.to_owned());
        }
    }

    fn fail(&mut self, label: &str, why: &str) {
        println!("FAIL {label}: {why}");
        self.fails.push(label.to_owned());
    }
}

/// A throwaway player with a token, in the site schema.
async fn make_player(
    pg: &tokio_postgres::Client,
    schema: &str,
    name: &str,
    scopes: &[&str],
) -> Result<String> {
    let id: i64 = pg
        .query_one(&format!("INSERT INTO {schema}.players (name) VALUES ($1) RETURNING id"), &[&name])
        .await?
        .get(0);
    let token = format!("esl_{}", hex::encode(rand::random::<[u8; 18]>()));
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
    pg.execute(
        &format!(
            "INSERT INTO {schema}.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
             VALUES ($1, 'conformance', $2, 'esl_xx', $3, now() + interval '1 hour')"
        ),
        &[&id, &hash, &scopes],
    )
    .await?;
    Ok(token)
}

/// An NDJSON stream read into a channel, keep-alives skipped.
struct Stream {
    url: String,
    rx: mpsc::UnboundedReceiver<Value>,
}

impl Stream {
    fn open(http: &reqwest::Client, url: String, token: Option<&str>) -> Stream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = http.get(&url).timeout(Duration::from_mins(2));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        tokio::spawn(async move {
            let Ok(r) = req.send().await else { return };
            let mut body = r.bytes_stream();
            let mut buf = Vec::new();
            while let Some(Ok(chunk)) = body.next().await {
                buf.extend_from_slice(&chunk);
                while let Some(i) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    if let Ok(v) = serde_json::from_slice::<Value>(&line)
                        && tx.send(v).is_err()
                    {
                        return;
                    }
                }
            }
        });
        Stream { url, rx }
    }

    /// The next line (of `kind`, if given), within ten seconds.
    async fn next(&mut self, kind: Option<&str>) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let v = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .ok()
                .flatten()
                .ok_or_else(|| anyhow!("no {} on {}", kind.unwrap_or("line"), self.url))?;
            if kind.is_none_or(|k| v["type"] == k) {
                return Ok(v);
            }
        }
    }
}

struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    fn req(&self, method: reqwest::Method, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let r = self.http.request(method, format!("{}{path}", self.base)).timeout(Duration::from_secs(20));
        match token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Result<Value> {
        Ok(self.req(reqwest::Method::GET, path, token).send().await?.error_for_status()?.json().await?)
    }

    async fn post(
        &self,
        path: &str,
        token: Option<&str>,
        form: &[(&str, &str)],
    ) -> Result<reqwest::Response> {
        Ok(self.req(reqwest::Method::POST, path, token).form(form).send().await?)
    }

    async fn post_json(&self, path: &str, token: Option<&str>, form: &[(&str, &str)]) -> Result<Value> {
        Ok(self.post(path, token, form).await?.error_for_status()?.json().await?)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let failed = run(a).await?;
    std::process::exit(i32::from(failed));
}

async fn run(a: Args) -> Result<bool> {
    let mut spec = Spec { root: a.spec.canonicalize().context("--spec")?, fails: Vec::new() };
    let (pg, conn) = tokio_postgres::connect(&a.db, tokio_postgres::NoTls).await.context("--db")?;
    tokio::spawn(conn);
    let api = Api { http: reqwest::Client::new(), base: a.base.trim_end_matches('/').to_owned() };
    let tag = hex::encode(rand::random::<[u8; 3]>());
    let human = ["board:play", "challenge:read", "challenge:write", "email:read"];
    let ta = make_player(&pg, &a.site_schema, &format!("Confa{tag}"), &human).await?;
    let tb = make_player(&pg, &a.site_schema, &format!("Confb{tag}"), &human).await?;
    let tbot = make_player(
        &pg,
        &a.site_schema,
        &format!("Confbot{tag}"),
        &["bot:play", "challenge:read", "challenge:write"],
    )
    .await?;
    let (ta, tb, tbot) = (ta.as_str(), tb.as_str(), tbot.as_str());

    let me = api.get("/api/account", Some(ta)).await?;
    spec.check("GET /api/account", &me, &spec.schema("UserExtended")?);
    let ua = me["id"].as_str().context("account id")?.to_owned();
    let ub = api.get("/api/account", Some(tb)).await?["id"].as_str().context("id")?.to_owned();
    let bot = api.get("/api/account", Some(tbot)).await?["id"].as_str().context("id")?.to_owned();
    api.post("/api/bot/account/upgrade", Some(tbot), &[]).await?.error_for_status()?;
    let user = api.get(&format!("/api/user/{ua}"), None).await?;
    spec.check("GET /api/user/{username}", &user, &spec.schema("UserExtended")?);

    let mut ev_a = Stream::open(&api.http, format!("{}/api/stream/event", api.base), Some(ta));
    let mut ev_b = Stream::open(&api.http, format!("{}/api/stream/event", api.base), Some(tb));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let event_schema = spec.response("board/api-stream-event.yaml", "get", "application/x-ndjson")?;

    // A challenge, listed, received and accepted.
    let ch = api
        .post_json(
            &format!("/api/challenge/{ub}"),
            Some(ta),
            &[("clock.limit", "180"), ("clock.increment", "2"), ("color", "white")],
        )
        .await?;
    spec.check("POST /api/challenge/{username}", &ch, &spec.schema("ChallengeJson")?);
    let gid = ch["id"].as_str().context("challenge id")?.to_owned();
    let list = api.get("/api/challenge", Some(tb)).await?;
    spec.check(
        "GET /api/challenge",
        &list,
        &spec.response("challenges/api-challenge.yaml", "get", "application/json")?,
    );
    spec.check("event stream: challenge", &ev_b.next(Some("challenge")).await?, &event_schema);
    let shown = api.get(&format!("/api/challenge/{gid}/show"), Some(tb)).await?;
    spec.check("GET /api/challenge/{id}/show", &shown, &spec.schema("ChallengeJson")?);
    let ok = api.post_json(&format!("/api/challenge/{gid}/accept"), Some(tb), &[]).await?;
    spec.check("POST /api/challenge/{id}/accept", &ok, &spec.schema("Ok")?);
    spec.check("event stream: gameStart", &ev_a.next(Some("gameStart")).await?, &event_schema);

    let board_schema =
        spec.response("board/api-board-game-stream-gameId.yaml", "get", "application/x-ndjson")?;
    let mut gs = Stream::open(&api.http, format!("{}/api/board/game/stream/{gid}", api.base), Some(ta));
    spec.check("board stream: gameFull", &gs.next(Some("gameFull")).await?, &board_schema);
    api.post(&format!("/api/board/game/{gid}/move/e2e4"), Some(ta), &[]).await?.error_for_status()?;
    spec.check("board stream: gameState", &gs.next(Some("gameState")).await?, &board_schema);
    let err = api.post(&format!("/api/board/game/{gid}/move/e2e4"), Some(ta), &[]).await?;
    let code = err.status().as_u16();
    let body: Value = err.json().await.unwrap_or(Value::Null);
    spec.check(&format!("move out of turn ({code})"), &body, &spec.schema("Error")?);
    api.post(&format!("/api/board/game/{gid}/chat"), Some(tb), &[("room", "player"), ("text", "good luck")])
        .await?
        .error_for_status()?;
    spec.check("board stream: chatLine", &gs.next(Some("chatLine")).await?, &board_schema);
    api.post(&format!("/api/board/game/{gid}/move/e7e5"), Some(tb), &[]).await?.error_for_status()?;
    gs.next(Some("gameState")).await?;
    let playing = api.get("/api/account/playing", Some(ta)).await?;
    spec.check(
        "GET /api/account/playing",
        &playing,
        &spec.response("games/api-account-playing.yaml", "get", "application/json")?,
    );
    let chat = api.get(&format!("/api/board/game/{gid}/chat"), Some(ta)).await?;
    spec.check(
        "GET /api/board/game/{id}/chat",
        &chat,
        &spec.response("board/api-board-game-gameId-chat.yaml", "get", "application/json")?,
    );

    let mut ms = Stream::open(&api.http, format!("{}/api/stream/game/{gid}", api.base), None);
    let move_schema = spec.schema("MoveStreamEntry")?;
    spec.check("move stream: description", &ms.next(None).await?, &move_schema);
    spec.check("move stream: position", &ms.next(None).await?, &move_schema);

    api.post(&format!("/api/board/game/{gid}/resign"), Some(tb), &[]).await?.error_for_status()?;
    spec.check("board stream: final gameState", &gs.next(Some("gameState")).await?, &board_schema);
    spec.check("event stream: gameFinish", &ev_a.next(Some("gameFinish")).await?, &event_schema);
    let exp: Value = api
        .req(reqwest::Method::GET, &format!("/game/export/{gid}?pgnInJson=true&clocks=true"), None)
        .header("Accept", "application/json")
        .send()
        .await?
        .json()
        .await?;
    spec.check("GET /game/export/{id} (json)", &exp, &spec.schema("GameJson")?);
    let pgn =
        api.req(reqwest::Method::GET, &format!("/game/export/{gid}"), None).send().await?.text().await?;
    if pgn.starts_with("[Event ") {
        println!("ok   GET /game/export/{{id}} (pgn)");
    } else {
        spec.fail("GET /game/export/{id} (pgn)", "not PGN");
    }

    // Declined and cancelled.
    let ch2 = api
        .post_json(
            &format!("/api/challenge/{ub}"),
            Some(ta),
            &[("clock.limit", "60"), ("clock.increment", "0")],
        )
        .await?;
    ev_b.next(Some("challenge")).await?;
    let id2 = ch2["id"].as_str().context("id")?;
    api.post(&format!("/api/challenge/{id2}/decline"), Some(tb), &[("reason", "later")])
        .await?
        .error_for_status()?;
    spec.check(
        "event stream: challengeDeclined",
        &ev_a.next(Some("challengeDeclined")).await?,
        &event_schema,
    );
    let ch3 = api
        .post_json(
            &format!("/api/challenge/{ub}"),
            Some(ta),
            &[("clock.limit", "60"), ("clock.increment", "0")],
        )
        .await?;
    ev_b.next(Some("challenge")).await?;
    let id3 = ch3["id"].as_str().context("id")?;
    api.post(&format!("/api/challenge/{id3}/cancel"), Some(ta), &[]).await?.error_for_status()?;
    spec.check(
        "event stream: challengeCanceled",
        &ev_b.next(Some("challengeCanceled")).await?,
        &event_schema,
    );

    let tt: Value = api
        .req(reqwest::Method::POST, "/api/token/test", None)
        .header("Content-Type", "text/plain")
        .body(format!("{ta},esl_nope"))
        .send()
        .await?
        .json()
        .await?;
    spec.check(
        "POST /api/token/test",
        &tt,
        &spec.response("oauth/api-token-test.yaml", "post", "application/json")?,
    );
    let _ev_bot = Stream::open(&api.http, format!("{}/api/stream/event", api.base), Some(tbot));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = api.req(reqwest::Method::GET, "/api/bot/online", None).send().await?.text().await?;
    let online: Vec<Value> =
        text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    if let Some(u) = online.first() {
        spec.check("GET /api/bot/online", u, &spec.schema("UserExtended")?);
    }
    if !online.iter().any(|u| u["id"] == json!(bot)) {
        spec.fail("GET /api/bot/online", &format!("{bot} not listed"));
    }

    if spec.fails.is_empty() {
        println!("\nall checks passed");
    } else {
        println!("\n{} failed", spec.fails.len());
    }
    Ok(!spec.fails.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schema `$ref`-ing another YAML file: references resolve, and a
    /// wrong instance fails.
    #[test]
    fn yaml_schemas_with_refs_validate() {
        let dir =
            std::env::temp_dir().join(format!("conformance-{}", hex::encode(rand::random::<[u8; 4]>())));
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::write(
            dir.join("schemas/User.yaml"),
            "type: object\nproperties:\n  id:\n    type: string\n  perfs:\n    $ref: './Perfs.yaml'\nrequired: [id]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("schemas/Perfs.yaml"),
            "type: object\nadditionalProperties:\n  type: integer\n",
        )
        .unwrap();
        let mut spec = Spec { root: dir.clone(), fails: Vec::new() };
        let user = spec.schema("User").unwrap();
        spec.check("good", &json!({"id": "ann", "perfs": {"blitz": 1500}}), &user);
        assert!(spec.fails.is_empty());
        spec.check("missing id", &json!({"perfs": {}}), &user);
        spec.check("bad ref'd value", &json!({"id": "ann", "perfs": {"blitz": "fast"}}), &user);
        assert_eq!(spec.fails, ["missing id", "bad ref'd value"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
