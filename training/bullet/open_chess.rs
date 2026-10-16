//! Training an open-chess evaluation network with bullet.
//!
//! This file is a bullet *example*: copy it into a clone of
//! https://github.com/jw1912/bullet as `examples/open_chess.rs`, add it to
//! `crates/bullet_lib/Cargo.toml` as the README there describes, and run
//!
//!     cargo run -r --example open_chess --features metal    # Apple Silicon
//!     cargo run -r --example open_chess --features cuda     # NVIDIA
//!     cargo run -r --example open_chess                     # CPU, for a tiny check
//!
//! It is the `simple` example with our numbers. The output format and
//! quantisation match `crates/chess-analysis/src/nnue.rs` exactly
//! (`QA = 255`, `QB = 64`, `SCALE = 400`, weights `l0w l0b l1w l1b` as
//! little-endian `i16`), so the quantised file bullet writes to
//! `checkpoints/<net_id>-<n>/quantised.bin` loads with
//! `setoption name EvalFile value quantised.bin` and in the app.
//!
//! bullet's API moves; if a method here no longer exists, line this file
//! up with the current `examples/simple.rs` and keep our constants.
//!
//! Data: `open-chess-uci datagen` writes bullet's `ChessBoard` records
//! directly (`--format bullet`). Shuffle and interleave shards first:
//!
//!     bullet-utils shuffle --input gen1.bin --output gen1.shuffled.bin --mem-used-mb 8192
//!     bullet-utils interleave --inputs gen1.shuffled.bin gen2.shuffled.bin --output train.bin
//!
//! Progression (each step an SPRT in the engine, see docs/engine.md):
//! HIDDEN 256 CReLU → 1024 SCReLU → output buckets → king buckets.
//! Only the first two shapes load into today's `nnue::Network`.

use bullet_lib::{
    game::inputs::Chess768,
    nn::optimiser::AdamW,
    trainer::{
        save::SavedFormat,
        schedule::{TrainingSchedule, TrainingSteps, lr, wdl},
        settings::LocalSettings,
    },
    value::{ValueTrainerBuilder, loader},
};

/// Neurons per perspective. 256 for the first (sanity) net; 1024 once the
/// data runs to hundreds of millions of positions.
const HIDDEN_SIZE: usize = 256;
/// Centipawns per network unit (nnue::SCALE).
const SCALE: i32 = 400;
/// Quantisation of the input layer (nnue::QA) and output layer (nnue::QB).
const QA: i16 = 255;
const QB: i16 = 64;

fn main() {
    let mut trainer = ValueTrainerBuilder::default()
        .dual_perspective()
        .optimiser(AdamW)
        .inputs(Chess768)
        .save_format(&[
            SavedFormat::id("l0w").round().quantise::<i16>(QA),
            SavedFormat::id("l0b").round().quantise::<i16>(QA),
            SavedFormat::id("l1w").round().quantise::<i16>(QB),
            SavedFormat::id("l1b").round().quantise::<i16>(QA * QB),
        ])
        .loss_fn(|output, target| output.sigmoid().squared_error(target))
        .build(|builder, stm_inputs, ntm_inputs| {
            let l0 = builder.new_affine("l0", 768, HIDDEN_SIZE);
            let l1 = builder.new_affine("l1", 2 * HIDDEN_SIZE, 1);
            let stm_hidden = l0.forward(stm_inputs).screlu();
            let ntm_hidden = l0.forward(ntm_inputs).screlu();
            l1.forward(stm_hidden.concat(ntm_hidden))
        });

    // One superbatch is batches_per_superbatch × batch_size positions:
    // 100 M here. Scale end_superbatch to the data: about 5–10 passes over
    // it is plenty; more only overfits.
    let schedule = TrainingSchedule {
        net_id: "open-chess-gen1".to_string(),
        eval_scale: SCALE as f32,
        steps: TrainingSteps { batch_size: 16_384, batches_per_superbatch: 6104, start_superbatch: 1, end_superbatch: 40 },
        // Weight of the game result against the search score in the target:
        // 0.75 is bullet's default; sweep 0.5–0.9 once a pipeline runs.
        wdl_scheduler: wdl::ConstantWDL { value: 0.75 },
        lr_scheduler: lr::StepLR { start: 0.001, gamma: 0.1, step: 18 },
        save_rate: 10,
    };

    let settings = LocalSettings { threads: 8, test_set: None, output_directory: "checkpoints", batch_queue_size: 64 };

    // bulletformat shards from `open-chess-uci datagen`, shuffled.
    let data_loader = loader::DirectSequentialDataLoader::new(&["data/train.bin"]);

    trainer.run(&schedule, &settings, &data_loader);
}
