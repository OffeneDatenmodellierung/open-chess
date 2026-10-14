//! The training loop: per-square cross-entropy; AdamW (weight decay 0.05,
//! gradients clipped to norm 1) on a cosine schedule after a linear
//! warm-up; the decoder at a higher rate than the encoder; the encoder
//! frozen for the first epochs (its output detached, so no gradients flow
//! into it), then trained with a fresh warm-up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use candle_core::backprop::GradStore;
use candle_core::{D, DType, Device, Tensor, Var};
use candle_nn::{AdamW, Optimizer, ParamsAdamW, VarBuilder, VarMap};
use chess_vision::network::Network;
use chess_vision::{Image, ModelCard, ModelConfig, preprocess, weights};
use image::RgbImage;
use rayon::prelude::*;
use serde::Serialize;

use crate::augment::{Rng, augment};
use crate::data::{Sample, Split};

pub struct Settings {
    pub epochs: usize,
    pub batch: usize,
    /// Photos per forward and backward pass: a batch is split into passes
    /// of this many and their gradients added, so a smaller pass trains the
    /// same as the whole batch in less memory. 0: the whole batch at once.
    pub micro_batch: usize,
    pub lr_decoder: f64,
    pub lr_encoder: f64,
    pub weight_decay: f64,
    pub warmup_epochs: f64,
    pub unfreeze_epoch: usize,
    pub clip: f64,
    pub seed: u64,
    pub augment: bool,
    pub cache_dir: PathBuf,
    /// Learning from a teacher's readings too (every training sample then
    /// carries one, `Sample::soft`).
    pub distill: Option<Distill>,
}

/// How much the teacher counts, and how much of its uncertainty is passed
/// on.
#[derive(Debug, Clone, Copy)]
pub struct Distill {
    /// The teacher's share of a labelled photo's loss (the labels get the
    /// rest); an unlabelled photo learns from the teacher alone.
    pub weight: f64,
    /// Softens both readings before they are compared, so the teacher's
    /// second guesses ("a bishop, perhaps a pawn") count too.
    pub temperature: f64,
}

/// The loss when distilling, over rows of squares: cross-entropy with the
/// labels (where a row has them, `labelled` 1, else 0) weighted `1 − w`,
/// plus `w · T²` times the KL divergence from the teacher's softened
/// reading to the student's. The T² keeps the teacher's gradients the same
/// size whatever the temperature (Hinton et al., 2015).
///
/// `logits` `(rows, 13)` f32, `labels` `(rows)` u32, `teacher` `(rows, 13)`
/// log probabilities, `labelled` `(rows)` f32.
pub fn distill_loss(
    logits: &Tensor,
    labels: &Tensor,
    teacher: &Tensor,
    labelled: &Tensor,
    d: Distill,
) -> candle_core::Result<Tensor> {
    use candle_nn::ops::log_softmax;
    let rows = logits.dim(0)? as f64;
    let log_p = log_softmax(logits, D::Minus1)?;
    let nll = log_p.gather(&labels.unsqueeze(1)?, 1)?.squeeze(1)?.neg()?;
    let ce = ((nll * labelled)?.sum_all()? * ((1.0 - d.weight) / rows))?;
    let t = d.temperature;
    let log_s = log_softmax(&(logits / t)?, D::Minus1)?;
    let log_q = log_softmax(&(teacher / t)?, D::Minus1)?;
    let kl = (log_q.exp()? * (log_q - log_s)?)?.sum_all()?;
    ce + (kl * (d.weight * t * t / rows))?
}

/// Learning-rate multiplier: 0 before `start`, linear warm-up over
/// `warmup` steps, then cosine to 0 at `total`.
pub fn schedule(step: usize, start: usize, warmup: usize, total: usize) -> f64 {
    if step < start {
        return 0.0;
    }
    let s = step - start;
    if s < warmup {
        return (s + 1) as f64 / warmup as f64;
    }
    let span = total.saturating_sub(start + warmup).max(1);
    let t = ((s - warmup) as f64 / span as f64).min(1.0);
    0.5 * (1.0 + (std::f64::consts::PI * t).cos())
}

