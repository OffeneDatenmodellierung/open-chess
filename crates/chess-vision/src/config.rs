//! The shape of a board-reading network. It travels inside the weights
//! file (safetensors metadata, key [`METADATA_KEY`]) so a file always
//! carries what is needed to run it.

use serde::{Deserialize, Serialize};

/// The safetensors metadata key holding the [`ModelConfig`] as JSON.
pub const METADATA_KEY: &str = "chess_vision.config";

/// The image encoder: a DINOv2-style ViT with 14-pixel patches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderConfig {
    /// Width of each token (384 small, 768 base, 1024 large).
    pub dim: usize,
    /// Transformer blocks (12, 12, 24).
    pub depth: usize,
    /// Attention heads (6, 12, 16).
    pub heads: usize,
    /// Hidden width of each block's MLP (4 × dim for DINOv2).
    pub mlp_dim: usize,
    pub patch: usize,
    /// Register tokens (0 for plain DINOv2, 4 for DINOv2 "with registers").
    #[serde(default)]
    pub registers: usize,
}

/// The square decoder: 64 learned queries, one per square, attending to
/// the encoder's patch tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoderConfig {
    pub dim: usize,
    pub layers: usize,
    pub heads: usize,
    pub ffn_dim: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// The square side, in pixels, photos are scaled to (a multiple of
    /// the patch size).
    pub image_size: usize,
    pub encoder: EncoderConfig,
    pub decoder: DecoderConfig,
    /// Per-channel RGB mean and standard deviation the pixels are
    /// normalised with (ImageNet's, as DINOv2 was trained with).
    #[serde(default = "imagenet_mean")]
    pub mean: [f32; 3],
    #[serde(default = "imagenet_std")]
    pub std: [f32; 3],
}

fn imagenet_mean() -> [f32; 3] {
    [0.485, 0.456, 0.406]
}

fn imagenet_std() -> [f32; 3] {
    [0.229, 0.224, 0.225]
}

impl EncoderConfig {
    pub fn vit_small() -> Self {
        Self { dim: 384, depth: 12, heads: 6, mlp_dim: 1536, patch: 14, registers: 0 }
    }

    pub fn vit_base() -> Self {
        Self { dim: 768, depth: 12, heads: 12, mlp_dim: 3072, patch: 14, registers: 0 }
    }

    pub fn vit_large() -> Self {
        Self { dim: 1024, depth: 24, heads: 16, mlp_dim: 4096, patch: 14, registers: 0 }
    }
}

impl DecoderConfig {
    /// Four layers, as in the published method.
    pub fn standard() -> Self {
        Self { dim: 256, layers: 4, heads: 8, ffn_dim: 1024 }
    }
}

impl ModelConfig {
    /// Patches along each side of the image.
    pub fn grid(&self) -> usize {
        self.image_size / self.encoder.patch
    }

    /// Whether the numbers fit together.
    pub fn validate(&self) -> Result<(), String> {
        let e = &self.encoder;
        let d = &self.decoder;
        if e.patch == 0 || self.image_size == 0 || !self.image_size.is_multiple_of(e.patch) {
            return Err(format!(
                "image_size {} isn't a multiple of the patch size {}",
                self.image_size, e.patch
            ));
        }
        if e.heads == 0 || !e.dim.is_multiple_of(e.heads) {
            return Err(format!("encoder width {} doesn't split into {} heads", e.dim, e.heads));
        }
        if d.heads == 0 || !d.dim.is_multiple_of(d.heads) {
            return Err(format!("decoder width {} doesn't split into {} heads", d.dim, d.heads));
        }
        if !d.dim.is_multiple_of(4) {
            return Err(format!("decoder width {} must be a multiple of 4 for the position encoding", d.dim));
        }
        if self.std.iter().any(|&s| s <= 0.0) {
            return Err("std must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_defaults() {
        let c = ModelConfig {
            image_size: 518,
            encoder: EncoderConfig::vit_small(),
            decoder: DecoderConfig::standard(),
            mean: imagenet_mean(),
            std: imagenet_std(),
        };
        c.validate().unwrap();
        assert_eq!(c.grid(), 37);
        let back: ModelConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let minimal = r#"{"image_size":644,"encoder":{"dim":1024,"depth":24,"heads":16,"mlp_dim":4096,"patch":14},
            "decoder":{"dim":256,"layers":4,"heads":8,"ffn_dim":1024}}"#;
        let m: ModelConfig = serde_json::from_str(minimal).unwrap();
        assert_eq!((m.grid(), m.encoder.registers, m.mean), (46, 0, imagenet_mean()));
        let bad = ModelConfig { image_size: 500, ..m };
        assert!(bad.validate().unwrap_err().contains("multiple"));
    }
}
