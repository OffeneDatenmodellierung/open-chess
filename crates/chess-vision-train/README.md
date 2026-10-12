# chess-vision-train

Trains the chess board reader (`crates/chess-vision`) with candle, on the CPU,
an NVIDIA GPU (`--features cuda`) or a Mac's GPU (`--features metal`).

```sh
cargo run --release -p open-chess-vision-train --features metal -- \
    --datasets chessred esl-review --internal --data-root /data/chess \
    --dinov2 dinov2_vits14_pretrain.pth --encoder small --image-size 392 \
    --out runs/small-392
```

`--internal` is needed whenever ChessReD is in (its licence is
noncommercial). Without ChessReD, `--datasets esl-review` trains a model
you can ship.

The result is `runs/small-392/esl-board-reader-small-392.safetensors` (f16, with
its config and model card inside), for `CHESS_VISION_LOCAL_MODEL`,
`esports-lab chess read --model` or the offline app's
`web/chess/nets/board-reader.safetensors`. `report.json` has every epoch's
validation scores and the test scores.

## Data

Two sources, both listed in `datasets.toml`, which the licence gate checks
before anything is read (`src/licences.rs`: unverified, no-derivatives and
ChessQueries material are refused; noncommercial data only with `--internal`):

| Dataset | Format | What |
|---|---|---|
| `chessred` | `annotations.json` and images, as published | 10,800 phone photos of 100 games, split by game. **CC BY-NC-SA 4.0** (its DOI record): internal-only models only |
| `esl-review` | `labels.jsonl` + `photos/` | Our photos a person confirmed: `open-chess photos export --out /data/chess/esl-review` |

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
