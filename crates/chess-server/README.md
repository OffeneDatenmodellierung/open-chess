# open-chess server

A Rust chess server that speaks **the Lichess API**: the Board, Bot,
Challenge and OAuth (PKCE) endpoints, the event, game and move streams, and
game export, with Lichess's JSON. Anything written for Lichess talks to it
by changing the base URL: a host site's Lichess backend, Lichess Board API
apps, and
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot).

It shares a host site's Postgres database: players, API tokens and
sessions stay where the site keeps them (`CHESS_SITE_SCHEMA`; see
"Plugging into a site" below). The server adds a
`chess` schema only for what the Lichess flows need: challenges, games,
moves and clocks, ratings and fair play.

**Database rights.** The role needs to create and write the `chess` schema.
In the site's schema it mostly reads, but it also writes in four places:
* `api_tokens.last_used_at` on use;
* revocations (`DELETE /api/token`);
* the tokens OAuth issues;
* the players and tokens `open-chess bot create` makes.

A read-only role there won't do.

```
Lichess client / bot / site ──▶ open-chess × N ──▶ Postgres
                                  │   ▲             site.*     players, api_tokens, sessions (see rights below)
                                  │   └─ LISTEN ──  chess.*    games, challenges, ratings, fair play
fair-play worker × M (Stockfish) ─┘
```

## Crates

The server is the top of a stack; the lower crates are what a bot (for
this server or for lichess.org) reuses.