/// A photo, decoded upright and scaled down to twice the model's size (what
/// augmentation and the reader's own scaling need), kept as a JPEG in the
/// cache so later epochs don't decode 12-megapixel originals again.
pub(crate) fn load_photo(path: &Path, size: u32, cache: &Path) -> Result<RgbImage> {
    use sha2::{Digest, Sha256};
    let key = Sha256::digest(format!("{}:{size}", path.display()).as_bytes());
    let cached = cache.join(format!("{}.jpg", hex(&key[..12])));
    if let Ok(img) = image::open(&cached) {
        return Ok(img.to_rgb8());
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut img = chess_vision::decode(&bytes).with_context(|| format!("decoding {}", path.display()))?;
    let long = img.width().max(img.height());
    if long > 2 * size {
        let s = f64::from(2 * size) / f64::from(long);
        let (w, h) =
            ((f64::from(img.width()) * s).round() as u32, (f64::from(img.height()) * s).round() as u32);
        img = image::imageops::resize(&img, w.max(1), h.max(1), image::imageops::FilterType::Triangle);
    }
    let mut buf = Vec::new();
    if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95).encode_image(&img).is_ok() {
        let _ = std::fs::write(&cached, buf);
    }
    Ok(img)
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// A batch prepared on the CPU: the network input `(n, 3, s, s)` and the
/// labels `(n·64)`, ready to go to the device.
struct Prepared {
    x: Vec<f32>,
    y: Vec<u32>,
    n: usize,
    /// When distilling: the teacher's log probabilities `(n·64·13)` and
    /// whether each square has a label `(n·64)`.
    soft: Option<(Vec<f32>, Vec<f32>)>,
}

/// Decode, augment and scale a batch's photos (on every core).
fn prepare(
    samples: &[&Sample],
    cfg: &ModelConfig,
    train: Option<(u64, usize)>,
    cache: &Path,
) -> Result<Prepared> {
    let fill = preprocess::pad_colour(cfg);
    let size = cfg.image_size as u32;
    let inputs: Vec<Vec<f32>> = samples
        .par_iter()
        .enumerate()
        .map(|(i, s)| {
            let mut img = load_photo(&s.image, size, cache)?;
            if let Some((seed, epoch)) = train {
                let mut rng = Rng::new(
                    seed ^ (epoch as u64).wrapping_mul(0x9E37_79B9)
                        ^ hash_str(&s.image.to_string_lossy())
                        ^ i as u64,
                );
                img = augment(&img, &mut rng, fill);
            }
            let px = Image {
                width: img.width() as usize,
                height: img.height() as usize,
                channels: 3,
                pixels: img.as_raw(),
            };
            Ok(preprocess::prepare(&px, cfg)?)
        })
        .collect::<Result<_>>()?;
    let y: Vec<u32> = samples.iter().flat_map(|s| s.labels.iter().map(|&l| u32::from(l))).collect();
    let soft = if train.is_some() && samples.iter().all(|s| s.soft.is_some()) {
        let t = samples.iter().flat_map(|s| s.soft.as_deref().unwrap_or_default().iter().copied()).collect();
        let labelled = samples
            .iter()
            .flat_map(|s| std::iter::repeat_n(if s.labelled { 1.0 } else { 0.0 }, 64))
            .collect();
        Some((t, labelled))
    } else {
        None
    };
    Ok(Prepared { n: inputs.len(), x: inputs.concat(), y, soft })
}

/// When distilling, the teacher's reading `(rows, 13)` and which squares
/// have labels `(rows)`, on the device.
type Soft = Option<(Tensor, Tensor)>;

/// A prepared batch on the device: input, labels and [`Soft`].
fn upload(p: Prepared, cfg: &ModelConfig, device: &Device) -> Result<(Tensor, Tensor, Soft)> {
    let s = cfg.image_size;
    let rows = p.n * 64;
    let x = Tensor::from_vec(p.x, (p.n, 3, s, s), &Device::Cpu)?.to_device(device)?;
    let y = Tensor::from_vec(p.y, rows, &Device::Cpu)?.to_device(device)?;
    let soft = match p.soft {
        Some((t, l)) => Some((
            Tensor::from_vec(t, (rows, 13), &Device::Cpu)?.to_device(device)?,
            Tensor::from_vec(l, rows, &Device::Cpu)?.to_device(device)?,
        )),
        None => None,
    };
    Ok((x, y, soft))
}

/// A batch as network input `(n, 3, s, s)` and labels `(n·64)`.
fn batch(
    samples: &[&Sample],
    cfg: &ModelConfig,
    train: Option<(u64, usize)>,
    cache: &Path,
    device: &Device,
) -> Result<(Tensor, Tensor)> {
    let (x, y, _) = upload(prepare(samples, cfg, train, cache)?, cfg, device)?;
    Ok((x, y))
}

fn hash_str(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Score {
    pub boards: usize,
    /// Boards with every square right.
    pub exact: f64,
    pub wrong_per_board: f64,
}

/// Exact-board accuracy and wrong squares per board, per dataset.
pub fn evaluate(
    net: &Network,
    samples: &[&Sample],
    cfg: &ModelConfig,
    s: &Settings,
    device: &Device,
) -> Result<HashMap<String, Score>> {
    let mut by: HashMap<String, (usize, usize, usize)> = HashMap::new();
    let pass = if s.micro_batch == 0 { s.batch } else { s.micro_batch };
    for chunk in samples.chunks(pass.max(1)) {
        let (x, y) = batch(chunk, cfg, None, &s.cache_dir, device)?;
        let pred =
            net.logits(&x.to_dtype(DType::F32)?)?.argmax(D::Minus1)?.flatten_all()?.to_vec1::<u32>()?;
        let y = y.to_vec1::<u32>()?;
        for (i, sample) in chunk.iter().enumerate() {
            let wrong = (0..64).filter(|&k| pred[i * 64 + k] != y[i * 64 + k]).count();
            let e = by.entry(sample.dataset.clone()).or_default();
            e.0 += 1;
            e.1 += usize::from(wrong == 0);
            e.2 += wrong;
        }
    }
    Ok(by
        .into_iter()
        .map(|(k, (n, exact, wrong))| {
            (
                k,
                Score {
                    boards: n,
                    exact: exact as f64 / n.max(1) as f64,
                    wrong_per_board: wrong as f64 / n.max(1) as f64,
                },
            )
        })
        .collect())
}

/// Clip the gradients to a total norm of `max`. The norm is added up on
/// the device and read back once (a read per parameter stalls the GPU some
/// two hundred times a step).
fn clip(grads: &mut GradStore, vars: &[Var], max: f64) -> Result<f64> {
    let mut squares = Vec::new();
    for v in vars {
        if let Some(g) = grads.get(v.as_tensor()) {
            squares.push(g.sqr()?.sum_all()?.to_dtype(DType::F32)?);
        }
    }
    if squares.is_empty() {
        return Ok(0.0);
    }
    let norm = f64::from(Tensor::stack(&squares, 0)?.sum_all()?.to_scalar::<f32>()?).sqrt();
    if norm > max && norm.is_finite() {
        let scale = max / norm;
        for v in vars {
            if let Some(g) = grads.get(v.as_tensor()) {
                let scaled = (g * scale)?;
                grads.insert(v.as_tensor(), scaled);
            }
        }
    }
    Ok(norm)
}

/// Where an epoch's time went (seconds), and the most device memory seen.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Timing {
    /// Waiting for photos (prepared ahead on other cores while the device
    /// works: more than a little means the CPU is the bottleneck).
    pub data: f64,
    pub forward: f64,
    pub backward: f64,
    /// Clipping and the optimiser's step.
    pub step: f64,
    pub validate: f64,
    /// Peak device memory in bytes (Metal only).
    pub peak_bytes: Option<u64>,
}

impl Timing {
    fn line(&self) -> String {
        let mem = self.peak_bytes.map_or_else(String::new, |b| format!(", peak {:.1} GB", b as f64 / 1e9));
        format!(
            "time: photos {:.1}s, forward {:.1}s, backward {:.1}s, step {:.1}s, validate {:.1}s{mem}",
            self.data, self.forward, self.backward, self.step, self.validate
        )
    }
}

/// Device memory in use now (Metal only).
#[allow(unused_variables)]
fn device_bytes(device: &Device) -> Option<u64> {
    #[cfg(feature = "metal")]
    if let Device::Metal(m) = device {
        return Some(m.metal_device().current_allocated_size() as u64);
    }
    None
}

/// Wait for the device to finish (so the time is the work's, and Metal can
/// release buffers), noting its memory first.
fn settle(device: &Device, peak: &mut Option<u64>) -> Result<()> {
    if let Some(b) = device_bytes(device) {
        *peak = Some(peak.unwrap_or(0).max(b));
    }
    device.synchronize()?;
    Ok(())
}

#[derive(Serialize)]
pub struct EpochLog {
    pub epoch: usize,
    pub loss: f64,
    pub frozen: bool,
    pub secs: f64,
    pub time: Timing,
    pub val: HashMap<String, Score>,
}

/// Train; the best parameters by mean exact-board validation accuracy end
/// up in `vm`. Returns the epoch log.
#[allow(clippy::too_many_arguments)]
pub fn run(
    vm: &mut VarMap,
    net: &Network,
    cfg: &ModelConfig,
    samples: &[Sample],
    s: &Settings,
    device: &Device,
    best_path: &Path,
) -> Result<Vec<EpochLog>> {
    let train: Vec<&Sample> = samples.iter().filter(|x| x.split == Split::Train).collect();
    let val: Vec<&Sample> = samples.iter().filter(|x| x.split == Split::Val).collect();
    anyhow::ensure!(!train.is_empty(), "no training photos");
    let (mut enc, mut dec) = (Vec::new(), Vec::new());
    {
        let data = vm.data().lock().map_err(|_| anyhow::anyhow!("parameters poisoned"))?;
        let mut names: Vec<&String> = data.keys().collect();
        names.sort();
        for n in names {
            if n.starts_with("encoder.") { enc.push(data[n].clone()) } else { dec.push(data[n].clone()) }
        }
    }
    let params = |lr| ParamsAdamW { lr, weight_decay: s.weight_decay, ..ParamsAdamW::default() };
    let mut opt_dec = AdamW::new(dec.clone(), params(s.lr_decoder))?;
    let mut opt_enc = AdamW::new(enc.clone(), params(s.lr_encoder))?;
    let all: Vec<Var> = enc.iter().chain(&dec).cloned().collect();

    let per_epoch = train.len().div_ceil(s.batch);
    let total = per_epoch * s.epochs;
    let warm = ((s.warmup_epochs * per_epoch as f64) as usize).max(1);
    let unfreeze = s.unfreeze_epoch * per_epoch;
    let mut order: Vec<usize> = (0..train.len()).collect();
    let mut rng = Rng::new(s.seed);
    let mut best = -1.0;
    let mut log = Vec::new();
    let mut step = 0;
    for epoch in 0..s.epochs {
        let frozen = epoch < s.unfreeze_epoch;
        let t0 = Instant::now();
        let mut time = Timing::default();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        let mut losses = Vec::new();
        let aug = s.augment.then_some((s.seed, epoch));
        // The epoch's batches, each as its passes.
        let batches: Vec<Vec<Vec<&Sample>>> = order
            .chunks(s.batch)
            .map(|chunk| {
                let picked: Vec<&Sample> = chunk.iter().map(|&i| train[i]).collect();
                let pass = if s.micro_batch == 0 { picked.len() } else { s.micro_batch.min(picked.len()) };
                picked.chunks(pass.max(1)).map(<[&Sample]>::to_vec).collect()
            })
            .collect();
        std::thread::scope(|scope| -> Result<()> {
            // Photos for the next batches are prepared while the device
            // works on this one.
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<Prepared>>>(2);
            let batches = &batches;
            scope.spawn(move || {
                for passes in batches {
                    let ready = passes.iter().map(|p| prepare(p, cfg, aug, &s.cache_dir)).collect();
                    if tx.send(ready).is_err() {
                        return;
                    }
                }
            });
            for passes in batches {
                let waited = Instant::now();
                let prepared = rx.recv().map_err(|_| anyhow::anyhow!("the photo loader stopped"))??;
                time.data += waited.elapsed().as_secs_f64();
                let in_batch: usize = passes.iter().map(Vec::len).sum();
                let mut grads = GradStore::default();
                let mut batch_loss = 0.0;
                for p in prepared {
                    let t = Instant::now();
                    let n = p.n;
                    let (x, y, soft) = upload(p, cfg, device)?;
                    let tokens = net.encode(&x)?;
                    let tokens = if frozen { tokens.detach() } else { tokens };
                    let logits = net.decode(&tokens)?.reshape(((), 13))?;
                    let logits32 = logits.to_dtype(DType::F32)?;
                    let loss = match (&soft, s.distill) {
                        (Some((teacher, labelled)), Some(d)) => {
                            distill_loss(&logits32, &y, teacher, labelled, d)?
                        }
                        _ => candle_nn::loss::cross_entropy(&logits32, &y)?,
                    };
                    drop((logits32, soft));
                    // Each pass's share of the batch's mean loss.
                    let loss = (loss * (n as f64 / in_batch as f64))?;
                    settle(device, &mut time.peak_bytes)?;
                    time.forward += t.elapsed().as_secs_f64();

                    let t = Instant::now();
                    let mut part = loss.backward()?;
                    batch_loss += f64::from(loss.to_scalar::<f32>()?);
                    drop((loss, logits, tokens, x, y));
                    // Only the parameters' gradients are needed; the rest
                    // (the input's, constants') go now.
                    let mut kept = GradStore::default();
                    for v in &all {
                        if let Some(g) = part.remove(v.as_tensor()) {
                            kept.insert(v.as_tensor(), g);
                        }
                    }
                    drop(part);
                    grads.extend(kept)?;
                    settle(device, &mut time.peak_bytes)?;
                    time.backward += t.elapsed().as_secs_f64();
                }
                let t = Instant::now();
                clip(&mut grads, if frozen { &dec } else { &all }, s.clip)?;
                opt_dec.set_learning_rate(s.lr_decoder * schedule(step, 0, warm, total));
                opt_dec.step(&grads)?;
                if !frozen {
                    opt_enc.set_learning_rate(s.lr_encoder * schedule(step, unfreeze, warm, total));
                    opt_enc.step(&grads)?;
                }
                drop(grads);
                settle(device, &mut time.peak_bytes)?;
                time.step += t.elapsed().as_secs_f64();
                losses.push(batch_loss);
                step += 1;
            }
            Ok(())
        })?;
        let t = Instant::now();
        let val_scores = evaluate(net, &val, cfg, s, device)?;
        time.validate = t.elapsed().as_secs_f64();
        let mean_exact = if val_scores.is_empty() {
            0.0
        } else {
            val_scores.values().map(|v| v.exact).sum::<f64>() / val_scores.len() as f64
        };
        let loss = losses.iter().sum::<f64>() / losses.len().max(1) as f64;
        let shown: Vec<String> = val_scores
            .iter()
            .map(|(k, v)| format!("{k} {:.1}% ({:.2} wrong/board)", v.exact * 100.0, v.wrong_per_board))
            .collect();
        println!(
            "epoch {epoch:3} loss {loss:.4} {}| {}",
            if frozen { "frozen " } else { "" },
            shown.join(", ")
        );
        println!("          {}", time.line());
        if mean_exact >= best {
            best = mean_exact;
            vm.save(best_path)?;
        }
        log.push(EpochLog { epoch, loss, frozen, secs: t0.elapsed().as_secs_f64(), time, val: val_scores });
    }
    vm.load(best_path)?;
    Ok(log)
}

/// A fresh network of shape `cfg` on `device`, its encoder from DINOv2 if
/// given.
pub fn build(cfg: &ModelConfig, dinov2: Option<&Path>, device: &Device) -> Result<(VarMap, Network)> {
    let vm = VarMap::new();
    let vb = VarBuilder::from_varmap(&vm, DType::F32, device);
    let net = Network::new_trainable(vb, cfg)?;
    if let Some(path) = dinov2 {
        let loaded = crate::dinov2::load(path, cfg.grid())?;
        let data = vm.data().lock().map_err(|_| anyhow::anyhow!("parameters poisoned"))?;
        let mut missing = Vec::new();
        for (name, var) in data.iter().filter(|(n, _)| n.starts_with("encoder.")) {
            let key = name.trim_start_matches("encoder.");
            match loaded.get(key) {
                Some(t) if t.dims() == var.dims() => var.set(&t.to_dtype(DType::F32)?.to_device(device)?)?,
                Some(t) => anyhow::bail!(
                    "{key}: the checkpoint has {:?}, the network {:?}: is it a DINOv2 of this size?",
                    t.dims(),
                    var.dims()
                ),
                None => missing.push(key.to_owned()),
            }
        }
        anyhow::ensure!(
            missing.is_empty(),
            "the checkpoint is missing {:?}",
            &missing[..missing.len().min(5)]
        );
    }
    Ok((vm, net))
}

/// The weights file: f16, with the config and card in its metadata.
pub fn export(vm: &VarMap, cfg: &ModelConfig, card: &ModelCard, path: &Path) -> Result<usize> {
    let tensors = weights::varmap_tensors(vm);
    let n = tensors.iter().map(|(_, t)| t.elem_count()).sum();
    let bytes = weights::to_bytes(&tensors, cfg, card, true)?;
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> ModelConfig {
        ModelConfig {
            image_size: 42,
            encoder: chess_vision::config::EncoderConfig {
                dim: 32,
                depth: 2,
                heads: 2,
                mlp_dim: 64,
                patch: 14,
                registers: 2,
            },
            decoder: chess_vision::config::DecoderConfig { dim: 16, layers: 2, heads: 2, ffn_dim: 32 },
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        }
    }

    /// Every parameter learns: each gets a gradient (none sits behind an
    /// operation without a backward pass, as candle's fused layer-norm and
    /// softmax are), and changing any one changes the output (nothing is
    /// computed from the parameters once and kept).
    #[test]
    fn every_parameter_gets_a_gradient_and_counts() {
        let cfg = tiny();
        let (vm, net) = build(&cfg, None, &Device::Cpu).unwrap();
        let x = Tensor::randn(0f32, 1.0, (2, 3, 42, 42), &Device::Cpu).unwrap();
        let y = Tensor::from_vec((0..128u32).map(|i| i % 13).collect::<Vec<_>>(), 128, &Device::Cpu).unwrap();
        let logits = || net.logits(&x).unwrap();
        let loss = candle_nn::loss::cross_entropy(&logits().reshape(((), 13)).unwrap(), &y).unwrap();
        let grads = loss.backward().unwrap();
        let data = vm.data().lock().unwrap();
        let without: Vec<&String> =
            data.iter().filter(|(_, v)| grads.get(v.as_tensor()).is_none()).map(|(k, _)| k).collect();
        assert!(without.is_empty(), "no gradient for {without:?}");

        // The first decoder layer starts from all-zero square states, so its
        // first norm's scale and its value projection multiply zero: they
        // can't count yet (DETR's design, not a fault). With every value
        // the same (the value bias), that self-attention's weights don't
        // matter either: any mix of equal values is that value. So its
        // query and key projections are inert too; rounding sometimes made
        // them look otherwise, which is why this test was flaky.
        // And an attention key bias adds the same q·b to every score of a
        // query, which softmax ignores: inert in any attention, by design.
        let inert = [
            "decoder.layers.0.norm1.weight",
            "decoder.layers.0.self_attn.v.weight",
            "decoder.layers.0.self_attn.q.weight",
            "decoder.layers.0.self_attn.q.bias",
            "decoder.layers.0.self_attn.k.weight",
        ];
        let base = logits();
        let mut ignored = Vec::new();
        for (name, var) in
            data.iter().filter(|(n, _)| !inert.contains(&n.as_str()) && !n.ends_with(".k.bias"))
        {
            let saved = var.as_tensor().copy().unwrap();
            // Random, not uniform: a uniform shift is exactly what the next
            // layer-norm removes.
            let noise = Tensor::randn(0f32, 0.5, saved.dims(), &Device::Cpu).unwrap();
            var.set(&(&saved + noise).unwrap()).unwrap();
            let moved =
                (logits() - &base).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
            var.set(&saved).unwrap();
            if moved < 1e-6 {
                ignored.push(name.clone());
            }
        }
        assert!(ignored.is_empty(), "changing these didn't change the output: {ignored:?}");
    }

    /// The training network and the reading network are the same function.
    #[test]
    fn training_and_reading_networks_agree() {
        let cfg = tiny();
        let bytes = chess_vision::weights::random_model(&cfg, &ModelCard::default()).unwrap();
        let x = Tensor::randn(0f32, 1.0, (2, 3, 42, 42), &Device::Cpu).unwrap();
        let read =
            Network::new(VarBuilder::from_slice_safetensors(&bytes, DType::F32, &Device::Cpu).unwrap(), &cfg)
                .unwrap();
        let train = Network::new_trainable(
            VarBuilder::from_slice_safetensors(&bytes, DType::F32, &Device::Cpu).unwrap(),
            &cfg,
        )
        .unwrap();
        let diff = (read.logits(&x).unwrap() - train.logits(&x).unwrap())
            .unwrap()
            .abs()
            .unwrap()
            .max_all()
            .unwrap()
            .to_scalar::<f32>()
            .unwrap();
        assert!(diff < 1e-4, "{diff}");
    }

    fn t2(v: &[f32], rows: usize) -> Tensor {
        Tensor::from_vec(v.to_vec(), (rows, v.len() / rows), &Device::Cpu).unwrap()
    }

    fn scalar(t: &Tensor) -> f32 {
        t.to_scalar::<f32>().unwrap()
    }

    #[test]
    fn the_distillation_loss() {
        let logits = t2(&[2.0, 0.5, -1.0, 0.0, 1.0, 3.0], 2);
        let y = Tensor::from_vec(vec![0u32, 2], 2, &Device::Cpu).unwrap();
        let all = Tensor::from_vec(vec![1.0f32, 1.0], 2, &Device::Cpu).unwrap();
        let ce = scalar(&candle_nn::loss::cross_entropy(&logits, &y).unwrap());
        let teacher =
            candle_nn::ops::log_softmax(&t2(&[-1.0, 2.0, 0.0, 1.0, 1.0, 1.0], 2), D::Minus1).unwrap();
        let loss = |w: f64, t: f64, labelled: &Tensor| {
            scalar(
                &distill_loss(&logits, &y, &teacher, labelled, Distill { weight: w, temperature: t })
                    .unwrap(),
            )
        };
        // Weight 0: the labels alone, exactly cross-entropy.
        assert!((loss(0.0, 2.0, &all) - ce).abs() < 1e-5);
        // A teacher reading what the student reads adds nothing.
        let same = candle_nn::ops::log_softmax(&logits, D::Minus1).unwrap();
        let l = scalar(
            &distill_loss(&logits, &y, &same, &all, Distill { weight: 0.5, temperature: 3.0 }).unwrap(),
        );
        assert!((l - 0.5 * ce).abs() < 1e-5);
        // A disagreeing teacher adds a positive amount, and an unlabelled
        // row learns from the teacher alone.
        assert!(loss(0.5, 2.0, &all) > 0.5 * ce);
        let none = Tensor::from_vec(vec![0.0f32, 0.0], 2, &Device::Cpu).unwrap();
        let kd_only = loss(1.0, 2.0, &none);
        assert!(kd_only > 0.0 && (loss(1.0, 2.0, &all) - kd_only).abs() < 1e-6);
        // And it trains: gradients reach the logits.
        let v = candle_core::Var::from_tensor(&logits).unwrap();
        let g = distill_loss(v.as_tensor(), &y, &teacher, &none, Distill { weight: 0.5, temperature: 2.0 })
            .unwrap()
            .backward()
            .unwrap();
        assert!(scalar(&g.get(v.as_tensor()).unwrap().abs().unwrap().sum_all().unwrap()) > 0.0);
    }

    #[test]
    fn the_schedule() {
        assert!(schedule(0, 5, 2, 20).abs() < 1e-12);
        assert!((schedule(5, 5, 2, 20) - 0.5).abs() < 1e-12);
        assert!((schedule(6, 5, 2, 20) - 1.0).abs() < 1e-12);
        assert!(schedule(19, 0, 2, 20) < 0.05);
        assert!(schedule(30, 0, 2, 20).abs() < 1e-12);
    }
}
