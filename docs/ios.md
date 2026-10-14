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

## Still to come

- **Server:** `POST /api/games/offline`, `POST /api/games/{id}/confirm`,
  `GET /api/sync?since=`, a contributor photo upload, APNs for "your
  move", and `/api/feed` ids by receipt (a game uploaded late must still
  reach esports). See the ADR.
- **Bot games are rated** against the level's fixed rating
  (`Level::rating`, 800–2200): the level is known, so the opponent's
  strength is too, and Glicko gives little for beating a bot far below
  you, so it can't be farmed. Games with any help stay unrated.
  `offlineRecordJson` carries the level and its rating for the offline
  endpoint. The server's own `ai-level-N` games are still unrated today.
- **The app:** in its own repository, against this package.
