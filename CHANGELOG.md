# What's new

Each release's changes, newest first. The Chess lab shows this page as
What's new (Guides › What's new), and each GitHub release starts with its
section. Pull requests add their changes under Unreleased; cutting a release
renames that heading to the version and date.

## Unreleased

## 0.1.7 — 2026-10-16

- **The engine is a program of its own**, `open-chess-uci` (in every
  release): play it in any chess GUI or match runner, set every search
  tunable as a UCI option, see each depth's line as it searches, and
  run `bench` for the build's node signature. `open-chess-uci datagen`
  writes self-play training data in the bullet trainer's binary format,
  rejecting lopsided random openings, with a sidecar recording what
  produced it. `scripts/engine/` is the test rig: SPRT runs, the
  gauntlet and the bench check through fastchess; `training/` has the
  bullet configuration and the loop (ADR-0002).
- The engine no longer loses on time in the first endgame of a game: the
  three-piece tablebases are built once at start-up instead of inside the
  first search that needs them. `open-chess-uci` also has a `Move
  Overhead` option and plays new moves on rather than replaying the game.
- **The board reader ships with each release**: the Chess lab carries the
  model, and the server reads photos with the same one.
- **Sign in with Apple on the website** says why when Apple didn't sign you
  in (cancelled, or Apple's error) instead of returning to the sign-in page
  as if the button had done nothing.
- The host site uses the server (its players, their tokens, linking) with
  `ESPORTS_SECRET` alone; `ESPORTS_LOGIN` now only puts "Sign in with
  <site>" on the sign-in page, so it can be off while the site still works.
- The Chess lab has the esports-lab look unless `CHESS_APP_THEME` says
  otherwise (`none` for the app's own), so a deployment can't lose it by
  leaving the setting out.
- **Deploys to Cloudflare** set the server's settings from GitHub (a
  repository variable or secret named like one of them, such as
  `DATABASE_URL`, `CHESS_APPLE_KEY` or `CHESS_APP_THEME`), hand the
  containers the Sign in with Apple and push settings (and others it had
  missed: admin emails, the tablebase, photo review, analysis threads), and
  start fresh containers, so changed settings take effect at once.
- The iOS app's analysis of a finished game says "Checkmate · Black wins"
  (or stalemate) instead of an error, and tapping its ratings says what
  they and their "?" mean.
- rustls 0.23.46 (RUSTSEC-2026-0346 to 0349). zip 8.6.0, yanked upstream,
  is allowed until candle takes zip 9.

## 0.1.6 — 2026-10-15

- **The iOS app's server side.** Games played offline in the app upload and
  are replayed and checked (rated against the computer at its level's fixed
  rating unless help was used; two people on one device both confirm), the
  app syncs ratings, games and challenges in one request, correspondence
  games have days per move instead of a clock, and push notifications tell
  you it's your move. Push is off unless the server has Apple's keys.
- The live game feed orders games by when the server recorded them, so a
  game played offline and uploaded later still reaches its readers.
- The iOS app signs in with open-chess accounts only.
- **The iOS app**, ready for TestFlight: play the computer or a friend
  offline, online games, analysis (never rated), hints and advice from a
  photo, check and checkmate called out (with sound, haptics and speech),
  moves in plain English, and the esports look by default.
- **Real-board games recorded live** from the app (a Chessnut, or any
  board with photos): the illegal moves shown on the board to put right,
  rated like games on one device once both players confirm, and a stream
  to Twitch or YouTube with the board and last five moves overlaid
  (`/broadcast/{id}`). Photos and video frames labelled by a connected
  board help train the board reader.
- **Sign in with Apple**, in the app and on the sign-in page: a new
  account, or linked to yours under Account. Apple is told when you unlink
  it or close your account, and the server hears from Apple when someone
  stops using their Apple ID here or deletes their Apple Account. Needs the
  server's Sign in with Apple key.
- Signing in to Lichess in the app is single sign-on, not a pasted token.
- The API guide points to the live feed for finding new games, and says
  the spectator stream needs a token.
- **Credits** in the Chess lab (Settings): the datasets, models and tools
  the board reader and the app build on, and the card of the board reader
  it ships with. felix-boards and Roboflow's Chess Pieces join the training
  datasets, their licences checked at the source.

## 0.1.5 — 2026-10-15

- **Accounts of its own.** Players have roles (admin, manager), passkeys,
  public profiles and an admin page; partner sites get tokens of their own.
  Reading players needs signing in.
- **The iOS app's core** as a Swift package: playing, the bot and its fixed
  ratings, Chessnut boards over Bluetooth, the board reader, and a store for
  saved games and offline play.
- **Bundled themes.** `CHESS_APP_THEME` can name a theme the app ships with
  (`esports-lab`) instead of holding CSS.
- **Deploys to Cloudflare** from this repository after each release.
- "Sign in with …" shows the host site's own name (`CHESS_HOST_NAME`).
- The Chessnut driver is MIT or Apache-2.0.
- A 48-hour cooldown on new crates, checked in CI.

## 0.1.4 — 2026-10-14

- **Photo review in the Chess lab** for anyone signed in, under the photo
  licence (Guides › The photo licence).
- The board reader learns from a bigger model and from unlabelled photos.
- **Chessnut on USB:** opened shared on macOS rather than taken over; every
  interface; commands framed as the board expects.
- A site can link its player to an existing open-chess account.
- Openly licensed board datasets listed in `datasets.toml`; ChessReD used
  commercially by its author's permission, with a Credits page.

## 0.1.3 — 2026-10-14

- **One program, `open-chess`,** for the server, the board runtime and the
  photo reader.
- `open-chess photo`: read a board from a photo, carry a game online, play
  on from photos; `open-chess train board-reader` to train the reader.
- A logo and favicon; the guides in the Chess lab; a deployment's theme
  always wins over the app's own.

## 0.1.2 — 2026-10-13

- **open-chess as a provider:** accounts of its own, "Sign in with" the host
  site, and a live feed of finished games.
- The Chess lab's look can be themed per deployment, and it can be embedded.
- Play the computer on the server.
- The board's screen: choose the game, moves in words, the result and
  ratings.

## 0.1.0 — 2026-10-13

The first release.

- **Electronic boards:** DGT (serial, the DGT Pi and its clock, the
  Centaur and its e-paper screen), Chessnut Air and Go and the DGT Pegasus
  over Bluetooth, and Chessnut on USB, on Linux, macOS and Windows.
- Play the engine on a board, with Polyglot opening books, time controls
  and the DGT3000's buttons.
- The Chess lab app as the board's screen, or a terminal screen.
- PicoChess's web API, clock menu and `picochess.ini`; Lichess's DGT page.
- Releases: binaries, the image and the crates.
