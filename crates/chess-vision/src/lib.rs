//! Reading a chess position from a photo of a real board.
//!
//! A [`Reader`] loads a trained network from a safetensors file and turns
//! a photo into a [`BoardReading`]: for each of the 64 squares, the
//! probability of each piece (or empty). The probabilities are the point:
//! when the game being followed is known,
//! [`chess_analysis::photo::match_after_move_probs`] weighs every legal
//! move against them, so an uncertain square or two still finds the move;
//! when the reading is unsure, the server keeps the photo for review.
//!
//! The method follows Seytre, "ChessQueries: Toward Better Chess Board
//! Recognition" (arXiv:2608.30762): a DINOv2 vision transformer, 64 learned
//! square queries and a DETR-style decoder. This is an independent
//! implementation written from the paper. It contains none of the
//! authors' code and loads none of their weights or data, which are
//! licensed for noncommercial use only. Weights come from
//! `crates/chess-vision-train` (`chess-vision-train`), which starts from
//! Meta's DINOv2 (Apache-2.0) and trains only on datasets whose licence
//! allows commercial use (its `datasets.toml`): ChessReD and our own
//! confirmed photos.
//!
//! Runs with candle: on the server's CPU, or its GPU with the `cuda` or
//! `metal` feature (see [`device`]); and in the browser as wasm (feature
//! `wasm`, CPU) for the offline app.

pub mod config;
pub mod network;
pub mod preprocess;
mod softmax;
pub mod tracker;
#[cfg(feature = "wasm")]
pub mod wasm;
pub mod weights;

use std::collections::HashMap;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use chess_analysis::photo::{CLASSES, Grid, SquareProbs, argmax_grid, placement};
use cozy_chess::{Color, Piece, Square};
use serde::{Deserialize, Serialize};

pub use config::ModelConfig;
pub use preprocess::Image;

/// The safetensors metadata key holding the [`ModelCard`] as JSON.
pub const CARD_KEY: &str = "chess_vision.card";

/// Below this probability a square counts as unsure.
pub const UNSURE: f32 = 0.8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the weights file: {0}")]
    Weights(String),
    #[error("the photo: {0}")]
    Image(String),
    #[error("the device: {0}")]
    Device(String),
    #[error("the network: {0}")]
    Network(#[from] candle_core::Error),
}

/// Where a weights file came from: kept in its metadata, shown to whoever
/// runs it, so the licence and attribution travel with the weights.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCard {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// The weights' own licence (SPDX).
    #[serde(default)]
    pub licence: String,
    /// The starting point, e.g. "DINOv2 ViT-S/14 (Apache-2.0, Meta)".
    #[serde(default)]
    pub base: String,
    /// Every dataset trained on, with its licence and the attribution it
    /// requires.
    #[serde(default)]
    pub datasets: Vec<DatasetCredit>,
    /// Exact-board accuracy on each held-out test set.
    #[serde(default)]
    pub accuracy: HashMap<String, f32>,
    /// Trained on data licensed for noncommercial use: for our own staff
    /// only, never distributed (the Chess lab app build refuses it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub internal_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DatasetCredit {
    pub name: String,
    pub licence: String,
    #[serde(default)]
    pub attribution: String,
}

/// What the network read from one photo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardReading {
    /// The most likely piece placement (FEN's first field, from White's
    /// side whichever way the photo was taken).
    pub placement: String,
    /// Each square's class probabilities (a1 = 0; classes in the order of
    /// [`CLASSES`]).
    pub squares: Vec<[f32; 13]>,
    /// The chance every square is right: the product of each square's
    /// top probability.
    pub confidence: f32,
    /// Squares whose top probability is under [`UNSURE`], like `"e4"`.
    pub unsure_squares: Vec<String>,
    /// Whether the placement could be a game position (one king each, no
    /// pawns on the end ranks, at most 16 pieces a side). The reading is
    /// still returned when it isn't.
    pub plausible: bool,
}

