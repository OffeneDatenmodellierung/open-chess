# chess-vision

Reads a chess position from a photo of a real board. Runs with
[candle](https://github.com/huggingface/candle): natively on the chess server
(`CHESS_VISION_LOCAL_MODEL`) and a host application's command line (esports-lab's `chess read`; CPU, or the GPU
with the `metal` or `cuda` feature; the Mac build turns Metal on), and as wasm
on the CPU in the offline chess app
(`web/chess/vision-worker.js`, weights at `web/chess/nets/board-reader.safetensors`).

```rust
let reader = Reader::from_safetensors(&std::fs::read("board-reader.safetensors")?)?;
let reading = reader.read_encoded(&jpeg_bytes)?;      // feature "decode"
reading.placement;      // "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR"
reading.squares;        // 64 × 13 probabilities, a1 first
reading.is_sure(0.5);   // no unsure square, plausible, confident
```

From esports-lab's command line (`esports-lab chess read`; on a Mac it runs on the GPU
with Metal):

```sh
esports-lab chess read photo1.jpg photo2.jpg                 # what each photo shows
esports-lab chess read data/recordings/club-night --pgn game.pgn --review-dir unsure/
esports-lab chess read twitch:somechannel --fps 2            # a live feed, Ctrl-C to stop
```

Following a game (photos after each move, or video frames):
`chess_analysis::photo::match_after_move_probs` weighs every legal move against
a reading, and `tracker::GameTracker` takes a move only once it has held for a
few frames, searching two moves ahead when a frame was missed.

## Method and licence

The network follows the method published in Joël Seytre, *ChessQueries: Toward
Better Chess Board Recognition* ([arXiv:2608.30762](https://arxiv.org/abs/2608.30762)):
a DINOv2 ViT/14 encoder, 64 learned square queries, a 4-layer DETR-style
decoder and one shared 13-way head. It is an independent implementation written
from the paper. It contains none of the ChessQueries code, and loads none of
its weights or its SLCC data: those are licensed for noncommercial use only
(PolyForm Noncommercial 1.0.0 and CC BY-NC 4.0).

This crate ships no weights. They come from `crates/chess-vision-train`, which
starts from Meta's DINOv2 (Apache-2.0; note DINOv3 has a different, custom
licence) and refuses any dataset whose licence doesn't allow commercial use or
that nobody has verified (its `datasets.toml`), except for an internal-only
model (`--internal`, stamped `internal_only` and kept out of the app). The two
sources are our own confirmed photos and ChessReD (CC BY-NC-SA 4.0, so
internal-only). Each weights file carries its model card (licence, base, datasets with
attribution, accuracy) in its metadata: `Reader::card`.

The trainer runs this crate's network (`Network::new_trainable`: the same
parameters and maths through operations that carry gradients, where reading
uses candle's faster fused kernels) and prepares photos with this crate's
`preprocess`, so training and reading see the same network and the same pixels.

## Speed

On a 2-core cloud VM, one photo through ViT-S/14 took 4.2 s at 518 px and 1.9 s
at 392 px. A server with more cores is proportionally faster; the browser
(single-threaded wasm) is slower, so the offline model should be small (ViT-S
at about 392 px, stored in f16: about 50 MB). The paper's ViT-L at 644 px is
for the server, ideally on a GPU.

## Building for wasm

```sh
RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
  cargo build -p open-chess-vision --features wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript --out-dir web/chess/pkg \
  target/wasm32-unknown-unknown/release/open_chess_vision.wasm
```

(The CI `chess-pwa` job does this.)
