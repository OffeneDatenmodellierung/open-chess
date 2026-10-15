# What's new

Each release's changes, newest first. The Chess lab shows this page as
What's new (Guides › What's new), and each GitHub release starts with its
section. Pull requests add their changes under Unreleased; cutting a release
renames that heading to the version and date.

## Unreleased

- **The iOS app's server side.** Games played offline in the app upload and
  are replayed and checked (rated against the computer at its level's fixed
  rating unless help was used; two people on one device both confirm), the
  app syncs ratings, games and challenges in one request, correspondence
  games have days per move instead of a clock, and push notifications tell
  you it's your move. Push is off unless the server has Apple's keys.
- The live game feed orders games by when the server recorded them, so a
  game played offline and uploaded later still reaches its readers.
- The iOS app signs in with open-chess accounts only.

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
