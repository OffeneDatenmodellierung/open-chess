//! Training the board reader (`crates/chess-vision`): `open-chess train
//! board-reader …` (the `open-chess` program with its `train` feature), or
//! this crate's own `chess-vision-train`.
//!
//! ```sh
//! open-chess train board-reader \
//!     --datasets chessred open-chess-photos --data-root /data/chess \
//!     --dinov2 dinov2_vits14_pretrain.pth --encoder small --image-size 392 --out runs/small-392
//! ```
//!
//! Every dataset goes through the licence gate (`datasets.toml`) first; the
//! ones used, with their licences and attribution, go into the model card
//! inside the weights file. Each epoch prints exact-board accuracy (every
//! square right) per dataset on its validation split; the best epoch is
//! kept, scored on the test splits and written as f16 safetensors.

mod augment;
mod data;
mod dinov2;
mod licences;
mod teacher;
mod train;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chess_vision::config::{DecoderConfig, EncoderConfig};
use chess_vision::{DatasetCredit, ModelCard, ModelConfig};
use clap::Parser;
use serde_json::json;

// Command-line switches, one bool each.
#[allow(clippy::struct_excessive_bools)]
/// The trainer's options.
#[derive(Debug, Clone, Parser)]
#[command(name = "chess-vision-train", about = "Train the chess board reader")]
pub struct Args {
    /// Datasets to train on, by their names in the manifest.
    #[arg(long, num_args = 1.., required = true)]
    datasets: Vec<String>,
    /// The datasets and their licences (default: the manifest built in,
    /// this crate's datasets.toml).
    #[arg(long)]
    manifest: Option<PathBuf>,
    /// Where the datasets' folders are.
    #[arg(long)]
    data_root: PathBuf,
    #[arg(long)]
    allow_share_alike: bool,
    /// An internal-only model: may also learn from noncommercial (CC BY-NC)
    /// data. The weights are stamped internal-only and must not leave our
    /// own servers (the Chess lab app build refuses them).
    #[arg(long)]
    internal: bool,
    /// Meta's DINOv2 weights for the encoder (a .pth from
    /// dl.fbaipublicfiles.com/dinov2, or Hugging Face's model.safetensors).
    /// Without it the encoder starts random: only for trying things out.
    #[arg(long)]
    dinov2: Option<PathBuf>,
    /// small (ViT-S/14, for the app), base or large (for the server); tiny
    /// for tests.
    #[arg(long, default_value = "small")]
    encoder: String,
    /// Add DINOv2's 4 register tokens (use with the reg4 checkpoints).
    #[arg(long)]
    registers: bool,
    /// A multiple of 14. 392 suits phones; the paper used 644 with ViT-L.
    #[arg(long, default_value_t = 392)]
    image_size: usize,
    #[arg(long, default_value_t = 4)]
    decoder_layers: usize,
    #[arg(long, default_value_t = 256)]
    decoder_dim: usize,
    #[arg(long, default_value_t = 45)]
    epochs: usize,
    #[arg(long, default_value_t = 16)]
    batch: usize,
    /// Photos per pass, when a whole batch doesn't fit in memory (Metal's
    /// "Insufficient Memory" once the encoder unfreezes): gradients are
    /// added over the passes, so training is the same, only slower.
    /// 0: the whole batch at once.
    #[arg(long, default_value_t = 0)]
    micro_batch: usize,
    #[arg(long, default_value_t = 1.4e-4)]
    lr_decoder: f64,
    #[arg(long, default_value_t = 1.4e-5)]
    lr_encoder: f64,
    #[arg(long, default_value_t = 0.05)]
    weight_decay: f64,
    #[arg(long, default_value_t = 3.0)]
    warmup_epochs: f64,
    #[arg(long, default_value_t = 5)]
    unfreeze_epoch: usize,
    /// At most this many photos per dataset and split (to try things out).
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// cpu, metal, cuda, cuda:N or auto.
    #[arg(long, default_value = "auto")]
    device: String,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "esl-board-reader")]
    name: String,
    /// Default: today's date.
    #[arg(long)]
    version: Option<String>,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Train on the photos as they are (to check the network can fit them).
    #[arg(long)]
    no_augment: bool,
    /// A board reader trained earlier (typically a big one: `--encoder
    /// base`, 518) to learn from as well as the labels: distillation. It
    /// reads each training photo once, before training. Needed for
    /// unlabelled photos (datasets of format `photos`). A teacher trained on
    /// noncommercial data makes this model internal-only too.
    #[arg(long)]
    teacher: Option<PathBuf>,
    /// The teacher's share of a labelled photo's loss (the labels get the
    /// rest). Unlabelled photos learn from the teacher alone.
    #[arg(long, default_value_t = 0.5)]
    distill_weight: f64,
    /// Softens the teacher's and the student's readings before they are
    /// compared, so the teacher's second guesses count too.
    #[arg(long, default_value_t = 2.0)]
    distill_temperature: f64,
    /// Unlabelled photos are kept only if the teacher reads a plausible
    /// position with at least this confidence (every square's top
    /// probability, multiplied).
    #[arg(long, default_value_t = 0.5)]
    teacher_min_confidence: f32,
}

