# chess-vision-train

Trains the chess board reader (`crates/chess-vision`) with candle, on the CPU,
an NVIDIA GPU (`--features cuda`) or a Mac's GPU (`--features metal`). The
`open-chess` program runs it as `open-chess train board-reader` (the same
options); this crate's `chess-vision-train` is the trainer on its own.

```sh
cargo run --release -p open-chess-vision-train --features metal -- \
    --datasets chessred open-chess-photos --internal --data-root /data/chess \
    --dinov2 dinov2_vits14_pretrain.pth --encoder small --image-size 392 \
    --out runs/small-392
```

`--internal` is needed whenever ChessReD is in (its licence is
noncommercial). Without ChessReD, `--datasets open-chess-photos` trains a model
you can ship.

The result is `runs/small-392/esl-board-reader-small-392.safetensors` (f16, with
its config and model card inside), for `CHESS_VISION_LOCAL_MODEL`,
`open-chess photo … --weights` ([docs/photos.md](../../docs/photos.md)) or the offline app's
`web/chess/nets/board-reader.safetensors`. `report.json` has every epoch's
validation scores and the test scores.

## Data

Two sources, both listed in `datasets.toml`, which the licence gate checks
before anything is read (`src/licences.rs`: unverified, no-derivatives and
ChessQueries material are refused; noncommercial data only with `--internal`):

| Dataset | Format | What |
|---|---|---|
| `chessred` | `annotations.json` and images, as published | 10,800 phone photos of 100 games, split by game. **CC BY-NC-SA 4.0** (its DOI record): internal-only models only |
| `open-chess-photos` | `labels.jsonl` + `photos/` | An open-chess server's labelled photos, sent under the open-chess photo licence (any model, commercial or not): `open-chess photos export --out /data/chess/open-chess-photos` (`--reviewed-only`: only labels two people agreed on, or an admin gave) |
| `open-chess-public` | `labels.jsonl` + `photos/` | The photos their senders also released as **CC0**: `open-chess photos export --public-only --out /data/chess/open-chess-public` |

`labels.jsonl` lines: `{"image": "photos/1.jpg", "placement": "<FEN placement,
White's side>", "group": "<same game or board>", "split": "train|val|test"}`
(`split` optional: a stable 80/10/10 split by group). A group never crosses
splits; the trainer checks.

## Internal-only models

Trained with `--internal` on noncommercial data, the model card says
`internal_only` and its licence is `LicenseRef-esl-internal-only`, with each
dataset and its licence listed. Such weights are for our own servers and
staff: the open-chess server and `esports-lab chess read` load them (the
server logs that it's internal-only), but `scripts/build-chess-app.sh`
refuses to put them in the Chess lab app, which every visitor downloads.
To ship a model trained on ChessReD, ask its authors for a commercial
licence first.

## What it does

- **Encoder** from Meta's DINOv2 (Apache-2.0): Meta's `.pth` checkpoints
  (`dl.fbaipublicfiles.com/dinov2/dinov2_vits14/dinov2_vits14_pretrain.pth`, and
  `vitb14` / `vitl14`) or Hugging Face's `facebook/dinov2-*` `model.safetensors`.
  The position embedding is resized (bicubic) to the image size.
- **Recipe**, after the published method: per-square cross-entropy; AdamW
  (weight decay 0.05, gradients clipped to norm 1) with a cosine schedule after
  a 3-epoch warm-up; decoder 1.4e-4, encoder 1.4e-5; the encoder frozen for the
  first 5 epochs, then trained with a fresh warm-up; 45 epochs.
- **Augmentation** (`src/augment.rs`): the photo padded to a phone's shape half
  the time (the reader letterboxes every photo to a square, so it must see bars
  of every size and none), turned to any side and tilted, sometimes seen at an
  angle, with changes of light, colour, focus and JPEG quality. Never cropped
  (a cut-off square would still be in the label) and never mirrored (that would
  change the position).
- Photos are decoded once and cached, scaled down, under `--out`.

`--encoder small --image-size 392` suits the app (phones); `base` or `large` at
518–644 the server. `--limit N` and `--no-augment` are for trying things out:
with augmentation off, a few photos should be learnt exactly, which is a quick
check that a change hasn't broken training.

## Options

| Option | Default | |
|---|---|---|
| `--datasets NAME…` | | Datasets from the manifest |
| `--manifest FILE` | this crate's `datasets.toml` | The datasets and their licences |
| `--data-root DIR` | | Where each dataset's `path` is |
| `--internal` | | Allow noncommercial data (the model is internal-only) |
| `--allow-share-alike` | | Allow share-alike data (CC BY-SA) |
| `--dinov2 FILE` | random (only for trying things out) | DINOv2 weights for the encoder |
| `--encoder small\|base\|large\|tiny` | `small` | ViT-S/B/L/14; `tiny` for tests |
| `--registers` | | DINOv2 with registers (`reg4` weights) |
| `--image-size N` | `392` | A multiple of 14 |
| `--decoder-layers N` / `--decoder-dim N` | `4` / `256` | |
| `--epochs N` | `45` | |
| `--batch N` | `16` | Photos per optimiser step |
| `--micro-batch N` | `0` (the whole batch) | Photos per forward and backward pass: a batch is split into passes of this many and their gradients added, so training is the same in less memory, only slower |
| `--lr-decoder` / `--lr-encoder` | `1.4e-4` / `1.4e-5` | |
| `--weight-decay` | `0.05` | |
| `--warmup-epochs` / `--unfreeze-epoch` | `3` / `5` | |
| `--limit N` | `0` (all) | At most this many photos per dataset and split |
| `--device cpu\|metal\|cuda\|cuda:N\|auto` | `auto` | |
| `--out DIR` | | Where the model, `report.json` and the photo cache go |
| `--name` / `--version` | `esl-board-reader` / today | The model card's |
| `--seed N` | `0` | |
| `--no-augment` | | Train on the photos as they are |

## Speed and memory

Each epoch prints where its time went, and on a Mac the most GPU memory
seen:

```
epoch   7 loss 0.7211 | chessred 1.4% (14.92 wrong/board)
          time: photos 0.3s, forward 210.4s, backward 402.7s, step 9.1s, validate 61.0s, peak 18.2 GB
```

`photos` is time spent waiting for the next batch (prepared on the other
cores while the GPU works): more than a few seconds means the CPU is the
bottleneck. Memory jumps when the encoder unfreezes (`--unfreeze-epoch`):
from then on every layer's activations are kept for the backward pass. If
Metal reports "Insufficient Memory", `--micro-batch 8` (or 4) trains the
same in a half (or a quarter) of the activation memory. Attention runs as
one fused softmax that keeps a single copy of each attention matrix for
the backward pass.
