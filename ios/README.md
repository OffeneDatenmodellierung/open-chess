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

- **Sign in with Apple**: the `com.apple.developer.applesignin`
  entitlement (`Default`), and the capability on the App ID. Unsigned CI
  builds aren't affected; in the simulator it needs an Apple ID signed
  in to the simulator.
- **Push** ("Tell me when it's my move"): the `aps-environment`
  entitlement in `Support/OpenChess.entitlements`. Without it,
  `registerForRemoteNotifications` fails, and the setting shows the
  reason. The token goes to `POST /api/push/devices` with
  `sandbox: true` in Debug builds and `false` in Release.
- **Device builds** of any kind, and Bluetooth (the simulator has none).

What needs a real device (any team): the camera (live board mode's
preview, photos after each move, labelled frames, live advice), the
Chessnut board (Bluetooth), streaming (camera and microphone; the
simulator has neither) and haptics. In the simulator, "Take a photo"
falls back to choosing one from the library.

## Layout

| Folder | What's in it |
|---|---|
| `OpenChess/App` | The app entry point, `AppModel` (shared state), settings (kept in the core's store), the tab root, small utilities |
| `OpenChess/Theme` | The esports site's colour tokens (dark and light), fonts, card, button and tab-bar styles |
| `OpenChess/Board` | FEN placement, squares, the board view (tap-tap moves, last-move, check, arrows, tints, ghosts and remove badges, the illegal-tap shake), `MoveInput`, real-board corrections, the promotion picker, the W/D/L bar |
| `OpenChess/Game` | `GameViewModel` (a game: core session, bot, clocks, hints, the Chessnut board, online play), the game screen, the result card, analysis JSON decoding, the analysis board, Advice (shared panel), plain-English moves, result and speech text |
| `OpenChess/Live` | Live board mode ("Play on a real board"): setup, the screen, `LiveBoardModel`, the server queue (`OTBSender`), the camera feed and frame tools |
| `OpenChess/Streaming` | Streaming with HaishinKit (`StreamController`, the only file that imports it), the cutout, streaming settings, recordings |
| `OpenChess/Play` | Home (ratings, play buttons, tiles, sync pill), new-game sheets, saved games |
| `OpenChess/Train` | Training games and the position editor |
| `OpenChess/Online` | Correspondence and real-time challenges on open-chess and Lichess, games in play |
| `OpenChess/Scan` | Camera, the board reader, the board editor (Advice, Analyse, play from here), live advice, saving a correction, the photo licence |
| `OpenChess/Sync` | `SyncEngine` (outbox, `/api/sync`, background refresh), the network monitor, push, the Sync and data screen |
| `OpenChess/Account` | Sign-in to open-chess and Lichess (OAuth 2 PKCE), `SignInPrompt`, the server URL, deleting the account, About and credits |
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

- **Sign in where it's needed.** Every place that needs an account
  (online play, sync and uploads, confirmations, photos and labels,
  real-board games, notifications, Lichess games) shows `SignInPrompt`:
  a reason and a button that signs in right there. It goes away once
  signed in; cancelling leaves it.
- **Sign in with Apple** (native, `AuthenticationServices`): Apple's
  white button sits above "Sign in to open-chess" in `SignInPrompt` and
  on Account. The app makes a nonce (32 random bytes, base64url), gives
  Apple its SHA-256 (hex) and sends the raw nonce, the identity token and
  the authorization code to `POST /api/auth/apple` (with the OAuth
  scopes and `email:read`, and the name and email Apple gives the first
  time). A 200 brings the same kind of token as the OAuth sign-in. A new
  Apple user gets "Choose a username" (prefilled with the server's
  suggestion); the same Apple answer is sent again with the name while
  Apple's code is still fresh (about 5 minutes, else Apple's sheet runs
  again). Signed in, Account shows the Apple ID: "Link Apple ID" posts
  with the bearer token (the token is kept), and Unlink (confirmed)
  calls `DELETE /api/auth/apple`. The server's messages (taken name,
  linked elsewhere, email in use, no new accounts) are shown as sent.
- **Lichess sign-in** is OAuth 2 PKCE on lichess.org (no client
  registration: client id `open-chess-ios`, redirect
  `org.offenedatenmodellierung.openchess:/lichess`, scopes `board:play
  challenge:read challenge:write`). The token is kept in the Keychain;
  signing out revokes it (`DELETE /api/token`). Pasting a personal token
  is still possible under "Advanced" in Play online.
- **Feedback.** In check, the king's square turns red, with a light
  haptic and a short sound. A game's end shows a result card over the
  board (the result and why: mate, stalemate, repetition, fifty moves,
  insufficient material, resignation, time, an agreed draw), with
  *Analyse this game*, *Rematch* (computer, pass and play) and *Done*, a
  success or error haptic, and a VoiceOver announcement. An illegal tap
  shakes the piece. Settings: Sounds, Haptics (on), Speak moves (off;
  the opponent's or computer's move, and every move recorded on a real
  board, read aloud), Moves in plain English (off; the core's
  `describeMoves` beside SAN in every move list, the last move and the
  hint, and used when speaking), and Appearance: Dark (the default, also
  set in the Info.plist), Light or Match system.
