# The chess engine and training app

`crates/chess-analysis` is our own chess engine and training toolkit, in
pure Rust with no I/O, so the same code runs in the chess server, the
fair-play worker, offline training jobs and, through `crates/chess-wasm`,
in the browser (`web/chess`, the Chess lab app).

## What it does

| Module | |
|---|---|
| `search` | Iterative-deepening principal-variation search: aspiration windows, transposition table, null-move and reverse-futility pruning, late-move reductions and pruning, futility and static-exchange pruning, check extensions, killer / countermove / history ordering, quiescence. Multi-PV for the "five best lines". Stops on depth, nodes or time. |
| `eval` | Hand-written evaluation: material and piece-square tables tapered by phase, mobility, king shelter and attack, rook files, pawn structure, passed pawns. |
| `nnue` | A (768 → H)×2 → 1 neural evaluation with incrementally updated accumulators. Used instead of `eval` when a trained network is loaded. |
| `tablebase` | Exact results for every three-piece ending, built on demand by retrograde analysis (KQK, KRK, KPK…). The search plays these perfectly. |
| `wdl` | Win / draw / loss odds from a score (a logistic model by game phase). |
| `explain` | Notation in plain English, token by token, and the notation dictionary. |
| `patterns` | Tactics and themes a move creates: forks, pins, skewers, discovered and double checks, back-rank and smothered mates, hanging pieces, castling, promotions, opening principles. |
| `openings` | Named openings by move order (ECO codes). |
| `photo` | Matching a board read from a photo against the position and every position one legal move away. |
| `analysis` | All of the above for a game position, as JSON for the app. |
| `book` | Polyglot opening books (`.bin`), the format most engines and GUIs (PicoChess among them) read. No I/O: pass the bytes. |
| `dictionary` | Keys for the position dictionary (the chess server's stored evaluations of positions). |
| `params` | The search's tunable parameters and their built-in defaults, which training tunes. |
| `bot` | Levels 1–8 (search budget plus a seeded margin for human-looking slips) and `best_move` for the max level. |

None of it is copied from another engine. The search techniques are the
standard published ones (the Chess Programming Wiki describes them all);
the code and its tuning are ours. Stockfish is GPL-3.0: we run it as a
separate program to measure against and to screen games on the server, and
never link it or ship it. Its trained networks are GPL too, so any network
we ship must be trained by us.

## How strong is it?

Chess has about 10⁴⁴ legal positions, so no engine looks at every future
move: the odds are estimates from a search to a limited depth, and they
sharpen as the game simplifies. They are exact once a forced mate is found,
in three-piece endings (built in) and up to seven pieces (Lichess's
tablebase server, when online).

`crates/chess-uci` is the engine as a UCI program, `open-chess-uci`
(in every release): any chess GUI, lichess-bot and the match runners
the engine-testing world uses run it. Its options are `Hash`, `Threads`,
`MultiPV`, `EvalFile` (a network), `UseTablebases`, and every search
tunable by name, so a test can change one number without a rebuild.
It also carries `bench` and `datagen` (below).

Strength is measured, never guessed, with `scripts/engine/` (see its
README for the rules):

```sh
scripts/engine/setup.sh                    # fastchess, the UHO book, reference engines
scripts/engine/bench.sh main               # same node signature as main = no functional change
scripts/engine/sprt.sh --set se_depth=6    # is one change better? stops when it knows
scripts/engine/gauntlet.sh                 # Elo estimate against engines with known ratings
```

`open-chess-uci bench` searches a fixed set of positions to a fixed
depth on one thread and prints the node count and speed. The count is
the build's signature: a refactor that keeps it has not changed the
search; one that changes it is a search change and needs an SPRT.

Results so far are in the pull request that introduced the engine. A
hand-written engine of this kind lands around 2200–2500 on the CCRL scale
depending on time; full-strength Stockfish is around 3600. Closing that gap
is mostly the evaluation network and the speed of the search.

## Distributed training

Search improvements are settled by games, not by guesswork, and the games
are spread over as many machines as you like. (This repository has the
training library, `crates/chess-train`; the coordinator that keeps the runs
and the `chess train` command line below still live in esports-lab, the
host application this code came from.) Every margin, reduction and
switch in the search is a named parameter (`crates/chess-analysis/src/params.rs`,
listed by `esports-lab chess train params`), and the server keeps:

- **runs**: SPRT tests (is this change better?), SPSA tuning (find better
  values for these parameters) and self-play data generation;
- **the champion**: the best parameter set so far (version 0 is the
  built-in defaults). A passing SPRT started with `--promote` makes a new
  champion; a finished SPSA run is tested against the champion
  automatically.

Any machine with an `ingest:any` token (`ESPORTS_API_TOKEN`) can help:

```sh
esports-lab chess train run                     # until Ctrl-C
esports-lab chess train run --threads 8 --memory-mb 2048 --hours 6
esports-lab chess train run --idle-only --ac-only --nice 15    # a laptop that's also in use
esports-lab chess train run --kinds datagen     # only self-play data
```

Before each job it fetches the latest state (champion, runs), claims a job
sized to its threads, plays it, uploads any self-play data and reports.
Limits come from `[chess_training]` in the config file, with flags on top:
threads (games at once), hash memory in total, which job kinds, a time
budget (`--hours`, `--max-jobs`), only-when-idle (pauses while other load
is above `max_load`), only-on-mains-power and CPU priority. Ctrl-C (or the
end of `--hours`) stops part-way through a job; the finished games still
count.

**Merging.** Each run is one database row; every result is applied in a
transaction that locks that row, so reports from many machines at once are
never lost:

- SPRT: the game-pair counts add up, and the test stops as soon as it has
  passed or failed;
- SPSA: each job is one step from the values current when it was claimed;
  steps are applied in the order they arrive (asynchronous SPSA, as
  fishtest does);
- datagen: shards (named by their SHA-256, so re-sending is harmless) go
  to the server's storage under `chess-training/data/`, and counts add up.

A machine that can't reach the server keeps its results and shards in
`<data_dir>/chess-training/outbox/` and sends them when it can. Results for
a run that has ended meanwhile are recorded but don't change it. Jobs only
go to machines running the same engine build as the run
(`chess_analysis::engine_id()`, bumped with `ENGINE_REVISION` whenever the
search changes behaviour), so results from different engines never mix.

Starting runs:

```sh
esports-lab chess train sprt --name "singular extensions" --set se_depth=6 --promote
esports-lab chess train spsa --name "pruning margins" --tune rfp_margin,fp_base,fp_margin --pairs 40000
esports-lab chess train datagen --name "net data 1" --positions 50000000
esports-lab chess train status
esports-lab chess train params --rust      # what to change in the built-in defaults
```

`esports-lab chess train local --set se_depth=6 --pairs 200` (or
`cargo run --release -p open-chess-train --example local --
se_depth=6 200`) plays a quick test on one machine without the server.

Time controls are a node budget per move by default (`nodes=20000`), which
gives the same games on a laptop and a server; `10+0.1` uses the clock
instead (machine speed then matters, so keep clock tests to one kind of
machine).

New search features arrive switched off behind a parameter (singular
extensions `se_depth`, continuation history `cont_hist`, correction history
`corr_hist`, best-move-stability time management `time_stable`) so the
cluster can test them before they become the default.

**Threads.** `Searcher::search_threads` searches one position on several
threads sharing a lock-free hash table (lazy SMP); the UCI engine's
`Threads` option uses it. Training plays one game per thread instead,
which uses cores more efficiently. The browser app stays single-threaded.

## Training an evaluation network

`training/README.md` has the full loop; in short:

1. **Data.** Self-play at a fixed node count, a few random opening moves
   per game (lopsided ones rejected), quiet positions only, written
   straight in the bullet trainer's binary format with a `.json`
   sidecar recording what produced it:

   ```sh
   open-chess-uci datagen --games 200000 --nodes 5000 --threads 12 --out data/gen1.bin
   open-chess-uci datagen --games 1000 --format text --out sample.txt   # the old text form
   ```

   Hundreds of millions of positions make a strong network; a million is
   enough to test the pipeline. Our own finished games can be added in the
   text format. The training server's datagen jobs (below) produce the
   same records from many machines.

2. **Train.** Small networks train on CPU with the built-in trainer:

   ```sh
   cargo run --release -p open-chess-analysis --example train -- \
       --data sample.txt --hidden 128 --epochs 10 --out net.nnue
   ```

   For real strength use [bullet](https://github.com/jw1912/bullet) (MIT;
   CUDA, ROCm or Metal) with `training/bullet/open_chess.rs`, which
   writes the layout `nnue::Network` reads (`QA = 255`, `QB = 64`,
   `SCALE = 400`).

3. **Check it's better** with `scripts/engine/sprt.sh --net net.nnue`.

4. **Ship it** as `web/chess/nets/default.nnue` (the app loads it when
   present) and point the fair-play worker at the same file with
   `CHESS_FAIRPLAY_APP_NET`, so the training-app fingerprint compares
   against what players actually see. A private net (the server's hidden
   top bot level) stays out of the repository and loads from a secret at
   start-up, as the fair-play model does.

The same data feeds the bot and the fraud model: the self-play games are
labelled engine play, and games from the training app carry declared
assistance labels (`chess.assist_marks`).

## The training app

`web/chess` is static files plus the wasm engine; CI's `app` job builds
them into the `chess-app` artifact, and each release has them as
`chess-app-vX.Y.Z.tar.gz`. Modes: bot, two players, analyse, online (this
server or lichess.org through the Board API). After one visit it works
offline: bot, analysis, built-in tablebases, editor. Online-only: play,
Lichess's 7-piece tablebase, the position dictionary and photo reading
(both on our server, after signing in).

Engine help (odds, lines, tactics, tablebases) is off during live Lichess
games, as Lichess's rules require. On our own server it can be switched
on; the app then declares it to the server as a label, and the fair-play
worker screens the game as usual.
