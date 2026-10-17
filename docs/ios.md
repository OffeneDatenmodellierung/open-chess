# The iOS app's core

The iPhone and iPad app is SwiftUI over `crates/chess-mobile`
(`open-chess-mobile`): the same rules, bot, analysis, ratings, board
driver and board reader the server and the Chess lab use, compiled into a
Swift package through [UniFFI](https://mozilla.github.io/uniffi-rs/). The
app does the I/O (the screens, the camera, CoreBluetooth, the network,
background refresh, notifications) and calls in for every decision. Why,
and the rest of the plan: [ADR-0001](adr/0001-native-ios-app-on-the-open-chess-core.md).

## Building the Swift package

On a Mac with Xcode:

```sh
scripts/build-xcframework.sh            # dist/ios/OpenChessCore
METAL=1 scripts/build-xcframework.sh    # the board reader on the GPU
```

Add `dist/ios/OpenChessCore` to the Xcode project as a local package and
`import OpenChessCore`. Release tags build it too (the iOS core workflow)
and attach `OpenChessCore-vX.Y.Z.zip` to the release; so do pull requests
that change `crates/chess-mobile`, the build script or the workflow, and
it can be run by hand from the Actions tab once the workflow is on
`main`. Other pull requests don't, to save macOS minutes: CI tests the
crate on Linux like the others.

## What's in it

| Swift | For |
|---|---|
| `GameSession(fen:mode:)` | A game: `play(mv:atMs:)` (UCI or SAN), `undo`, `resign`, `agreeDraw`, `state()`, `legalUci()`, `botMove(level:seed:)`, `pgn` |
| … training | `hint(timeMs:)`, `analyseJson(…)` (odds and the best lines), `legalMovesJson()` (every move explained, with patterns), `explainJson`, `tablebaseJson`. All count as help (`assistance()`), and a rated online game (`GameMode.online(rated: true)`) refuses them |
| … offline games | `ratable()`, `offlineRecordJson(clientId:white:black:speed:startedMs:)`: the upload, with every move's time and the help used |
| `newRating()`, `ratingAfter(player:opponent:score:)`, `botRating(level:)`, `isProvisional` | Glicko-2 as the server computes it, for the provisional ratings shown between syncs |
| `ChessnutBoard(fen:moves:rotated:)` | The Chessnut boards: `start`, `receive(bytes:)`, `poll()`, `takeWrites()`, `opponentMove(uci:)` (lights its squares), `light`, `lightsOff`. Characteristics: `chessnutReadCharacteristic()`, `chessnutWriteCharacteristic()` |
| `BoardReader(weights:)` | A position from RGBA pixels: `readJson(rgba:width:height:)`, `imageSize()`, `cardJson()` (the weights' licence and credits). Refuses internal-only weights, as the Chess lab build does |
| `matchPhotoJson`, `checkFen`, `completeFen` | Updating a game from a photo |
| `Capture(fen:moves:policy:)` | A real game's labelled photos and video frames: `boardState` (what a connected board shows; `ChessnutBoard.boardNow()`), `movePlayed`, `offerFrame(atMs:nowMs:angle:readerPlacement:)` (keep it, with which label, or why not), `matchPhotos(readings:)` (one or more photos of an ordinary board against the legal moves) |
| `Store.open(path:)` | SQLite in the app's container: saved games, the outbox (`enqueue`, `pending(includePhotos:maxBytes:)`, `pendingTotals`, `markSent`, `pruneSent`), each speed's rating (`setServerRating`, `setLocalRating`, `ratings`) and settings |

Analysis results cross as JSON in the shapes the Chess lab already reads;
decode them with `Codable`. Everything else is typed.

### The Chessnut board over CoreBluetooth

1. Connect, discover the two characteristics, subscribe to the read one.
2. `board.start()`, then write every `takeWrites()` result (without
   response) to the write characteristic.
3. Each notification → `board.receive(bytes:)`; a timer every ~100 ms →
   `board.poll()`; after either, write `takeWrites()`.
4. `.move(uci, san)` events are moves made on the board: play them in the
   `GameSession` too. For the bot's or an online opponent's move, call
   `opponentMove(uci:)`: its squares light until it's made on the board.

### Low-data mode and sync

The app syncs on the interval the player chooses (manual, 1, 5, 10, 30 or
60 minutes) while it's open, and when iOS gives it background time. In
low-data mode, when `NWPathMonitor` says the path is constrained or
expensive, or off Wi-Fi with photos set to Wi-Fi only, it asks the store
for `pending(includePhotos: false, maxBytes: …)`. Each outbox item's key
is the app's own id for it, sent with the upload so a retry is harmless.

## The app

`ios/`: SwiftUI for iPhone and iPad, in the deployment's Chess lab colours
(royal navy and orange for esports), built with XcodeGen against this
package: `scripts/build-xcframework.sh`, then `cd ios && xcodegen` and
open `OpenChess.xcodeproj` ([ios/README.md](../ios/README.md)). The iOS
core workflow builds it for the simulator, unsigned, on pull requests that
change it.

It talks to the server's [iOS endpoints](api.md#the-ios-app): offline
games and their confirmation, one sync answer, board photos, push devices
and closing the account. Sign-in is OAuth (PKCE) with open-chess accounts
only. Online games are correspondence games through the Board API (and
real-time ones when low-data mode is off), open-chess first and Lichess
second.

**Bot games are rated** against the level's fixed rating
(`Level::rating`, 800–2200), offline and on the server alike: the level is
known, so the opponent's strength is too, and Glicko gives little for
beating a bot far below you, so it can't be farmed. Games with any help
stay unrated.

## Real boards, labelled frames and streaming

**A game on a real board** ("Play on a real board") is recorded live by
one of its players ([`POST /api/otb`](api.md#the-ios-app)): each move goes
to the server as it's made, so the game is followed online as any other.
The moves come from a connected Chessnut, from a photo or two after each
move (the board reader matches them against the legal moves; a sure match
is played, an unsure one asks), or by hand. It's rated like a game played
on one device: both players need accounts, the other confirms the result,
and any help makes it unrated.

**Labelled photos and video frames** come out of it for free
(`Capture` in the core). With a connected board, every frame is stamped
with the position the board showed at that moment; frames while the
position was changing (a hand over the board, a capture half made) are
skipped. With an ordinary board, the confirmed move labels the photos.
The phone keeps the frames the board reader got wrong and a few it got
right per position and camera angle, and sends them as training data under
the [photo licence](photo-licence.md) (`labelSource: "board"`).

**Streaming** goes from the phone to Twitch, YouTube or any RTMP server
(HaishinKit, BSD-3-Clause): the camera, with a cutout showing the board,
the last five moves and the players, and a copy recorded on the phone if
asked. Streaming from OBS instead, the server's live board overlay
(`/broadcast/{id}?key=…&bg=transparent`) is the same cutout as a browser
source.

**Lichess** sign-in is Lichess's own page (OAuth with PKCE; Lichess takes
the app's reversed-domain return address).

## Publishing to the App Store

The TestFlight workflow (`.github/workflows/testflight.yml`) builds a
signed release and uploads it to App Store Connect. Once:

1. **Apple Developer Program** membership (organisation or individual).
2. **The app id**: in Certificates, Identifiers & Profiles, register
   `org.offenedatenmodellierung.openchess` with Push Notifications (Xcode's
   automatic signing does this too on the first signed build).
3. **The app record** in App Store Connect (My Apps › +): name, primary
   language, the bundle id, a SKU.
4. **An API key** for uploads (Users and Access › Integrations › App
   Store Connect API › Team Keys, access **Admin**, which Xcode's
   cloud-managed signing needs), and these repository secrets:
   `APP_STORE_CONNECT_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID`,
   `APP_STORE_CONNECT_KEY` (the `.p8`, as text or base64), plus
   `APPLE_TEAM_ID`, which the macOS release already has. Optional:
   `BOARD_READER_URL` (weights to bundle).
5. **The server** the app starts with: the repository *variable*
   `CHESS_SERVER_URL` (Settings › Secrets and variables › Actions ›
   Variables), e.g. `https://chess.your-domain`. Without it the app asks
   for its server before signing in; players can change it under
   Account › Server either way.

   The macOS release's secrets ([releases.md](releases.md#secrets)) stay
   as they are: its Developer ID certificate signs the Mac binary, and its
   Developer-access key notarises it. The app is signed with an *Apple
   Distribution* certificate that Xcode makes and keeps in the team
   ("cloud-managed"), so nothing is exported and the Developer ID
   certificate isn't touched. Releases are tagged `v*` and app builds
   `ios-v*`, so neither starts the other.
5. **Push**: an APNs key (Keys › +, Apple Push Notifications service) for
   the server: `CHESS_APNS_KEY`, `CHESS_APNS_KEY_ID`, `CHESS_APNS_TEAM_ID`.
6. **Sign in with Apple**: tick Sign In with Apple on the app id (step 2),
   and make a key for it (Keys › +, Sign in with Apple, primary app id
   `org.offenedatenmodellierung.openchess`) for the server:
   `CHESS_APPLE_KEY`, `CHESS_APPLE_KEY_ID` (the team id comes from
   `CHESS_APNS_TEAM_ID`). Without them the app's Apple button answers that
   the server isn't set up for it.

   For the website as well (so an account made with Apple in the app can
   sign in there): Identifiers › + › Services IDs, e.g.
   `org.offenedatenmodellierung.openchess.web`; tick Sign In with Apple ›
   Configure: primary app id the app's, domain your server's host, return
   URL `https://<your server>/auth/apple/callback` (with the path prefix,
   if the server has one). Set `CHESS_APPLE_SERVICES_ID` to it.

   Apple also asks, on the app id's Sign in with Apple settings, for a
   server-to-server notification endpoint:
   `https://<your server>/auth/apple/notifications`. It hears when someone
   stops using their Apple ID with open-chess or deletes their Apple
   Account ([api.md](api.md)).

Each release: run *TestFlight* from the Actions tab (or push a tag
`ios-v1.0.0`). The build appears in App Store Connect › TestFlight after
Apple processes it (minutes to an hour); test it there, then fill in the
App Store page and submit it for review:

- **Privacy**: the policy URL is `https://<your server>/docs/privacy`
  ([privacy.md](privacy.md)); the App Privacy answers follow the app's
  privacy manifest (`ios/OpenChess/PrivacyInfo.xcprivacy`): user ID, email,
  gameplay content and photos, linked to the user, not used for tracking.
- **Screenshots**: Actions › *App Store screenshots* › Run workflow takes
  them on the simulators App Store Connect asks for: iPhone 6.9" (Pro Max),
  iPhone 6.1"/6.3" (Pro) and iPad 13", as JPEGs: Play, a training game, the analysis board, Train
  and a real-board game's setup, with sample ratings and Apple's 9:41 status
  bar (the run's artifact, and the `app-store-screenshots-images` branch).
  Each set goes in its own slot on the version page. The scenes are `ScreenshotMode` in Debug builds
  (`-OCScreenshot <scene>`); on a Mac, `scripts/ios-screenshots.sh` takes
  them from a Debug simulator build.
- **Review notes**: a demo account on the server you point the app at, and
  that a Chessnut board is optional (everything else works without one).
- **Export compliance**: the app uses only Apple's and standard encryption
  (`ITSAppUsesNonExemptEncryption` is false).
- **Age rating**: no objectionable content; user-generated content is
  board photos only, seen by reviewers on the server.

## Still to come

- **App Attest** isn't checked yet: uploads are trusted as the server's
  other games are, with fair-play screening.
- **Board-reader weights** licensed to ship, for the TestFlight workflow's
  `BOARD_READER_URL`.