impl BoardReading {
    fn from_probs(probs: &SquareProbs) -> Self {
        let grid = argmax_grid(probs);
        let mut log_conf = 0.0f64;
        let mut unsure = Vec::new();
        for (sq, p) in probs.iter().enumerate() {
            let top = p.iter().copied().fold(0.0f32, f32::max);
            log_conf += f64::from(top.max(1e-9)).ln();
            if top < UNSURE {
                unsure.push(Square::index(sq).to_string());
            }
        }
        Self {
            placement: placement(&grid),
            squares: probs.to_vec(),
            confidence: log_conf.exp() as f32,
            unsure_squares: unsure,
            plausible: plausible(&grid),
        }
    }

    /// The probabilities as a fixed array (`None` unless there are 64).
    pub fn probs(&self) -> Option<SquareProbs> {
        self.squares.as_slice().try_into().ok()
    }

    /// Whether this reading can be used without anyone checking it.
    pub fn is_sure(&self, min_confidence: f32) -> bool {
        self.plausible && self.unsure_squares.is_empty() && self.confidence >= min_confidence
    }
}

/// One king each, no pawns on the first or last rank, at most 16 pieces
/// and 8 pawns a side.
pub fn plausible(grid: &Grid) -> bool {
    let count = |want: Option<(Piece, Color)>| grid.iter().filter(|&&c| c == want).count();
    let side = |c: Color| grid.iter().filter(|cell| cell.is_some_and(|(_, col)| col == c)).count();
    let back_rank_pawn = (0..8).chain(56..64).any(|sq| grid[sq].is_some_and(|(p, _)| p == Piece::Pawn));
    count(Some((Piece::King, Color::White))) == 1
        && count(Some((Piece::King, Color::Black))) == 1
        && !back_rank_pawn
        && side(Color::White) <= 16
        && side(Color::Black) <= 16
        && count(Some((Piece::Pawn, Color::White))) <= 8
        && count(Some((Piece::Pawn, Color::Black))) <= 8
}

/// A loaded network.
pub struct Reader {
    net: network::Network,
    cfg: ModelConfig,
    card: ModelCard,
    device: Device,
    dtype: DType,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader").field("cfg", &self.cfg).field("card", &self.card).finish_non_exhaustive()
    }
}

/// The config and card stored in a weights file's metadata.
pub fn read_metadata(bytes: &[u8]) -> Result<(ModelConfig, ModelCard), Error> {
    let (_, meta) =
        safetensors::SafeTensors::read_metadata(bytes).map_err(|e| Error::Weights(e.to_string()))?;
    let m = meta.metadata().clone().unwrap_or_default();
    let cfg_json = m.get(config::METADATA_KEY).ok_or_else(|| {
        Error::Weights(format!("no `{}` in its metadata: not a chess-vision model", config::METADATA_KEY))
    })?;
    let cfg: ModelConfig =
        serde_json::from_str(cfg_json).map_err(|e| Error::Weights(format!("its config: {e}")))?;
    cfg.validate().map_err(Error::Weights)?;
    let card = match m.get(CARD_KEY) {
        Some(j) => serde_json::from_str(j).map_err(|e| Error::Weights(format!("its model card: {e}")))?,
        None => ModelCard::default(),
    };
    Ok((cfg, card))
}

