# Training the engine's evaluation network

The loop, each generation:

```
engine N ──datagen──▶ data N ──bullet──▶ net N+1 ──SPRT──▶ engine N+1
```

1. **Data.** `open-chess-uci datagen` plays the engine against itself at
   a fixed node count and writes quiet positions straight in bullet's
   binary format, with a `.json` sidecar recording the engine, network,
   parameters, seed and settings that produced it. Keep every sidecar:
   a net is only as traceable as its data.

   ```sh
   open-chess-uci datagen --games 200000 --nodes 5000 --threads 12 --out data/gen1.bin
   open-chess-uci datagen --games 200000 --nodes 5000 --net nets/gen1.bin --out data/gen2.bin
   ```

   32 bytes a position: a billion positions is 32 GB. One laptop makes
   tens of millions a day; rented Linux cores make the rest
   (`scripts/engine/README.md` for the rules, `docs/engine.md` for the
   training server that spreads jobs across machines).

2. **Shuffle.** bullet trains sequentially, so interleave and shuffle the
   shards with `bullet-utils` from the bullet repository.

3. **Train.** `bullet/open_chess.rs` is the trainer configuration: copy
   it into bullet's `examples/` and run it with the `metal` (Apple
   Silicon) or `cuda` feature. First run the CPU backend on a few
   million positions with `HIDDEN_SIZE = 256` to prove the format round
   trips: the quantised net must load in `open-chess-uci` (`setoption
   name EvalFile value …`) and `bench` must run. Then compare a short
   Metal run with the CPU one before any long run.

4. **Test.** `scripts/engine/sprt.sh --net checkpoints/<id>/quantised.bin`
   (against no network the first time, `--base-net` against the last
   one after that). A net that passes becomes the engine's; a gauntlet
   run then gives its number.

5. **Ship.** Public nets go to `web/chess/nets/default.nnue` and the iOS
   app; the private bot's net stays out of the repository and loads from
   a secret at start-up (`docs/engine.md`).

Licences: `cargo deny` keeps GPL and AGPL out of the engine. Stockfish or
Leela may be used to *label* positions (they are tools, run separately);
the weights trained on those labels are ours. Their own networks never
ship with open-chess.