fn encoder(name: &str, registers: bool) -> Result<EncoderConfig> {
    let mut e = match name {
        "small" => EncoderConfig::vit_small(),
        "base" => EncoderConfig::vit_base(),
        "large" => EncoderConfig::vit_large(),
        "tiny" => EncoderConfig { dim: 32, depth: 2, heads: 2, mlp_dim: 64, patch: 14, registers: 0 },
        other => bail!("--encoder {other}: small, base, large or tiny"),
    };
    if registers {
        e.registers = 4;
    }
    Ok(e)
}

fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}.{m:02}.{d:02}")
}

/// The datasets a model owes credit to: its own and, when it was taught,
/// its teacher's (each once).
fn credits(chosen: &[&licences::Dataset], teacher: Option<&ModelCard>) -> Vec<DatasetCredit> {
    let mut out: Vec<DatasetCredit> = chosen
        .iter()
        .map(|d| DatasetCredit {
            name: d.name.clone(),
            licence: d.licence.clone(),
            attribution: d.attribution.clone(),
        })
        .collect();
    for c in teacher.map(|t| t.datasets.as_slice()).unwrap_or_default() {
        if !out.iter().any(|d| d.name == c.name) {
            out.push(c.clone());
        }
    }
    out
}

/// Train with these options.
pub fn run(a: &Args) -> Result<()> {
    let (manifest, from) = match &a.manifest {
        Some(p) => (licences::load_manifest(p)?, p.display().to_string()),
        None => (licences::parse_manifest(licences::BUILT_IN)?, "the built-in datasets.toml".to_owned()),
    };
    let missing: Vec<&String> = a.datasets.iter().filter(|n| !manifest.contains_key(*n)).collect();
    if !missing.is_empty() {
        bail!("not in {from}: {missing:?}");
    }
    let chosen: Vec<&licences::Dataset> = a.datasets.iter().map(|n| &manifest[n]).collect();
    let policy = licences::Policy { allow_share_alike: a.allow_share_alike, internal: a.internal };
    licences::check(&chosen, policy)?;
    // A teacher passes on what it learnt from, licences included.
    let teacher_card = match &a.teacher {
        Some(p) => {
            let bytes = std::fs::read(p).with_context(|| format!("reading the teacher {}", p.display()))?;
            Some(chess_vision::read_metadata(&bytes)?.1)
        }
        None => None,
    };
    if let Some(c) = &teacher_card {
        if c.internal_only && !a.internal {
            bail!(
                "the teacher {} is internal-only (trained on noncommercial data), so a model it teaches is too: pass --internal",
                c.name
            );
        }
        if c.internal_only {
            eprintln!("note: the teacher {} is internal-only, so this model is too", c.name);
        }
    }
    anyhow::ensure!(
        (0.0..=1.0).contains(&a.distill_weight) && a.distill_temperature > 0.0,
        "--distill-weight goes from 0 to 1, and --distill-temperature must be above 0"
    );
    let internal_only = chosen.iter().any(|d| licences::needs_internal(d))
        || teacher_card.as_ref().is_some_and(|c| c.internal_only);
    if chosen.iter().any(|d| licences::needs_internal(d)) {
        eprintln!(
            "note: trained on noncommercial data ({}): the model is internal-only and won't go in the Chess lab app",
            chosen
                .iter()
                .filter(|d| licences::needs_internal(d))
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut samples = Vec::new();
    for d in &chosen {
        let mut got = data::load(d, &a.data_root)?;
        if a.limit > 0 {
            let mut kept: HashMap<data::Split, usize> = HashMap::new();
            got.retain(|s| {
                let n = kept.entry(s.split).or_default();
                *n += 1;
                *n <= a.limit
            });
        }
        let count = |sp| got.iter().filter(|s| s.split == sp).count();
        println!(
            "{}: {} train, {} val, {} test",
            d.name,
            count(data::Split::Train),
            count(data::Split::Val),
            count(data::Split::Test)
        );
        samples.extend(got);
    }
    data::check_no_leaks(&samples)?;
    if a.teacher.is_none() && samples.iter().any(|s| !s.labelled) {
        bail!("unlabelled photos (datasets of format `photos`) need a teacher to learn from: give --teacher");
    }

    let cfg = ModelConfig {
        image_size: a.image_size,
        encoder: encoder(&a.encoder, a.registers)?,
        decoder: DecoderConfig {
            dim: a.decoder_dim,
            layers: a.decoder_layers,
            heads: 8.min(a.decoder_dim / 2).max(1),
            ffn_dim: 4 * a.decoder_dim,
        },
        mean: [0.485, 0.456, 0.406],
        std: [0.229, 0.224, 0.225],
    };
    cfg.validate().map_err(anyhow::Error::msg)?;
    let device = chess_vision::device(&a.device)?;
    println!("training on {}", chess_vision::device_name(&device));
    std::fs::create_dir_all(&a.out).with_context(|| format!("creating {}", a.out.display()))?;
    let cache_dir = a.out.join("photo-cache");
    std::fs::create_dir_all(&cache_dir)?;

    let distill = match &a.teacher {
        Some(path) => {
            let t = teacher::Teacher::load(path, &device)?;
            let train_idx: Vec<usize> =
                (0..samples.len()).filter(|&i| samples[i].split == data::Split::Train).collect();
            let refs: Vec<&data::Sample> = train_idx.iter().map(|&i| &samples[i]).collect();
            let readings = t.read_all(&refs, &cache_dir, &a.out.join("teacher-cache"), a.batch)?;
            drop(t);
            let mut keep = vec![true; samples.len()];
            let (mut unlabelled, mut kept) = (0, 0);
            for (i, r) in train_idx.into_iter().zip(readings) {
                if !samples[i].labelled {
                    unlabelled += 1;
                    if !(r.plausible && r.confidence >= a.teacher_min_confidence) {
                        keep[i] = false;
                        continue;
                    }
                    kept += 1;
                }
                samples[i].soft = Some(r.log_probs);
            }
            let mut k = keep.into_iter();
            samples.retain(|_| k.next().unwrap_or(true));
            if unlabelled > 0 {
                println!(
                    "teacher: kept {kept} of {unlabelled} unlabelled photos (a plausible position, confidence ≥ {})",
                    a.teacher_min_confidence
                );
            }
            Some(train::Distill { weight: a.distill_weight, temperature: a.distill_temperature })
        }
        None => None,
    };

    let (mut vm, net) = train::build(&cfg, a.dinov2.as_deref(), &device)?;
    let base = match &a.dinov2 {
        Some(_) => format!(
            "DINOv2 ViT-{}/14{} (Meta, Apache-2.0)",
            a.encoder.chars().next().unwrap_or('?').to_ascii_uppercase(),
            if a.registers { " reg4" } else { "" }
        ),
        None => "random initialisation (not for use)".to_owned(),
    };
    let base = match &teacher_card {
        Some(c) => format!("{base}; taught by {} {} ({})", c.name, c.version, c.base),
        None => base,
    };
    let settings = train::Settings {
        epochs: a.epochs,
        batch: a.batch,
        micro_batch: a.micro_batch,
        lr_decoder: a.lr_decoder,
        lr_encoder: a.lr_encoder,
        weight_decay: a.weight_decay,
        warmup_epochs: a.warmup_epochs,
        unfreeze_epoch: a.unfreeze_epoch,
        clip: 1.0,
        seed: a.seed,
        augment: !a.no_augment,
        cache_dir,
        distill,
    };
    let history =
        train::run(&mut vm, &net, &cfg, &samples, &settings, &device, &a.out.join("best.safetensors"))?;

    let test: Vec<&data::Sample> = samples.iter().filter(|s| s.split == data::Split::Test).collect();
    let test_scores = train::evaluate(&net, &test, &cfg, &settings, &device)?;
    let card = ModelCard {
        name: a.name.clone(),
        version: a.version.clone().unwrap_or_else(today),
        licence: if internal_only {
            "LicenseRef-esl-internal-only".into()
        } else {
            "MIT OR Apache-2.0".into()
        },
        base,
        datasets: credits(&chosen, teacher_card.as_ref()),
        accuracy: test_scores.iter().map(|(k, v)| (k.clone(), v.exact as f32)).collect(),
        internal_only,
    };
    let file = a.out.join(format!("{}-{}-{}.safetensors", a.name, a.encoder, a.image_size));
    let n = train::export(&vm, &cfg, &card, &file)?;
    std::fs::write(
        a.out.join("report.json"),
        serde_json::to_vec_pretty(&json!({"card": card, "test": test_scores, "history": history}))?,
    )?;
    println!("test: {}", serde_json::to_string(&test_scores)?);
    println!("wrote {} ({:.1}M parameters)", file.display(), n as f64 / 1e6);
    Ok(())
}
