# The open-chess server API

The server speaks **the Lichess API**: the same paths, parameters, JSON and
NDJSON streams, documented in [Lichess's API reference](https://lichess.org/api).
Anything written for Lichess works by changing the base URL (`https://lichess.org`
→ your server). This page lists what is implemented, where it differs, and the
endpoints open-chess adds. For using it with Lichess tools see
[lichess.md](lichess.md); for electronic boards see [boards.md](boards.md).

Requests authenticate with `Authorization: Bearer <token>`. Scopes are
Lichess's (below). Streams are NDJSON (one JSON object per line, a blank line
every few seconds to keep the connection open); read them with `curl -N`, or
`fetch()` and a line reader in a browser.

## Endpoints

| Area | Endpoints |
|---|---|
| OAuth | `GET/POST /oauth` (consent page), `POST /api/token`, `DELETE /api/token`, `POST /api/token/test` |
| Account | `GET /api/account`, `/api/account/email`, `/api/account/playing`, `/api/user/{username}` |
| Bots | `POST /api/bot/account/upgrade`, `GET /api/bot/online` |
| Streams | `GET /api/stream/event`, `/api/board/game/stream/{id}`, `/api/bot/game/stream/{id}`, `/api/stream/game/{id}` (see [Live games](#live-games)) |
| Challenges | `POST /api/challenge/{username}` (with `keepAliveStream`, `rules` and `acceptByToken`), `GET /api/challenge`, `GET /api/challenge/{id}/show`, `POST …/{id}/accept`, `…/decline`, `…/cancel` |
| Playing | `/api/board/game/{id}/…` and `/api/bot/game/{id}/…`: `move/{uci}` (`offeringDraw`), `resign`, `abort`, `draw/{yes\|no}`, `takeback/{yes\|no}`, `claim-victory`, `claim-draw`, `chat` (GET and POST) |
| Games | `GET /game/export/{id}` and `/api/games/export/{id}` (PGN, or JSON with `Accept: application/json`; `pgnInJson`, `clocks`, `moves`) |
| Fair play (ours) | `GET /api/fairplay/queue`, `/api/fairplay/games/{id}`, `/api/fairplay/players/{username}`, `POST /api/fairplay/reviews` (scope `chess:fairplay`) |
| Training app (ours) | see [below](#training-app-endpoints) |
| Server | `GET /` (what this is: `name`, `site`, `url`, `version`, `app`; browsers are sent to the app), `/app/` (the Chess lab app), `/healthz`, `/metrics` (Prometheus, on the metrics port) |

**Conformance.** `crates/chess-conformance` drives a running server through a
full game, challenges and streams, and validates every response against the
schemas in Lichess's OpenAPI spec:

```sh
git clone --depth 1 https://github.com/lichess-org/api /tmp/lichess-api
cargo run -p open-chess-conformance -- --base http://127.0.0.1:8090 \
    --spec /tmp/lichess-api/doc/specs --db "$DATABASE_URL" --site-schema <your site schema>
```

An unmodified [lichess-bot](https://github.com/lichess-bot-devs/lichess-bot)
connects, accepts challenges, chats and plays.

**Not implemented** (answered with Lichess's errors): variants other than
standard and from-position, correspondence games, seeks and the lobby,
tournaments, `POST /api/challenge/ai`, studies, puzzles, and the streams of
several games at once (`/api/stream/games-by-users`, `/api/stream/games/{id}`)
and TV. Rated games from a position, and rated unlimited games, are refused
as on Lichess.

**Differences worth knowing**
* `acceptByToken` (start a game without either player clicking: the
  opponent's token, with a playing scope) isn't in Lichess's current spec.
* Clocks start after both sides' first move. A side that doesn't make its
  first move within `CHESS_FIRST_MOVE_SECS` gets the game aborted.
  Threefold repetition and the fifty-move rule draw automatically.
* The public move stream runs behind live games (below).

## Live games

Three streams follow games as they happen. Every server replica hears each
move at once (Postgres notifications), so streams scale with the API.

**Spectators: `GET /api/stream/game/{id}`.** No token needed. The first line
describes the game; then one line per position from the start; then, when
the game ends, the description again with its result:

```json
{"id":"q7ZvsdUF","variant":{"key":"standard","name":"Standard","short":"Std"},"speed":"blitz","perf":"blitz","rated":true,"initialFen":"rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1","fen":"…","player":"white","turns":0,"source":"friend","status":{"id":20,"name":"started"},"createdAt":1760280000000,"players":{"white":{"user":{"name":"Anna","id":"anna"},"rating":1712},"black":{"user":{"name":"Bert","id":"bert"},"rating":1650}},"clock":{"initial":180,"increment":2,"totalTime":260}}
{"fen":"rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1","wc":180,"bc":180}
{"fen":"rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1","lm":"e2e4","wc":180,"bc":180}
```

`lm` is the move (UCI), `wc`/`bc` both clocks in seconds. While a game is
live the stream holds back the last `CHESS_PUBLIC_DELAY_PLIES` plies (3 by
default), as Lichess does, so it can't feed an engine helping a player; set
it to `0` for a real-time feed (for a broadcast where that doesn't matter).

**Players: `GET /api/board/game/stream/{id}`** (people, `board:play`) and
**`/api/bot/game/stream/{id}`** (bots, `bot:play`): real time, for the two
players only. `gameFull` first, then a `gameState` (moves, clocks, draw and
takeback offers, status) after every change, and `chatLine`, `opponentGone`.

**A player's events: `GET /api/stream/event`**: `challenge`,
`challengeCanceled`, `challengeDeclined`, `gameStart`, `gameFinish`. A client
opens this first and a game stream for each `gameStart`.

## Accounts, tokens and OAuth

A chess account is a player of the host site (see the
[server README](../crates/chess-server/README.md#plugging-into-a-site)),
created on first use with a Lichess-style username made from the player's
name. Bearer tokens are the site's API tokens, with Lichess's scope names:

| Scope | For |
|---|---|
| `board:play` | playing through the Board API (people) |
| `challenge:read`, `challenge:write` | seeing and sending challenges (as on Lichess, `board:play` and `bot:play` may also send and answer challenges, and see the event stream) |
| `email:read` | apps reading the player's email |
| `bot:play` | bot accounts |
| `chess:fairplay` | the fair-play review API (reviewers) |

A revoked token stops working within 15 seconds (tokens are cached that long).

**OAuth** works as on Lichess: no client registration, PKCE S256, one-year
tokens. The consent page recognises whoever is signed in to the site through
its session cookie: on a host of its own (`https://chess.example.com`), set
the site's session cookie domain to the shared parent (`example.com`); under
the site's origin (`CHESS_PUBLIC_URL=https://<site>/chess`) nothing extra is
needed. Tokens it issues are site tokens named `OAuth: <client_id>`.

**Bots.** `open-chess bot create --name stockfish-1800` makes a bot: a site
player, a bot account and a token (printed once). Bots play only through the
Bot API and people only through the Board API, as on Lichess. See
[lichess.md](lichess.md#bots) to run one.

## Training-app endpoints

open-chess's own additions, used by the Chess lab app (`/app/`). The
`/api/esl/…` paths keep the name they had in the application this server
came from, so existing app installs keep working.

| Endpoint | |
|---|---|
| `POST /api/board/game/{id}/assist` | The app's engine help went on (`on=true`) or off during your game. Ground truth for training fair-play screening; screening never relies on it. |
| `GET /api/esl/position?fen=` | The position dictionary: the deepest stored evaluation of a position (best move, score, depth, how many screened games and confirmed photos reached it, win/draw/loss odds), `queued` when a confirmed photo's position is waiting for the worker, and `hint`: the deepest analysis a training app sent in (not checked). Refused while you have a game in progress here. |
| `POST /api/esl/position` `{fen, best?, scoreCp, depth, engine?, moves?: [{uci, scoreCp}]}` | Send in the app's own analysis (scores from the side to move's view). Kept per player and position, the deepest wins; shown as a hint, never mixed with the worker's evaluations. Refused while you're playing here. |
| `GET /api/esl/position/moves?fen=` | Every legal move with the best value known for it (`scoreCp` from the mover's view, `depth`, White's `wdl` after it, `source`): the worker's scoring of this position's moves (`engine`), else the stored evaluation of the position it leads to (`after`), else an app's hint (`hint`); `null` when unknown. |
| `GET /api/esl/tablebase?fen=` | Exact endgame results (up to seven pieces) from a Lichess-compatible tablebase server (`CHESS_TABLEBASE_URL`), cached in the database. |
| `POST /api/esl/recognise` | `{"image": "data:image/jpeg;base64,…"}` → the board read from a photo: `placement` (FEN piece placement, White's side), `unsureSquares`, `confidence`, and from the board reader (`source: "model"`) each square's class probabilities in `squares`. A photo the reader isn't sure of is also shown to the vision model (`CHESS_VISION_ENDPOINT`), whose reading comes back in `llm` (or as the answer, `source: "llm"`), and is kept for review (`photoId`). Off unless `CHESS_VISION_LOCAL_MODEL` or `CHESS_VISION_ENDPOINT` is set. |
| `POST /api/esl/photos/{id}/confirm` | `{"placement": "…"}` from the player who sent photo `id`: the position it showed. Kept as a player-confirmed label; a reviewer's label always wins. |

## Fair-play review

`chess:fairplay` scope. `GET /api/fairplay/queue` lists flagged games waiting
for a person; `GET /api/fairplay/games/{id}` and `/players/{username}` give
the screening reports; `POST /api/fairplay/reviews`
`{username, gameId?, verdict: "engine"|"clean"|"unclear", note?}` records a reviewer's
decision. An `engine` verdict marks the account (`tosViolation` in
`/api/user`; with `CHESS_BLOCK_MARKED` it can't start new games); `clean`
lifts it. How screening works, and keeping its model private, is in the
[server README](../crates/chess-server/README.md#anti-cheat).
