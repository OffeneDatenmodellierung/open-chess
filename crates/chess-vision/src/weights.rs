//! Writing weights files: every parameter as safetensors, with the
//! [`ModelConfig`] and the [`ModelCard`] in the file's metadata so they
//! travel with the weights. The trainer writes its results this way, and
//! [`random_model`] makes untrained networks for tests.

use std::collections::HashMap;

use candle_core::{DType, Device, Tensor};
use candle_nn::{VarBuilder, VarMap};
use safetensors::Dtype;
use safetensors::tensor::TensorView;

use crate::config::{METADATA_KEY, ModelConfig};
use crate::{CARD_KEY, Error, ModelCard, network};

/// Serialise named tensors, as f16 when `half` (half the size; the reader
/// widens them again), else f32.
pub fn to_bytes(
    tensors: &[(String, Tensor)],
    cfg: &ModelConfig,
    card: &ModelCard,
    half: bool,
) -> Result<Vec<u8>, Error> {
    let mut data: Vec<(String, Vec<usize>, Vec<u8>)> = Vec::with_capacity(tensors.len());
    for (name, t) in tensors {
        let shape = t.dims().to_vec();
        let flat = t.flatten_all()?;
        let bytes = if half {
            flat.to_dtype(DType::F16)?.to_vec1::<half::f16>()?.iter().flat_map(|v| v.to_le_bytes()).collect()
        } else {
            flat.to_dtype(DType::F32)?.to_vec1::<f32>()?.iter().flat_map(|v| v.to_le_bytes()).collect()
        };
        data.push((name.clone(), shape, bytes));
    }
    let dtype = if half { Dtype::F16 } else { Dtype::F32 };
    let views = data
        .iter()
        .map(|(n, shape, bytes)| {
            TensorView::new(dtype, shape.clone(), bytes)
                .map(|v| (n.clone(), v))
                .map_err(|e| Error::Weights(e.to_string()))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let meta = HashMap::from([
        (METADATA_KEY.to_owned(), serde_json::to_string(cfg).map_err(|e| Error::Weights(e.to_string()))?),
        (CARD_KEY.to_owned(), serde_json::to_string(card).map_err(|e| Error::Weights(e.to_string()))?),
    ]);
    safetensors::serialize(views, Some(meta)).map_err(|e| Error::Weights(e.to_string()))
}

/// Every variable of a [`VarMap`], sorted by name.
pub fn varmap_tensors(vm: &VarMap) -> Vec<(String, Tensor)> {
    let data = vm.data().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut out: Vec<(String, Tensor)> =
        data.iter().map(|(k, v)| (k.clone(), v.as_tensor().clone())).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A network of shape `cfg` with fresh random parameters, as a weights
/// file: what training starts from, and an untrained model for tests.
pub fn random_model(cfg: &ModelConfig, card: &ModelCard) -> Result<Vec<u8>, Error> {
    cfg.validate().map_err(Error::Weights)?;
    let vm = VarMap::new();
    let vb = VarBuilder::from_varmap(&vm, DType::F32, &Device::Cpu);
    network::Network::new(vb, cfg)?;
    to_bytes(&varmap_tensors(&vm), cfg, card, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DecoderConfig, EncoderConfig};
    use crate::{Image, Reader};

    pub(crate) fn tiny() -> ModelConfig {
        ModelConfig {
            image_size: 42,
            encoder: EncoderConfig { dim: 32, depth: 2, heads: 2, mlp_dim: 64, patch: 14, registers: 2 },
            decoder: DecoderConfig { dim: 16, layers: 2, heads: 2, ffn_dim: 32 },
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        }
    }

    #[test]
    fn a_random_model_loads_and_reads() {
        let card = ModelCard { name: "random".into(), ..ModelCard::default() };
        let bytes = random_model(&tiny(), &card).unwrap();
        let r = Reader::from_safetensors(&bytes).unwrap();
        assert_eq!(r.config(), &tiny());
        assert_eq!(r.card().name, "random");
        let px = vec![90u8; 30 * 20 * 3];
        let reading = r.read(&Image { width: 30, height: 20, channels: 3, pixels: &px }).unwrap();
        assert_eq!(reading.squares.len(), 64);
        assert!(reading.squares.iter().all(|p| (p.iter().sum::<f32>() - 1.0).abs() < 1e-4));
        // The squares don't all read the same: the queries start different.
        assert!(
            reading.squares.windows(2).any(|w| w[0].iter().zip(&w[1]).any(|(a, b)| (a - b).abs() > 1e-6))
        );
    }

    #[test]
    fn half_precision_agrees_with_full() {
        let bytes = random_model(&tiny(), &ModelCard::default()).unwrap();
        let full = Reader::from_safetensors(&bytes).unwrap();
        let half = Reader::from_safetensors_with(&bytes, &Device::Cpu, DType::F16).unwrap();
        let n = 3 * 42 * 42;
        let input: Vec<f32> = (0..2 * n).map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0).collect();
        let a = full.logits_prepared(input.clone(), 2).unwrap();
        let b = half.logits_prepared(input, 2).unwrap();
        let worst = a
            .iter()
            .flatten()
            .flatten()
            .zip(b.iter().flatten().flatten())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.05, "f16 logits differ by up to {worst}");
        // And a file written in f16 loads.
        let tensors: Vec<(String, Tensor)> =
            candle_core::safetensors::load_buffer(&bytes, &Device::Cpu).unwrap().into_iter().collect();
        let f16_file = to_bytes(&tensors, &tiny(), &ModelCard::default(), true).unwrap();
        assert!(f16_file.len() < bytes.len() * 6 / 10);
        Reader::from_safetensors(&f16_file).unwrap();
    }
}