- **Analysis board** (never scored): both sides move freely; the top five
  lines with scores, the W/D/L bar, the best move's explanation and
  patterns, and the other side's threat follow each move (debounced, off
  the main thread, on a session of its own). Take back, flip, copy FEN
  and PGN. From Train, from the board editor ("Analyse", with the side to
  move), from "Analyse a copy" in a game (allowed while help is, so not
  in a rated online or real-board game until it ends) and from the result
  card.
- **Advice** (scan result and board editor): the side to move (a
  toggle), check, mate or a drawn position, the odds and a verdict, the
  top five moves explained with their patterns, the threat against the
  side to move, and the opening. Nothing is saved. **Live advice** (camera
  devices) keeps the camera on, reads the board about once a second, and
  re-advises when a plausible new position appears (the side to move
  flips when it's one legal move from the last). It's its own screen,
  never attached to a game.
- **Play on a real board.** Pick the colours (you and an open-chess
  username, checked with `GET /api/user/{name}`), the speed, rated or
  not (a rated game counts once the other player confirms the result),
  and where the moves come from:
  - *Chessnut*: moves made on the board are recorded as they settle.
    A position the game can't reach (the board's `waiting`, or a
    mismatch held 1.5 s once the board is still) is shown as the board
    really is, with the core's `boardCorrections`: misplaced pieces red
    with an arrow to where they belong, pieces to remove with a red ✕,
    missing ones ghosted on their square. The squares light up on the
    board, with a haptic, a VoiceOver announcement and (Speak moves)
    "Knight on e5 should go back to f3". It clears when the board matches.
  - *Photos after each move*: take one or more photos (angles add up in
    the core's `matchPhotos`). A sure match plays itself (Undo for 5 s);
    otherwise pick from the top moves, tap the move, or edit the board.
    A photo no legal move explains shows the corrections as above.
    Without a board reader in the build, moves are tapped.
  - *By hand*: tap-tap on the board.
  The game is created with `POST /api/otb` and every move, take-back and
  result goes to `/api/otb/{id}/move|undo|end` in order through
  `OTBSender`, a queue kept in the store's settings and retried every 5 s
  (a take-back of a move not yet sent just drops it). Mate and stalemate
  end it on the server by themselves. Hints are allowed in unrated
  real-board games (the game shows as assisted) and not in rated ones.
  A real-board game can't be reopened once its screen is closed; it's
  saved in Games.
- **Help train the board reader** (needs the photo licence): frames are
  read at most twice a second (dropped while busy), held a second, and
  offered to the core's `Capture.offerFrame`; kept ones go to the outbox
  as `POST /api/photos` with `labelSource` `board` (Chessnut) or
  `player`, `gameId`, `angle`, `atMs` and `distance`, as a JPEG of at
  most 1280 px with no metadata. With a Chessnut the frames come from the
  camera (or the stream's raw camera, before the cutout is drawn); with
  photos, from the photos taken. Photo uploads follow low data as before.
- **Streaming** (Settings → Streaming): Twitch, YouTube or a custom RTMP
  server, the stream key (Keychain), 2.5/4/6 Mbps, a portrait option,
  and a copy recorded on the phone (Documents, listed under Recordings
  to share or delete). The picture is the back camera with the board,
  the players and the last five moves in a corner (an `ImageScreenObject`
  rendered with `ImageRenderer` on every move), 1280×720 by default, with
  the microphone. The preview on screen is the mixed picture. The screen
  stays awake while live; the stream stops when the app goes to the
  background (no background audio mode). HaishinKit 2.2.5 (BSD-3-Clause)
  comes from Swift Package Manager.
- **The overlay link** (`broadcastUrl`) is shown with Share: add it as a
  Browser Source in OBS, with `&bg=transparent`.

## Stubs and known gaps

- **App Attest** isn't used yet, so the app doesn't ask for it. Adding it
  needs a server endpoint first, then the
  `com.apple.developer.devicecheck.appattest-environment` entitlement.
- **Lichess** games in play come from `GET /api/account/playing`.
  Challenges to the player on Lichess aren't listed.
- **Labels from tapped moves**: with *By hand* (or photos) and a stream,
  the core learns of a move when it's entered, so frames from the
  moment between the move on the board and the tap can carry the old
  position. The core's settle window and distance limits keep most out.
- **Match system** reads the system's setting from the screen when the
  app becomes active; because the Info.plist starts the app dark, a
  change of the system setting while the app is open shows on the next
  return to the app.
- **Live board games** aren't resumable after the screen is closed (the
  server keeps the game; the moves already queued are still sent).
- **A game can't be created offline**: `POST /api/otb` needs a
  connection; after that, moves queue offline.
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
