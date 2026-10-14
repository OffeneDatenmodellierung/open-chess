//! Distillation's teacher: a board reader trained earlier (typically a
//! large one for the server, `--encoder base` at 518) whose readings the
//! model being trained learns to match, square by square and class by
//! class, as well as the labels.
//!
//! The teacher reads each training photo **once**, before training, as it
//! is (unaugmented). Augmentation never changes which piece stands on which
//! square (nothing is cropped or mirrored, see `augment.rs`), so the
//! teacher's reading of the plain photo is the right target for every
//! turned, tilted or recoloured version of it, every epoch. That keeps the
//! cost to one pass of the big network, not one per step.
//!
//! Its readings are cached under `--out` (`teacher-cache/`), keyed by the
//! teacher file's hash and the photo, so a second run with the same teacher
//! starts training at once.
//!
//! Photos without labels (format `photos`) learn only from the teacher, so
//! only those it reads confidently are kept: a plausible position and a
//! confidence (every square's top probability, multiplied) of at least
//! `--teacher-min-confidence`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chess_vision::{Image, ModelConfig, Reader, preprocess};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::data::Sample;

/// Values per reading: 64 squares × 13 classes.
pub const VALUES: usize = 64 * 13;

/// A loaded teacher.
pub struct Teacher {
    reader: Reader,
    /// The weights file's hash (what the cache is keyed by).
    key: String,
    pub path: PathBuf,
}

/// What the teacher read from one photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// Log probabilities, 64 squares × 13 classes, a1 first.
    pub log_probs: Arc<[f32]>,
    pub confidence: f32,
    pub plausible: bool,
}

impl Teacher {
    pub fn load(path: &Path, device: &candle_core::Device) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading the teacher {}", path.display()))?;
        let key = hex(&Sha256::digest(&bytes)[..12]);
        let reader = Reader::from_safetensors_on(&bytes, device)
            .with_context(|| format!("loading the teacher {}", path.display()))?;
        Ok(Self { reader, key, path: path.to_path_buf() })
    }

    pub fn config(&self) -> &ModelConfig {
        self.reader.config()
    }

    /// Read every sample's photo, from the cache where it can. `photos` is
    /// the trainer's decoded-photo cache, `cache` the teacher's.
    pub fn read_all(
        &self,
        samples: &[&Sample],
        photos: &Path,
        cache: &Path,
        batch: usize,
    ) -> Result<Vec<Reading>> {
        std::fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
        let file_of = |s: &Sample| {
            let k = Sha256::digest(format!("{}:{}", self.key, s.image.display()).as_bytes());
            cache.join(format!("{}.bin", hex(&k[..12])))
        };
        let mut out: Vec<Option<Reading>> = samples.iter().map(|s| from_cache(&file_of(s))).collect();
        let todo: Vec<usize> = (0..samples.len()).filter(|&i| out[i].is_none()).collect();
        if !todo.is_empty() {
            println!(
                "teacher: reading {} photos ({} cached) with {}",
                todo.len(),
                samples.len() - todo.len(),
                self.path.display()
            );
        }
        let size = self.config().image_size as u32;
        let started = std::time::Instant::now();
        let mut done = 0;
        for chunk in todo.chunks(batch.max(1)) {
            let inputs: Vec<Vec<f32>> = chunk
                .par_iter()
                .map(|&i| {
                    let img = crate::train::load_photo(&samples[i].image, size, photos)?;
                    let px = Image {
                        width: img.width() as usize,
                        height: img.height() as usize,
                        channels: 3,
                        pixels: img.as_raw(),
                    };
                    Ok(preprocess::prepare(&px, self.config())?)
                })
                .collect::<Result<_>>()?;
            let readings = self.reader.read_prepared(inputs.concat())?;
            for (&i, r) in chunk.iter().zip(readings) {
                let log_probs: Arc<[f32]> =
                    r.squares.iter().flatten().map(|p| p.max(1e-9).ln()).collect::<Vec<_>>().into();
                let reading = Reading { log_probs, confidence: r.confidence, plausible: r.plausible };
                to_cache(&file_of(samples[i]), &reading)?;
                out[i] = Some(reading);
            }
            done += chunk.len();
            if done % (batch.max(1) * 25) < chunk.len() || done == todo.len() {
                println!("teacher: {done}/{} ({:.0}s)", todo.len(), started.elapsed().as_secs_f64());
            }
        }
        Ok(out.into_iter().map(|r| r.expect("every photo read")).collect())
    }
}

/// A cached reading: 832 little-endian f32 log probabilities, the
/// confidence (f32) and whether it's plausible (one byte).
fn to_cache(path: &Path, r: &Reading) -> Result<()> {
    let mut b = Vec::with_capacity(VALUES * 4 + 5);
    for v in r.log_probs.iter().chain(std::iter::once(&r.confidence)) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.push(u8::from(r.plausible));
    std::fs::write(path, b).with_context(|| format!("writing {}", path.display()))
}

fn from_cache(path: &Path) -> Option<Reading> {
    let b = std::fs::read(path).ok()?;
    if b.len() != VALUES * 4 + 5 {
        return None;
    }
    let f = |i: usize| f32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    Some(Reading {
        log_probs: (0..VALUES).map(f).collect::<Vec<_>>().into(),
        confidence: f(VALUES),
        plausible: b[VALUES * 4 + 4] != 0,
    })
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Split;
    use chess_vision::ModelCard;
    use chess_vision::config::{DecoderConfig, EncoderConfig};

    #[test]
    fn reads_once_then_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("cvt-teacher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ModelConfig {
            image_size: 42,
            encoder: EncoderConfig { dim: 32, depth: 2, heads: 2, mlp_dim: 64, patch: 14, registers: 0 },
            decoder: DecoderConfig { dim: 16, layers: 2, heads: 2, ffn_dim: 32 },
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        };
        let card = ModelCard { name: "teacher".into(), internal_only: true, ..ModelCard::default() };
        let weights = dir.join("teacher.safetensors");
        std::fs::write(&weights, chess_vision::weights::random_model(&cfg, &card).unwrap()).unwrap();
        let mut samples = Vec::new();
        for (i, colour) in [[200u8, 30, 30], [20, 160, 40]].into_iter().enumerate() {
            let p = dir.join(format!("{i}.png"));
            image::RgbImage::from_pixel(60, 45, image::Rgb(colour)).save(&p).unwrap();
            samples.push(Sample {
                image: p,
                labels: [0; 64],
                labelled: false,
                dataset: "loose".into(),
                group: format!("loose:{i}"),
                split: Split::Train,
                soft: None,
            });
        }
        let refs: Vec<&Sample> = samples.iter().collect();
        let t = Teacher::load(&weights, &candle_core::Device::Cpu).unwrap();
        let (photos, cache) = (dir.join("photos"), dir.join("teacher-cache"));
        std::fs::create_dir_all(&photos).unwrap();
        let first = t.read_all(&refs, &photos, &cache, 1).unwrap();
        assert_eq!(first.len(), 2);
        for r in &first {
            assert_eq!(r.log_probs.len(), VALUES);
            // Each square's probabilities add up to one.
            for sq in r.log_probs.chunks(13) {
                assert!((sq.iter().map(|v| v.exp()).sum::<f32>() - 1.0).abs() < 1e-3);
            }
            assert!(r.confidence > 0.0 && r.confidence <= 1.0);
        }
        assert_ne!(first[0].log_probs, first[1].log_probs);
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 2);
        // The second time, from the cache, the same.
        assert_eq!(t.read_all(&refs, &photos, &cache, 2).unwrap(), first);
        std::fs::remove_dir_all(dir).ok();
    }
}