| Crate | What |
|---|---|
| `chess-core` | Rules on [cozy-chess](https://crates.io/crates/cozy-chess) (MIT): legal moves, SAN, standard-castling UCI, threefold / fifty-move / insufficient-material draws, Fischer clocks, Lichess status codes and speeds, PGN, Glicko-2. No I/O. |
| `chess-proto` | Lichess's JSON as Rust types (events, challenges, users, exports), checked against Lichess's examples. |
| `chess-client` | Async client for the Board, Bot and Challenge APIs, with NDJSON streams. Works against lichess.org or this server. |
| `chess-engine` | Async UCI driver (Stockfish), behind an `Engine` trait. |
| `chess-analysis` | The training app's engine, pure Rust and wasm-safe: alpha-beta search, win/draw/loss odds, move explanations, tactical motifs, openings, 3-piece tablebases, photo matching, NNUE evaluation. See [docs/engine.md](../../docs/engine.md). |
| `chess-wasm` | `chess-analysis` for the browser (the PWA in `web/chess`). |
| `chess-fairplay` | Post-game screening: accuracy, ACPL, engine-match rate, move-time patterns, the training-app fingerprint, a 0–100 score. |
| `chess-server` | This server (`open-chess`). |

`shakmaty`, the usual Rust chess crate, is GPL-3.0, which `deny.toml`
doesn't allow, so cozy-chess handles move generation and the rest is
written here.

## API

The endpoints (the Lichess API, live game streams, OAuth and scopes, and
open-chess's own additions) are in [docs/api.md](../../docs/api.md); using the
server with Lichess tools and bots is in [docs/lichess.md](../../docs/lichess.md).

## Anti-cheat

Bots are allowed and openly marked (`title: BOT`). For people, these
checks apply:

* **Separate APIs.** Bot accounts can't play through the Board API, and a
  human account can only become a bot before it has played a game.
* **No live feed.** The public move stream runs three plies behind live
  games. Game streams are for the two players only.
* **Post-game screening.** When a game with a human player ends (20 plies
  or more), it's queued for the worker (`open-chess analyse`). Stockfish
  evaluates every position at multi-PV 3. Each human side then gets:
  * accuracy and ACPL, on Lichess's win-percentage scale;
  * how often their move was the engine's first, or top three, choice
    (opening, forced moves and decided positions are left out);
  * how even their move times were;
  * whether they spent longer on critical positions (people do);
  * the **training-app fingerprint**: in the positions where Stockfish and
    the training app's own engine disagree on the best move, which one the
    player chose. Honest players choose either or neither; someone copying
    the app's suggestions chooses the app's move almost every time.

  A screening model folds these into a 0–100 score and decides when a
  game is flagged (and when the fingerprint alone is enough). The model
  is data, `chess_fairplay::Model` as JSON, given in
  `CHESS_FAIRPLAY_MODEL` (a file path or the JSON itself). Keep a tuned
  model private: its numbers tell people what to avoid. Without one the
  worker uses an untuned example and says so in its log.
* **Human review, never automatic bans.** Flagged games wait in
  `/api/fairplay/queue`. A reviewer's `engine` verdict marks the account:
  `/api/user` then shows `tosViolation`, and with `CHESS_BLOCK_MARKED` on
  the account can't start new games. A `clean` verdict lifts the mark. The
  host site decides what happens next: `chess.accounts.marked_at`
  and `chess.fairplay_reports` are there to read.

The score is a screening heuristic, not proof: strong players have accurate
games. Tune the model on your own players before relying on it.

**Assistance labels.** The training app can switch its engine help on
during games on this server (never on Lichess, where it's cheating). When
it does, it says so: `POST /api/board/game/{id}/assist` with `on=true` or
`false` goes into `chess.assist_marks`, and the game's reports carry
`declaredAssist`. This is ground truth for training the screening model
on known assisted and unassisted games; screening itself never relies on
it, because a cheating client wouldn't send it.

## Scaling

Every request and every clock decision goes through Postgres:
* A move locks its game row, validates and applies the move with
  chess-core, charges the clock, writes, and sends `pg_notify` in one
  transaction.
* The database clock (`clock_timestamp()`) is the only clock, so replicas
  never disagree about time used.
* Each replica keeps one LISTEN connection (on Neon's direct host,
  derived from the pooled one) and passes changes to its own open streams.
  A move sent to one replica reaches a stream held by another.
* A sweeper on every replica flags clocks, aborts unstarted games and
  lapses challenges. It claims rows with `FOR UPDATE SKIP LOCKED`, so
  replicas don't collide.

So the API scales by adding replicas (HPA on Kubernetes, `CHESS_INSTANCES`
on Cloudflare), and fair play by adding workers. Each player may hold 8
event streams and 40 game streams per replica.

## Configuration

Everything is environment (each variable is also a command-line flag:
`CHESS_PUBLIC_URL` is `--public-url`; `open-chess serve --help` lists them).

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | — | The site's database (pooled host fine) |
| `CHESS_LISTEN_DATABASE_URL` | derived | For LISTEN, if the direct host can't be derived from a `-pooler` host |
| `CHESS_SITE_SCHEMA` / `CHESS_SCHEMA` | `esports` / `chess` | |
| `CHESS_PUBLIC_URL` | `http://localhost:8090` | Where clients reach the API; a path in it (e.g. `/chess`) is where the API is mounted |
| `ESPORTS_PUBLIC_URL` | `http://localhost:8080` | The site (sign-in link on the consent page) |
| `ESPORTS_LOGIN` | `false` | Browser sign-in through the site: the OAuth consent page accepts its session cookie. Off, there's no OAuth sign-in (API tokens work either way) and `GET /` says `"login": false` |
| `CHESS_SITE_NAME` | `open-chess` | What players know the site as: the consent page, PGN event names, `GET /` (the app shows it) |
| `CHESS_BIND` / `CHESS_METRICS_BIND` | `0.0.0.0:8090` / `0.0.0.0:9465` | API; `/metrics`, `/healthz`, `/readyz` |
| `CHESS_POOL_SIZE` | 8 | |
| `CHESS_SWEEP_MS` | 500 | How often each replica's sweeper looks for flags, unstarted games and lapsed challenges |
| `CHESS_APP_DIR` | — (the image sets `/usr/share/open-chess/app`) | The Chess lab app's files, served at `/app/` |
| `CHESS_APP_THEME` | — | The app's look: CSS, or a `.css` file's path, served as `/app/theme.css` ([docs/api.md](../../docs/api.md#the-chess-labs-look)) |
| `CHESS_TABLEBASE_URL` | `https://tablebase.lichess.ovh` | A Lichess-compatible tablebase server for `/api/esl/tablebase`; empty turns it off |
| `CHESS_LAG_COMP_MS` | 100 | Given back per move for the round trip |
| `CHESS_CHALLENGE_TTL_SECS` | 20 | Real-time challenges lapse without `keepAliveStream` |
| `CHESS_FIRST_MOVE_SECS` | 30 | |
| `CHESS_GONE_AFTER_SECS` / `CHESS_CLAIM_WIN_SECS` | 20 / 10 | Opponent gone, then the win (or draw) can be claimed |
| `CHESS_PUBLIC_DELAY_PLIES` | 3 | |
| `CHESS_FAIRPLAY` / `CHESS_BLOCK_MARKED` | true / true | |
| `CHESS_STOCKFISH` | found | Worker: engine path |
| `CHESS_ANALYSIS_DEPTH` / `_THREADS` / `_HASH_MB` | 14 / 1 / 64 | Worker |
| `CHESS_FAIRPLAY_SKIP_PLIES` | 16 | Worker: opening plies left out of screening |
| `CHESS_FAIRPLAY_MODEL` | example model | Worker: the screening model (JSON file path or inline JSON). Keep yours private |
| `CHESS_FAIRPLAY_FLAG_SCORE` / `_MIN_MOVES` | from the model | Worker: override the model's flag score and minimum moves |
| `CHESS_FAIRPLAY_APP_DEPTH` / `_APP_NODES` | 6 / 400000 | Worker: the training app's "Normal" search, for the fingerprint. Keep in step with `web/chess/app.js` |
| `CHESS_FAIRPLAY_APP_NET` | — | Worker: the NNUE file the app ships (`web/chess/nets/default.nnue`), if any |
| `CHESS_VISION_ENDPOINT` / `_MODEL` / `_API_KEY` | — / `qwen3-vl:8b` / — | Photo reading: an OpenAI-style `/chat/completions` vision model (Claude at `https://api.anthropic.com/v1`, or Ollama) |
| `CHESS_VISION_LOCAL_MODEL` | — | Our board reader (`crates/chess-vision`): a `.safetensors` file from `chess-vision-train`, run on this server's CPU before any vision model is asked |
| `CHESS_VISION_MIN_CONFIDENCE` | `0.5` | The reader's answer is used without review when no square is unsure and the chance all 64 are right is at least this |
| `CHESS_VISION_REVIEW` / `_REVIEW_MAX` | `true` / `5000` | Keep photos that weren't read for sure for the Review page (at most this many waiting); `open-chess photos export --out DIR` writes the confirmed ones as a training set |
| `RUST_LOG` | `info` | Log filter (`debug`, `open_chess_server=debug` …) |

## Running

```sh
open-chess serve             # the API (runs migrations first); also what `open-chess` alone does
open-chess analyse           # the fair-play worker (and, when idle, the dictionary's photo positions)
open-chess analyse --once    # screen what's waiting, then exit
open-chess migrate           # schema only
open-chess bot create --name stockfish-1800 [--days 365]   # prints the bot's token once
open-chess bot list
open-chess photos export --out DIR [--reviewed-only]      # confirmed photos, for chess-vision-train
open-chess healthcheck       # exit 0 if this machine's server answers /healthz (the image's HEALTHCHECK)
```

Get it from a [release](../../docs/releases.md): the binary for Linux, or
the image `ghcr.io/offenedatenmodellierung/open-chess` (built by the
`Dockerfile` from the release's binaries): one image for both roles
(Stockfish included), and `:slim`, distroless with the API and the app but
no Stockfish. To deploy:

* **Kubernetes:** `deploy/k8s/chess.yaml` (an example).
* **Cloudflare:** `deploy/cloudflare` (Containers behind a Worker; an example).

## Plugging into a site

The server keeps no accounts of its own yet: it reads players, API tokens
and sessions from a host site's schema (`CHESS_SITE_SCHEMA`), which needs
`players`, `api_tokens` and `sessions` tables. esports-lab, the
(private) application this server was extracted from, is the reference
host, and its deployment sets `ESPORTS_LOGIN=true` so people sign in to
apps through it. A standalone mode with its own accounts is planned.

There, the site's `native` chess backend is a Lichess client pointed at
this server. The site's watcher mints
both players a short-lived site API token per game (`board:play`,
`challenge:read`, `challenge:write`, named `chess match {fixture}`), which
this server accepts because it reads the same `api_tokens`, and
revokes them when the game ends. Players link nothing; fixtures keep
`backend = 'native'`, the game id in `external_id`, and the competition
state in the site's tables.

Players play on the site's own board page (`#/play/{fixture}`), which asks
the site for its token (`GET /api/me/chess/play/{fixture}`) and then talks
to this server's Board API directly from the browser (`[chess]
native_public_url`). Any other Board API client works too.

## Tests

```sh
cargo test -p open-chess-core -p open-chess-proto -p open-chess-client \
           -p open-chess-engine -p open-chess-fairplay -p open-chess-server
# End to end: two replicas on one Postgres, games, clocks, OAuth, bots, fair play
CHESS_TEST_DATABASE_URL=postgres://…?sslmode=disable cargo test -p open-chess-server --test e2e
```

CI runs the end-to-end suite against a Postgres service with Stockfish
installed.