/// The device to run on: `cpu`, `cuda` / `cuda:N`, `metal` / `metal:N`, or
/// `auto` (the first GPU this build supports and finds, else the CPU).
pub fn device(want: &str) -> Result<Device, Error> {
    let want = want.trim().to_ascii_lowercase();
    let (kind, index) = match want.split_once(':') {
        Some((k, i)) => {
            (k, i.parse::<usize>().map_err(|_| Error::Device(format!("{want:?}: bad device number")))?)
        }
        None => (want.as_str(), 0),
    };
    let gpu = |e: candle_core::Error| Error::Device(format!("{want}: {e}"));
    match kind {
        "cpu" | "" => Ok(Device::Cpu),
        "cuda" if candle_core::utils::cuda_is_available() => Device::new_cuda(index).map_err(gpu),
        "cuda" => Err(Error::Device("this build has no CUDA support (build with the `cuda` feature)".into())),
        "metal" if candle_core::utils::metal_is_available() => Device::new_metal(index).map_err(gpu),
        "metal" => {
            Err(Error::Device("this build has no Metal support (build with the `metal` feature)".into()))
        }
        "auto" => {
            if candle_core::utils::cuda_is_available()
                && let Ok(d) = Device::new_cuda(0)
            {
                return Ok(d);
            }
            if candle_core::utils::metal_is_available()
                && let Ok(d) = Device::new_metal(0)
            {
                return Ok(d);
            }
            Ok(Device::Cpu)
        }
        other => Err(Error::Device(format!("{other:?}: expected cpu, cuda, metal or auto"))),
    }
}

/// A short name for a device, for logs.
pub fn device_name(d: &Device) -> &'static str {
    match d {
        Device::Cpu => "cpu",
        Device::Cuda(_) => "cuda",
        Device::Metal(_) => "metal",
    }
}

impl Reader {
    /// Load a weights file (safetensors with the config in its metadata)
    /// to run on the CPU, in f32.
    pub fn from_safetensors(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_safetensors_on(bytes, &Device::Cpu)
    }

    /// Load a weights file to run on `device`: f32 on the CPU, f16 on a GPU
    /// (half the memory, and what GPUs are fast at).
    pub fn from_safetensors_on(bytes: &[u8], device: &Device) -> Result<Self, Error> {
        let dtype = if device.is_cpu() { DType::F32 } else { DType::F16 };
        Self::from_safetensors_with(bytes, device, dtype)
    }

    /// Load a weights file to run on `device` in `dtype` (F32, F16 or BF16).
    pub fn from_safetensors_with(bytes: &[u8], device: &Device, dtype: DType) -> Result<Self, Error> {
        let (cfg, card) = read_metadata(bytes)?;
        let vb = VarBuilder::from_slice_safetensors(bytes, dtype, device)?;
        let net = network::Network::new(vb, &cfg).map_err(|e| Error::Weights(format!("{e}")))?;
        Ok(Self { net, cfg, card, device: device.clone(), dtype })
    }

    /// Where it runs.
    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn config(&self) -> &ModelConfig {
        &self.cfg
    }

    pub fn card(&self) -> &ModelCard {
        &self.card
    }

    /// Read a board from a photo.
    pub fn read(&self, img: &Image) -> Result<BoardReading, Error> {
        let input = preprocess::prepare(img, &self.cfg)?;
        Ok(self.read_prepared(input)?.remove(0))
    }

    /// Read already prepared inputs (each `3 × size × size`, see
    /// [`preprocess::prepare`]), concatenated.
    pub fn read_prepared(&self, input: Vec<f32>) -> Result<Vec<BoardReading>, Error> {
        let s = self.cfg.image_size;
        let per = 3 * s * s;
        if input.is_empty() || !input.len().is_multiple_of(per) {
            return Err(Error::Image(format!(
                "{} values isn't a whole number of {s}×{s} images",
                input.len()
            )));
        }
        let n = input.len() / per;
        let x = Tensor::from_vec(input, (n, 3, s, s), &Device::Cpu)?
            .to_device(&self.device)?
            .to_dtype(self.dtype)?;
        let p = self.net.probabilities(&x)?.to_vec3::<f32>()?;
        Ok(p.iter()
            .map(|b| {
                let mut probs: SquareProbs = [[0.0; 13]; 64];
                for (sq, row) in b.iter().enumerate() {
                    probs[sq].copy_from_slice(row);
                }
                BoardReading::from_probs(&probs)
            })
            .collect())
    }

