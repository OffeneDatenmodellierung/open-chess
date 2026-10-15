# open-chess for iPhone and iPad

A SwiftUI app over the open-chess Rust core (`crates/chess-mobile`, as the
`OpenChessCore` Swift package UniFFI generates). The core makes every
chess decision: the rules, the bot, hints and analysis, Glicko-2 ratings,
the Chessnut protocol, the board reader and the SQLite store. The app
handles the screens, camera, Bluetooth, network, background refresh and
notifications. See [`docs/ios.md`](../docs/ios.md) and
[ADR-0001](../docs/adr/0001-native-ios-app-on-the-open-chess-core.md).

## Build

On a Mac with Xcode 16 (iOS 17 SDK or later):

```sh
scripts/build-xcframework.sh   # writes dist/ios/OpenChessCore (from the repo root)
brew install xcodegen
cd ios && xcodegen             # writes OpenChess.xcodeproj from project.yml
open OpenChess.xcodeproj
```

Pick the `OpenChess` scheme and an iPhone or iPad simulator. For CI:

```sh
cd ios && xcodegen
xcodebuild -project OpenChess.xcodeproj -scheme OpenChess \
  -destination 'platform=iOS Simulator,name=iPhone 16' build test
```

The xcframework holds `aarch64-apple-ios` and `aarch64-apple-ios-sim`,
so simulator builds need an Apple-silicon Mac.

**The board reader:** copy a `board-reader.safetensors` (one whose model
card isn't internal-only) to `ios/Resources/` before running `xcodegen`
and it is bundled. Without it, "Scan a board" says so and opens the board
editor for setting up the position by hand.

## Signing, and what needs an Apple team

`project.yml` sets `DEVELOPMENT_TEAM: ""` and `CODE_SIGNING_ALLOWED: NO`,
so simulator builds work on CI with no certificates. For a device build,
set your team in `project.yml` (or in Xcode), remove the two
`CODE_SIGNING_*` lines and run `xcodegen` again.

These need a real team and its provisioning profile:

- **Push** ("Tell me when it's my move"): the `aps-environment`
  entitlement in `Support/OpenChess.entitlements`. Without it,
  `registerForRemoteNotifications` fails, and the setting shows the
  reason. The token goes to `POST /api/push/devices` with
  `sandbox: true` in Debug builds and `false` in Release.
- **Device builds** of any kind, and Bluetooth (the simulator has none).

## Layout

| Folder | What's in it |
|---|---|
| `OpenChess/App` | The app entry point, `AppModel` (shared state), settings (kept in the core's store), the tab root, small utilities |
| `OpenChess/Theme` | The esports site's colour tokens (dark and light), fonts, card, button and tab-bar styles |
| `OpenChess/Board` | FEN placement, squares, the board view (tap-tap moves, last-move and hint marks), the promotion picker, the W/D/L bar |
| `OpenChess/Game` | `GameViewModel` (a game: core session, bot, clocks, hints, the Chessnut board, online play), the game screen, analysis JSON decoding |
| `OpenChess/Play` | Home (ratings, play buttons, tiles, sync pill), new-game sheets, saved games |
| `OpenChess/Train` | Training games and the position editor |
| `OpenChess/Online` | Correspondence and real-time challenges on open-chess and Lichess, games in play |
| `OpenChess/Scan` | Camera, the board reader, the board editor, saving a correction, the photo licence |
| `OpenChess/Sync` | `SyncEngine` (outbox, `/api/sync`, background refresh), the network monitor, push, the Sync and data screen |
| `OpenChess/Account` | Sign-in (OAuth 2 PKCE), the server URL, deleting the account, About and credits |
| `OpenChess/Bluetooth` | The Chessnut board over CoreBluetooth |
| `OpenChess/Net` | The HTTP client, server response types, Keychain, PKCE |
| `OpenChessTests` | Logic tests for the pure-Swift parts (no core needed) |

## How it behaves

- **Offline first.** Every game is saved in the store as it's played
  (`saveGame`). A finished computer or pass-and-play game goes into the
  outbox (`enqueue(kind: "game")`) as `offlineRecordJson`. A game against
  the computer with no help also updates the speed's provisional rating on
  the device (`ratingAfter` against `botRating(level)`, then
  `setLocalRating`). The server's figure replaces it at the next sync.
- **Help makes a game unrated.** Pressing Hint shows the best move, the
  top five lines with scores, the W/D/L odds and the opening, and adds the
  "Hints on · unrated" badge. Training games have hints on from the
  start and are never uploaded.
- **Pass and play** asks for the other player's username. With one (and
  a signed-in player) the record is uploaded, and the server asks the other
  player to confirm. Without one the game stays on the device. On iPhone
  the board turns after each move (a setting).
- **Sync.** Syncs run on the chosen interval while the app is open, on
  "Sync now" or by tapping the pill, and when iOS gives the app background
  time (`BGAppRefreshTask` `org.offenedatenmodellierung.openchess.sync`).
  Each one sends games, then confirmations, then photos (if allowed),
  marks the accepted keys sent, then applies `GET /api/sync`. In low-data
  mode a sync sends games and confirmations only, up to 50 KB. Low data
  turns on by itself when `NWPathMonitor` reports a constrained or
  expensive path, and the player can override it.
- **Online.** Correspondence (1, 3 or 7 days a move) comes first. Real-time
  games are offered only when low data is off. Games use the Board API on
  open-chess or lichess.org. A live game follows the NDJSON stream. A
  correspondence game, or any game in low-data mode, reads the stream's
  first line once (on opening, and on Refresh).
- **Chessnut.** Moves made on the board play in the game on screen. The
  bot's or the opponent's reply, and hints, light up on the board.

## Stubs and known gaps

- **App Attest** isn't used yet, so the app doesn't ask for it. Adding it
  needs a server endpoint first, then the
  `com.apple.developer.devicecheck.appattest-environment` entitlement.
- **Lichess** uses a personal token pasted by the player (Keychain), not
  OAuth. Its games in play come from `GET /api/account/playing`.
  Challenges to the player on Lichess aren't listed.
- **Time-outs** in offline games: the core has no time-out result, so the
  app records the flagged side as resigning (`resign`). It shows and saves
  "Time out" locally, but the uploaded record's `status` is `resign`.
- **Online clocks** show the stream's `wtime`/`btime` and count down
  locally. Offline games' clocks are the app's own.
- **Opening an online game** starts from the stream's `gameFull`. Until
  it arrives (or with no connection), the board shows the saved moves or
  the start.
- **Fonts**: Inter and JetBrains Mono are used when installed, but they
  aren't bundled. Otherwise the app uses the system font with monospaced
  digits.
- **Photo upload** sends a centre-cropped square JPEG (768 px, quality 0.72,
  re-encoded so no EXIF survives) as `POST /api/photos`. The licence
  answer is stored locally and sent to `POST /api/esl/photo-terms` when
  signed in. The app doesn't yet read the account-level answer back with
  `GET /api/esl/photo-terms`.
