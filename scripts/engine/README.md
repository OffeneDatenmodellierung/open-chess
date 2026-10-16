# Engine test rig

Scripts for measuring the engine (`crates/chess-uci`, binary
`open-chess-uci`). Every strength claim comes from one of these; nothing
else counts. They wrap [fastchess](https://github.com/Disservin/fastchess)
(a UCI match runner with built-in SPRT) and run on a laptop or a rented
Linux node alike — the only state is the `engines/` and `books/`
directories they create next to the repository's `target/`.

| Script | What it answers |
|---|---|
| `bench.sh` | Is this build a "no functional change"? Prints the node signature and nps; compares two builds. |
| `sprt.sh` | Is this change better? Plays the working tree against a base commit (or a saved binary) until the sequential test passes or fails. |
| `gauntlet.sh` | How strong is it? Plays a fixed set of engines with known ratings and prints an Elo estimate. |
| `setup.sh` | Installs fastchess, the opening books and the reference engines into `engines/` and `books/`. |

## Rules

- One change per SPRT run. Elo bounds `[0, 5]` for a gain, `[-5, 0]` for a
  simplification (a change that must not lose). Short time control
  `10+0.1` first, then `60+0.6` for anything that passed.
- Fixed seeds, no pondering, one thread per engine, hash 16 MB, the UHO
  opening book, no book inside the engine. Tests that break any of these
  are not comparable with earlier ones.
- `bench` before and after a refactor: a changed node count means the
  search changed, and that is an SPRT, not a refactor.
- Gauntlet Elo is relative to the opponents' published CCRL numbers and is
  an estimate; the number to watch is its change between builds.

## Typical day

```sh
scripts/engine/setup.sh                       # once
scripts/engine/bench.sh                       # signature of this build
scripts/engine/sprt.sh --set se_depth=6       # a parameter change, against the same build
scripts/engine/sprt.sh --base main            # the working tree against main
scripts/engine/gauntlet.sh --games 300        # where do we stand
```

Set `THREADS` to the number of games to play at once (default: all cores
but one). Results land in `target/engine-tests/<timestamp>-<name>.log`
and the PGNs next to them.