    /// The raw logits for prepared inputs, `(batch, 64, 13)` (for checks).
    pub fn logits_prepared(&self, input: Vec<f32>, batch: usize) -> Result<Vec<Vec<Vec<f32>>>, Error> {
        let s = self.cfg.image_size;
        let x = Tensor::from_vec(input, (batch, 3, s, s), &Device::Cpu)?
            .to_device(&self.device)?
            .to_dtype(self.dtype)?;
        Ok(self.net.logits(&x)?.to_dtype(DType::F32)?.to_vec3::<f32>()?)
    }

    /// Decode a JPEG, PNG or WebP (turned upright by its EXIF orientation)
    /// and read it.
    #[cfg(feature = "decode")]
    pub fn read_encoded(&self, bytes: &[u8]) -> Result<BoardReading, Error> {
        let rgb = decode(bytes)?;
        self.read(&Image {
            width: rgb.width() as usize,
            height: rgb.height() as usize,
            channels: 3,
            pixels: rgb.as_raw(),
        })
    }
}

/// Decode a photo to RGB, upright.
#[cfg(feature = "decode")]
pub fn decode(bytes: &[u8]) -> Result<image::RgbImage, Error> {
    use image::ImageDecoder;
    let bad = |e: image::ImageError| Error::Image(e.to_string());
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Error::Image(e.to_string()))?;
    let mut decoder = reader.into_decoder().map_err(bad)?;
    let (w, h) = decoder.dimensions();
    if (w as usize).saturating_mul(h as usize) > preprocess::MAX_PIXELS {
        return Err(Error::Image(format!("{w}×{h} is too large")));
    }
    let orientation = decoder.orientation().map_err(bad)?;
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(bad)?;
    img.apply_orientation(orientation);
    Ok(img.to_rgb8())
}

/// The class letters, for display.
pub fn class_letters() -> [char; 13] {
    CLASSES
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_analysis::photo::{class_of, parse_placement};

    #[test]
    fn plausibility() {
        assert!(plausible(&parse_placement("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR").unwrap()));
        assert!(!plausible(&parse_placement("rnbq1bnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR").unwrap()));
        assert!(!plausible(&parse_placement("4k3/8/8/8/8/8/8/P3K3").unwrap()));
        assert!(!plausible(&parse_placement("4k3/pppppppp/p7/8/8/8/8/4K3").unwrap()));
    }

    #[test]
    fn reading_from_probabilities() {
        let g = parse_placement("4k3/8/8/8/8/8/8/4K2R").unwrap();
        let mut p: SquareProbs = [[0.0; 13]; 64];
        for (sq, cell) in g.iter().enumerate() {
            p[sq][class_of(*cell)] = 0.99;
            p[sq][0] += 0.01;
        }
        p[Square::H1 as usize] = [0.3, 0.0, 0.0, 0.0, 0.7, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let r = BoardReading::from_probs(&p);
        assert_eq!(r.placement, "4k3/8/8/8/8/8/8/4K2R");
        assert_eq!(r.unsure_squares, vec!["h1"]);
        assert!(r.plausible && !r.is_sure(0.1));
        // Empty squares are certain; the two kings 0.99; h1 0.7.
        assert!((r.confidence - 0.99f32.powi(2) * 0.7).abs() < 1e-4, "{}", r.confidence);
        assert_eq!(r.probs().unwrap(), p);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("unsureSquares").is_some());
    }

    #[test]
    fn devices_are_chosen_by_name() {
        assert!(device("cpu").unwrap().is_cpu());
        assert!(device("auto").is_ok());
        assert!(device("tpu").is_err());
        assert!(device("cuda:x").is_err());
        #[cfg(not(feature = "cuda"))]
        assert!(device("cuda").unwrap_err().to_string().contains("feature"));
        #[cfg(not(feature = "metal"))]
        assert!(device("metal").unwrap_err().to_string().contains("feature"));
    }

    #[test]
    fn refuses_files_that_are_not_models() {
        assert!(matches!(Reader::from_safetensors(b"nonsense"), Err(Error::Weights(_))));
    }
}
