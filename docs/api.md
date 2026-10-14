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
`fetch()` and a line reader in a browser. As on Lichess, any origin may call
the API from a browser (`Access-Control-Allow-Origin: *`; preflights allow
`GET, POST, DELETE` with `Authorization` and `Content-Type`): tokens, not
cookies, carry who you are.

```sh
curl -H "Authorization: Bearer $TOKEN" https://chess.example.com/api/account
curl -N -H "Authorization: Bearer $TOKEN" https://chess.example.com/api/stream/event
curl -X POST -H "Authorization: Bearer $TOKEN" -d clock.limit=300 -d clock.increment=3 \
  https://chess.example.com/api/challenge/anna
```

## Endpoints

| Area | Endpoints |
|---|---|
| OAuth | `GET/POST /oauth` (consent page), `POST /api/token`, `DELETE /api/token`, `POST /api/token/test` |
| Account | `GET /api/account` (Lichess's, plus `roles`, `staff` and `passkeys`, below), `/api/account/email`, `/api/account/playing` (`nb`), `/api/user/{username}`, `/api/player/autocomplete` (`term`, `object`, `nb`: players whose name starts with, then contains, `term`, for a type-ahead list), `POST /api/account/passkey/options` and `POST /api/account/passkey` (the staff passkey check, below) |
| Bots | `POST /api/bot/account/upgrade`, `GET /api/bot/online` (`nb`) |
| Streams | `GET /api/stream/event`, `/api/board/game/stream/{id}`, `/api/bot/game/stream/{id}`, `/api/stream/game/{id}` (see [Live games](#live-games)) |
| Challenges | `POST /api/challenge/ai` (the computer, below), `POST /api/challenge/{username}` (with `keepAliveStream`, `rules` and `acceptByToken`; a clock, or `days` per move for correspondence), `GET /api/challenge`, `GET /api/challenge/{id}/show`, `POST …/{id}/accept`, `…/decline` (`reason`), `…/cancel` |
| Playing | `/api/board/game/{id}/…` and `/api/bot/game/{id}/…`: `move/{uci}` (`offeringDraw`), `resign`, `abort`, `draw/{yes\|no}`, `takeback/{yes\|no}`, `claim-victory`, `claim-draw`, `chat` (GET, and POST with `room` and `text`, up to 140 characters) |
| Games | `GET /game/export/{id}` and `/api/games/export/{id}` (PGN, or JSON with `Accept: application/json`; `pgnInJson`, `clocks`, `moves`), `GET /api/games/user/{username}` (their games, newest first, NDJSON; `max`, default 20, up to 100; `ongoing=false`) |
| Fair play (ours) | `GET /api/fairplay/queue` (`limit`, default 50, up to 500; `all=true` for reviewed games too), `/api/fairplay/games/{id}`, `/api/fairplay/players/{username}`, `POST /api/fairplay/reviews` (managers and admins, below) |
| Admin (ours) | `GET /api/admin/players` (`q`, `only=staff`), `GET/POST /api/admin/players/{username}`, `POST …/tokens/revoke`, `POST …/passkeys/reset`, `GET/POST /api/admin/partners`, `POST /api/admin/partners/{id}/revoke` (see [Admins](#admins-managers-and-partners)) |
| Training app (ours) | see [below](#training-app-endpoints) |
| The iOS app (ours) | `POST /api/games/offline`, `POST /api/games/{id}/confirm`, `GET /api/sync`, `POST /api/photos`, `POST /api/push/devices`, `DELETE /api/push/devices/{token}`, `DELETE /api/account` (see [below](#the-ios-app)) |
| Server | `GET /` (what this is, below; browsers are sent to the app), `GET /app/` (the Chess lab app; `/app` redirects there), `GET /app/theme.css` (the deployment's look, below), `GET /app/docs/{api,boards,photos,photo-licence,lichess,engine,releases}.md` (these guides, as markdown, for sites that show them), `GET /docs` and `/docs/{guide}` (these guides, read in the app: `/app/#docs/{guide}`, or on GitHub when the app isn't installed), `GET /favicon.svg` (`/favicon.ico` too: the logo), `GET /@/{username}` (a player's profile, in the app: `/app/#@/{username}`) |
| Operations (the metrics port, `CHESS_METRICS_BIND`) | `GET /metrics` (Prometheus), `/healthz` (the process is up), `/readyz` (it can reach Postgres) |

**`GET /`** answers what the server is, for the app and for tools:

```json
{"name": "open-chess", "site": "open-chess", "api": "Lichess-compatible: …",
 "url": "https://chess.example.com", "version": "0.1.0",
 "login": true, "signup": true, "esports": false, "app": "https://chess.example.com/app/"}
```

`login` is always true (the server has its own sign-in); `signup` is
whether anyone may make an account (`CHESS_SIGNUP`); `esports` whether a
host site's players sign in with it (`ESPORTS_LOGIN`, below); `app` is
`null` without the app.

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

**Not implemented:** variants other than standard and from-position,
seeks and the lobby, tournaments, studies, puzzles, the streams of several games at once
(`/api/stream/games-by-users`, `/api/stream/games/{id}`) and TV. Paths that
don't exist get an empty 404; unsupported options on paths that do (a
variant) get Lichess's JSON errors. Rated games from
a position, and rated unlimited games, are refused as on Lichess.

**Signed in only.** Nothing is read anonymously: game export, the move
streams, `/api/user`, autocomplete, online bots and [the feed](#the-live-feed)
all need a token, a player's or [a partner's](#admins-managers-and-partners).
Only `GET /`, the app, the guides and the sign-in pages are open.

**Differences worth knowing**
* `acceptByToken` (start a game without either player clicking: the
  opponent's token, with a playing scope) isn't in Lichess's current spec.
* Correspondence: `days` (1, 2, 3, 5, 7, 10 or 14) per move instead of a
  clock, rated or not. A side out of days loses on time (a draw if the
  other couldn't mate); a first move that doesn't come in time aborts the
  game. `daysPerTurn` is in the game's stream, and `secondsLeft` in
  `nowPlaying` counts down the days.
* Clocks start after both sides' first move. A side that doesn't make its
  first move within `CHESS_FIRST_MOVE_SECS` gets the game aborted.
  Threefold repetition and the fifty-move rule draw automatically.
* The public move stream runs behind live games (below).

## The iOS app

What the iPhone and iPad app needs beyond the Lichess API
([ios.md](ios.md)). All take a token; JSON bodies.

| Endpoint | |
|---|---|
| `POST /api/games/offline` | `{"games": [record, …]}` (up to 50): games played in the app, as its core records them (`clientId`, `mode` `computer` or `passAndPlay`, `botLevel`, `white`, `black`, `speed`, `moves` in UCI, `moveTimesMs`, `startedMs`, `status`, `winner`, `assistance`). Each is replayed: the board decides mate and stalemate, the record says resignations, agreed draws and time. Against the computer (`ai-level-N` as one side) the game is recorded at once, and rated at the level's fixed rating unless help was used, it started from a set position, or the move times don't add up. Between two people on one device it waits for the other to confirm. The answer, per game: `{clientId, gameId, status: rated\|unrated\|pending\|duplicate\|rejected, reason, ratingDiff}`. A `clientId` already sent is a `duplicate`, so retrying is safe |
| `POST /api/games/{id}/confirm` | `{"accept": true\|false}`, by the other player of a `pending` game: recorded (and rated, if it can be) or dropped |
| `GET /api/sync` | `since` (Unix ms): `{now, username, ratings: {perf: {rating, deviation, volatility, provisional, games}}, playing: [{gameId, color, fen, lastMove, isMyTurn, speed, rated, daysPerTurn, secondsLeft, opponent}], confirmations: […], challenges: […], finished: [games finished since]}`, gzipped when asked. One small answer for slow, metered links |
| `POST /api/photos` | `{clientId, image (base64 JPEG, up to 4 MB), placement, confidence, model}`: a board photo with the position the player confirmed, kept as training data under the [photo licence](photo-licence.md) (accept it first: `POST /api/esl/photo-terms`). `{id}`; the same `clientId` again gives the same id |
| `POST /api/push/devices`, `DELETE /api/push/devices/{token}` | `{token: the APNs device token in hex, sandbox}`: where to send "your move" in correspondence games and results to confirm |
| `DELETE /api/account` | Close the account: games in progress are resigned, finished games stay under a placeholder name, tokens and sessions are revoked. 204 |

Push notifications go out when `CHESS_APNS_KEY` (the `.p8` key, or its
path), `CHESS_APNS_KEY_ID` and `CHESS_APNS_TEAM_ID` are set;
`CHESS_APNS_TOPIC` is the app's bundle id
(`org.offenedatenmodellierung.openchess`). The app signs in with OAuth
(PKCE) and the redirect `org.offenedatenmodellierung.openchess:/oauth`:
native apps' reversed-domain schemes are accepted ([RFC 8252](https://www.rfc-editor.org/rfc/rfc8252#section-7.1)).

## Playing the computer

`POST /api/challenge/ai` with `level` (1–8) plays open-chess's own engine
at that level, as Lichess's endpoint does: `clock.limit` and
`clock.increment` (or neither, for no clock), `color` (`white`, `black`,
`random`), `fen` and `rated` (with a clock, from the starting position,
as for any rated game). The game starts at once; the answer
is the game (`id`, `player`: your colour, `aiLevel`, `source: "ai"` …), and
it's played through the Board API like any other. The computer is a bot
account, `ai-level-N`, made on first use, with a fixed rating per level
(800, 1000 … 2200). A rated game moves your rating against that, never
the computer's; Glicko gives little for beating a level far below you, so
it can't be farmed. The iOS app rates offline games against the same
levels the same way. Every server replica plays the
computer's moves, taking each game with a Postgres advisory lock.

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" -d level=4 -d clock.limit=600 -d clock.increment=5 \
  https://chess.example.com/api/challenge/ai
```

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

The server has its own accounts, and needs no other site's. People make one
at `/signup` (a username, optionally an email, and a password of 10 or more
characters or a passkey; off with `CHESS_SIGNUP=false`), sign in at `/login`
(username or email and password, or a passkey), and manage passkeys, their
password and API tokens on `/account`. With `ESPORTS_LOGIN`, a host site's
players can also sign in with the site; their account here is linked to
theirs there, and neither site needs an account on the other (see the
[server README](../crates/chess-server/README.md#plugging-into-a-site)).

**Passkeys** (WebAuthn: a fingerprint, face or device PIN) work instead of a
password: an account can be made with one and have no password at all, and
the Account page adds more (or removes them, keeping one when there's no
password). The server is the relying party for its `CHESS_PUBLIC_URL`'s
host, so passkeys work on that origin (the Chess lab served at `/app/`),
not inside another site's frame.
Bearer tokens start `ocp_` (tokens imported from a host site keep its
`esl_`), with Lichess's scope names:

| Scope | For |
|---|---|
| `board:play` | playing through the Board API (people) |
| `challenge:read`, `challenge:write` | seeing and sending challenges (as on Lichess, `board:play` and `bot:play` may also send and answer challenges, and see the event stream) |
| `email:read` | apps reading the player's email |
| `bot:play` | bot accounts |
| `chess:fairplay` | the fair-play review API (reviewers) |

A revoked token stops working within 15 seconds (tokens are cached that long).

## Admins, managers and partners

Roles here are this server's own: a host site's roles don't carry over.

* **Admins** run the server: accounts and their roles, tokens, partners.
  `CHESS_ADMIN_EMAILS` (comma-separated) makes an account an admin when it
  signs in with a listed email that's confirmed (a host site's sign-in
  vouches for its emails, so this trusts that sign-in; one typed into
  `/signup` isn't confirmed), and `open-chess admin grant <username>`
  (`--manager` for a manager; `revoke`, `list`) does it from the command
  line. Admins make others admins or managers in the Chess lab's admin page.
  A role only goes to an account that has a passkey.
* **Managers** review fair play and settle board photos.

Their rights need a **passkey check** as well: until then `/api/account`
says `"staff": "pending"` and staff endpoints answer 403. `POST
/api/account/passkey/options` then `POST /api/account/passkey` (the
`navigator.credentials.get` answer) checks the token in hand (the Chess lab
does this); signing in with a passkey, or confirming on the Account page,
checks the session. A token made through OAuth never inherits a check: an
app approved for `board:play` doesn't get an admin's rights.

On an account with passkeys, adding or removing one, or setting or dropping
the password, needs that check in the session too, so a stolen password or
session can't add the thief's passkey. Without passkeys, changing the
password takes the current one. A check lasts
`CHESS_ADMIN_PASSKEY_HOURS` (12). `/api/account` adds `"roles": ["admin",
"manager"]`, `"staff": "checked" | "pending"` and `"passkeys": <count>`.

**Partners** are other services' API access: an admin makes one in the admin
page, or with `open-chess partner create --name esports-lab --scopes
read,site`, and gives its token (`ocs_…`, shown once) to the service, which
sends it as a bearer token. Its scopes:

| Scope | For |
|---|---|
| `read` | games, players and the live feed (everything a signed-in player reads) |
| `site` | acting as the host site: `/api/esports/token`, `…/revoke`, `…/players` and `…/link` |

Revoking a partner stops its token at once. A host site's shared secret
(`ESPORTS_SECRET`) still works as a partner with both scopes, so it can move
to a partner token at its own pace; the secret also signs its players'
sign-ins.

**OAuth** works as on Lichess: no client registration, PKCE S256, one-year
tokens. The consent page (`GET /oauth`) acts for whoever is signed in to
this server in that browser, and sends anyone else to sign in (or make an
account) first, then back. Tokens it issues are named `OAuth: <client_id>`,
listed and revocable on `/account`.

The flow, for an app at `https://app.example/`:

1. Send the browser to `GET /oauth?response_type=code&client_id=my-app&redirect_uri=https://app.example/&code_challenge_method=S256&code_challenge=<S256 of a random verifier>&scope=board:play&state=<random>`.
2. The player approves; the browser comes back to `redirect_uri?code=…&state=…`.
3. `POST /api/token` (form: `grant_type=authorization_code`, `code`, `code_verifier`, `redirect_uri`, `client_id`) answers `{"token_type":"Bearer","access_token":"…","expires_in":…}`.
4. `DELETE /api/token` with the token revokes it; `POST /api/token/test` (a comma-separated list of tokens as the body) says which are valid.

**Bots.** `open-chess bot create --name stockfish-1800` makes a bot: a site
player, a bot account and a token (printed once). Bots play only through the
Bot API and people only through the Board API, as on Lichess. See
[lichess.md](lichess.md#bots) to run one.

## The live feed

`GET /api/feed` sends every game on the server as
[server-sent events](https://html.spec.whatwg.org/multipage/server-sent-events.html),
to a player's token or a partner's with `read` (it shows what the public
game streams show):

| Event | Data |
|---|---|
| `start` | A game began: the public game description (as the first line of `/api/stream/game/{id}`) |
| `move` | It moved on: the same description (position, last move, `turns`). While a game is live it runs `CHESS_PUBLIC_DELAY_PLIES` behind, as the public stream does |
| `end` | It finished: the game as `/game/export/{id}` gives it in JSON (players, ratings and `ratingDiff`, status, winner, moves, clock). Its event id is when the server recorded it finished (Unix ms): the finish for games played here, the upload for games played offline in the iOS app |

A client that reconnects with `Last-Event-ID` (a browser's `EventSource`
does it itself), or asks with `?since=<Unix ms>`, first gets the `end` of
every game finished since then, oldest first, then the live events; a
consumer that keeps the last id it handled misses nothing. `?game=<id>`
follows one game; `?only=end` sends only finished games. A comment every
15 seconds keeps proxies from closing the stream.

```sh
curl -N -H "Authorization: Bearer $PARTNER_TOKEN" "https://chess.example.com/api/feed?only=end"
```

## The Chess lab's look

A deployment can give the app (`/app/`) its own colours and fonts:
`CHESS_APP_THEME` holds CSS (or the path of a `.css` file), served as
`/app/theme.css` after the app's styles. The app's colours are CSS
variables, for light (`:root`) and dark (`:root[data-theme="dark"]`, or the
system's dark mode when nothing chose):

```css
:root, :root[data-theme="light"] { --paper: #eef2fb; --panel: #ffffff; --ink: #06114a; --muted: #4d5f8b;
  --rule: #c9d4ee; --sq-light: #e3eafa; --sq-dark: #7b8bb0; --action: #d94a00; --action-ink: #ffffff; }
:root[data-theme="dark"] { --paper: #06114a; --panel: #0d1e6a; --ink: #ffffff; /* … */ }
body { font-family: Inter, system-ui, sans-serif; }
```

The others are `--amber`, `--danger`, `--w-seg`, `--b-seg`, `--d-seg`
(the win/draw/loss bar), `--arrow-best` and `--arrow-reply`.

**Embedding.** A page can show the app in an `<iframe>`:
`/app/?embed=1&theme=dark` drops the app's own title and picks the theme
(`light` or `dark`), and

```js
frame.contentWindow.postMessage({ type: "open-chess:theme", theme: "light" }, "*");
```

changes it when the page's own theme changes. Both stay set for the tab,
through signing in.

## Training-app endpoints

open-chess's own additions, used by the Chess lab app (`/app/`). The
`/api/esl/…` paths keep the name they had in the application this server
came from, so existing app installs keep working.

| Endpoint | |
|---|---|
| `POST /api/board/game/{id}/assist` | The app's engine help went on (`on=true`) or off during your game. Ground truth for training fair-play screening; screening never relies on it. |
| `GET /api/esl/position?fen=` | The position dictionary: `key` (the position's key), `known`, the deepest stored evaluation (`best`, `scoreCp`, `depth`, `engine`, win/draw/loss odds), `seen` (screened games that reached it), `photos` (confirmed photos of it), `queued` when a confirmed photo's position is waiting for the worker, and `hint`: the deepest analysis a training app sent in (not checked). Refused (403) while you have a game in progress here. |
| `POST /api/esl/position` `{fen, best?, scoreCp, depth, engine?, moves?: [{uci, scoreCp}]}` | Send in the app's own analysis (scores from the side to move's view). Kept per player and position, the deepest wins; shown as a hint, never mixed with the worker's evaluations. Refused while you're playing here. |
| `GET /api/esl/position/moves?fen=` | `{key, moves}`: every legal move (`uci`, `san`) with the best value known for it (`scoreCp` from the mover's view, `depth`, White's `wdl` after it, `source`): the worker's scoring of this position's moves (`engine`), else the stored evaluation of the position it leads to (`after`), else an app's hint (`hint`); `null` when unknown. Refused while you're playing here. |
| `GET /api/esl/tablebase?fen=` | Exact endgame results (up to seven pieces) from a Lichess-compatible tablebase server (`CHESS_TABLEBASE_URL`), in that server's JSON, cached in the database (`"cached": true` when it came from there). Refused while you're playing here; 400 above seven pieces; 503 when `CHESS_TABLEBASE_URL` is empty. |
| `POST /api/esl/recognise` | `{"image": "data:image/jpeg;base64,…"}` (up to 8 MB) → the board read from a photo: `placement` (FEN piece placement, White's side), `plausible` (a legal-looking position), `whiteAtBottom`, `sideToMove` (when it can tell), `unsureSquares`, `confidence`, `model` (which reader), and from the board reader (`source: "model"`) each square's class probabilities in `squares`. A photo the reader isn't sure of is also shown to the vision model (`CHESS_VISION_ENDPOINT`), whose reading comes back in `llm` (or as the answer, `source: "llm"`). With `CHESS_VISION_REVIEW` on, an unsure photo is kept for review (`photoId`, `review`) while fewer than `CHESS_VISION_REVIEW_MAX` are waiting, and only if its sender accepted the photo licence ([photo-licence.md](photo-licence.md)); `termsNeeded` says they haven't. Off unless `CHESS_VISION_LOCAL_MODEL` or `CHESS_VISION_ENDPOINT` is set. |
| `GET /api/esl/photo-terms` | The photo licence and my answer: `{terms, accepted, public, text}` |
| `POST /api/esl/photo-terms` | `{"accept": true, "public": false}`: accept the photo licence for photos I send from now on (`public`: also release them as CC0 for the public dataset); `accept: false` withdraws |
| `POST /api/esl/photos/{id}/confirm` | `{"placement": "…"}` from the player who sent photo `id`: the position it showed. A player-confirmed label, and one vote towards agreement; an admin's label or two agreeing people win over it. |
| `GET /api/esl/photos` | Anyone signed in: photos waiting for people to say what they show, not their own and not ones they've answered, newest first, without the bytes (`votes`, `my_vote`, `public`, `disputed`). Admins and managers (`staff: true` in the answer) see disputed photos first, and with `?all=true` every photo. |
| `GET /api/esl/photos/{id}/image` | Anyone signed in: the photo |
| `POST /api/esl/photos/{id}/label` | Anyone signed in: `{"placement": "…"}` or `{"unreadable": true}`, my answer. Two people giving the same answer (the sender's confirmation counts) label the photo (`label_source: consensus`); an admin's or manager's answer is the label at once (`admin`) and isn't outvoted. Answers `{settled, label}`; a settled position joins the dictionary. |

## Fair-play review

Managers and admins, with their passkey check. `GET /api/fairplay/queue` lists flagged games waiting
for a person; `GET /api/fairplay/games/{id}` and `/players/{username}` give
the screening reports; `POST /api/fairplay/reviews`
`{username, gameId?, verdict: "engine"|"clean"|"unclear", note?}` records a reviewer's
decision. An `engine` verdict marks the account (`tosViolation` in
`/api/user`; with `CHESS_BLOCK_MARKED` it can't start new games); `clean`
lifts it. How screening works, and keeping its model private, is in the
[server README](../crates/chess-server/README.md#anti-cheat).
