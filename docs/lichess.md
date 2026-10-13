# Using open-chess with Lichess tools

The server speaks the Lichess API, so tools written for Lichess work with it
by pointing them at your server instead of `https://lichess.org`. The other
way round, open-chess's own pieces (the Chess lab app, `chess-client`, the
board tools) work with lichess.org. The full endpoint list is in
[api.md](api.md).

## Accounts and tokens

Every request carries a token: `Authorization: Bearer <token>`.

* **Personal tokens** come from the host site's account page, with Lichess's
  scope names (`board:play`, `challenge:read`, `challenge:write`, …).
* **Apps** use OAuth with PKCE exactly as on Lichess: no client registration,
  any `client_id`, `GET /oauth?response_type=code&client_id=…&redirect_uri=…&code_challenge_method=S256&code_challenge=…&scope=…`,
  then `POST /api/token`. The consent page shows whoever is signed in to the
  site.

Check a token with `curl -H "Authorization: Bearer $TOKEN" https://chess.example.com/api/account`.

## Playing through the Board API

Any Lichess Board API client plays here. By hand, with `curl`:

```sh
API=https://chess.example.com
# Watch for challenges and game starts (keep this running)
curl -N -H "Authorization: Bearer $TOKEN" $API/api/stream/event
# Challenge someone to 3+2
curl -X POST -H "Authorization: Bearer $TOKEN" $API/api/challenge/bert \
     -d clock.limit=180 -d clock.increment=2 -d color=white
# Follow your game, and move
curl -N -H "Authorization: Bearer $TOKEN" $API/api/board/game/stream/$GAME
curl -X POST -H "Authorization: Bearer $TOKEN" $API/api/board/game/$GAME/move/e2e4
```

In Rust, `chess-client` does the same for either server:

```rust
use open_chess_client::Client;
let lichess = Client::new("https://lichess.org", Some(&token))?;
let ours = Client::new("https://chess.example.com", Some(&token))?;
```

## Bots

Make a bot account on the server (`open-chess bot create --name
stockfish-1800`, which prints its token once), then run
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot) unchanged with
two lines of its `config.yml` changed:

```yaml
token: "<the bot's token>"
url: "https://chess.example.com/"
```

It connects, accepts challenges by its own rules, chats and plays. A bot of
your own can use `chess-client` and `chess-engine` (any UCI engine). As on
Lichess, bots play only through the Bot API (`/api/bot/…`) and people only
through the Board API.

## Watching games

`GET /api/stream/game/{id}` is a public feed of a game, as on Lichess:
no token, the game's description, then a line per move with the FEN, the move
and both clocks. It runs a few plies behind live games
(`CHESS_PUBLIC_DELAY_PLIES`, 3 by default; `0` for broadcasts). See
[api.md](api.md#live-games). Finished games export as PGN or JSON from
`/game/export/{id}`.

## The Chess lab app and lichess.org

The app (`/app/` on the server) plays online on either: **Online › Lichess**
signs in to lichess.org with OAuth and plays through its Board API; **Online ›
<your site>** does the same against your server (signing in, or making an
account, on its own pages; or takes an API token: see
[api.md](api.md#accounts-tokens-and-oauth)). Its training help (engine
lines, move explanations) is switched off during online games on Lichess,
where using it would be cheating; on your own server it can be used and is
declared (`/api/board/game/{id}/assist`), and fair-play screening takes
account of it.

## Lichess's DGT board connector

[lichess.org/dgt](https://lichess.org/dgt) plays your Lichess games on a DGT
board. It is a browser page that talks to DGT's LiveChess program on your
computer (a websocket at `ws://localhost:1982/api/v1.0`) and to Lichess's
Board API. open-chess fits in two ways, both working today (details in
[boards.md](boards.md)):

* **A board on open-chess instead of LiveChess:**
  `open-chess-device --board … --livechess` answers on that same local
  websocket, so the Lichess page works with every board open-chess drives,
  including ones LiveChess doesn't (a DGT Pi, a Centaur, Chessnut boards).
  Browsers only allow that plain websocket to `localhost`, so run it on the
  computer with the browser (or give `--livechess` another address).
* **A board straight to a server:** `open-chess-device --online` plays your
  games through the Board API itself, on lichess.org or on your server, with
  no browser at all.

## Differences from Lichess

Standard chess and games from a position only; no correspondence, lobby,
tournaments, studies or puzzles; clocks start after both first moves, and a
side that doesn't make its first move in time gets the game aborted. The full
list is in [api.md](api.md#endpoints).
