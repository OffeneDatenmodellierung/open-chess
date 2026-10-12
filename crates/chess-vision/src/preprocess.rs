//! Turning a photo into the network's input: pad it to a square (centred,
//! with the mean colour, which normalises to zero), scale it to the
//! model's size, and normalise each channel.
//!
//! The scaling is a bilinear filter widened when shrinking, so every pixel
//! counts (fixed-point weights, rounded between the horizontal and vertical
//! passes). Training prepares its photos with this same code, so the
//! network sees the same pixels in training and in use.

use crate::Error;
use crate::config::ModelConfig;

/// Pixels of a photo, row-major from the top-left, 3 (RGB) or 4 (RGBA,
/// alpha ignored) bytes each.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub pixels: &'a [u8],
}

/// Photos larger than this many pixels are refused (a 40-megapixel photo
/// is already more than any phone sends; the app scales to 1280 first).
pub const MAX_PIXELS: usize = 40_000_000;

impl Image<'_> {
    fn check(&self) -> Result<(), Error> {
        if !(self.channels == 3 || self.channels == 4) {
            return Err(Error::Image(format!("{} bytes per pixel: expected RGB or RGBA", self.channels)));
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::Image("the image is empty".into()));
        }
        if self.width.saturating_mul(self.height) > MAX_PIXELS {
            return Err(Error::Image(format!("{}×{} is too large", self.width, self.height)));
        }
        if self.pixels.len() != self.width * self.height * self.channels {
            return Err(Error::Image(format!(
                "{} bytes for a {}×{} image with {} channels",
                self.pixels.len(),
                self.width,
                self.height,
                self.channels
            )));
        }
        Ok(())
    }
}

/// The colour photos are padded with: the normalisation mean, in bytes.
pub fn pad_colour(cfg: &ModelConfig) -> [u8; 3] {
    cfg.mean.map(|m| (m * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// The photo centred on a square of the mean colour, RGB.
pub fn letterbox(img: &Image, fill: [u8; 3]) -> Result<(Vec<u8>, usize), Error> {
    img.check()?;
    let side = img.width.max(img.height);
    let (ox, oy) = ((side - img.width) / 2, (side - img.height) / 2);
    let mut out = Vec::with_capacity(side * side * 3);
    for _ in 0..side * side {
        out.extend_from_slice(&fill);
    }
    for y in 0..img.height {
        for x in 0..img.width {
            let s = (y * img.width + x) * img.channels;
            let d = ((y + oy) * side + x + ox) * 3;
            out[d..d + 3].copy_from_slice(&img.pixels[s..s + 3]);
        }
    }
    Ok((out, side))
}

/// Fixed-point precision of the filter weights (for 8-bit images).
const PRECISION_BITS: u32 = 32 - 8 - 2;

/// For each output pixel along one axis: the first input pixel and the
/// fixed-point weights of the input pixels from there.
fn coefficients(in_size: usize, out_size: usize) -> Vec<(usize, Vec<i32>)> {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    // The bilinear (triangle) filter has a support of 1, widened when
    // shrinking so every input pixel contributes.
    let support = filterscale;
    let ss = 1.0 / filterscale;
    let tri = |x: f64| {
        let x = x.abs();
        if x < 1.0 { 1.0 - x } else { 0.0 }
    };
    (0..out_size)
        .map(|xx| {
            let center = (xx as f64 + 0.5) * scale;
            // As in C: truncation towards zero, then clamping.
            let xmin = ((center - support + 0.5) as i64).max(0) as usize;
            let xmax = (((center + support + 0.5) as i64).min(in_size as i64) as usize).saturating_sub(xmin);
            let w: Vec<f64> = (0..xmax).map(|x| tri((x as f64 + xmin as f64 - center + 0.5) * ss)).collect();
            let total: f64 = w.iter().sum();
            let one = f64::from(1u32 << PRECISION_BITS);
            let k = w
                .iter()
                .map(|v| {
                    let v = if total == 0.0 { 0.0 } else { v / total } * one;
                    (if v < 0.0 { v - 0.5 } else { v + 0.5 }) as i32
                })
                .collect();
            (xmin, k)
        })
        .collect()
}

fn clip8(acc: i64) -> u8 {
    (acc >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Resample an RGB image (`w × h`) to `ow × oh` with the bilinear filter:
/// horizontally, rounded to bytes, then vertically.
pub fn resize_bilinear(src: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<u8> {
    let half = 1i64 << (PRECISION_BITS - 1);
    let horizontal: Vec<u8> = if ow == w {
        src.to_vec()
    } else {
        let coeffs = coefficients(w, ow);
        let mut out = vec![0u8; ow * h * 3];
        for y in 0..h {
            for (xx, (xmin, k)) in coeffs.iter().enumerate() {
                for c in 0..3 {
                    let mut acc = half;
                    for (i, &kv) in k.iter().enumerate() {
                        acc += i64::from(src[(y * w + xmin + i) * 3 + c]) * i64::from(kv);
                    }
                    out[(y * ow + xx) * 3 + c] = clip8(acc);
                }
            }
        }
        out
    };
    if oh == h {
        return horizontal;
    }
    let coeffs = coefficients(h, oh);
    let mut out = vec![0u8; ow * oh * 3];
    for (yy, (ymin, k)) in coeffs.iter().enumerate() {
        for x in 0..ow {
            for c in 0..3 {
                let mut acc = half;
                for (i, &kv) in k.iter().enumerate() {
                    acc += i64::from(horizontal[((ymin + i) * ow + x) * 3 + c]) * i64::from(kv);
                }
                out[(yy * ow + x) * 3 + c] = clip8(acc);
            }
        }
    }
    out
}

/// The square, scaled photo as RGB bytes (`size × size × 3`), before
/// normalisation: what the training script's `prepare` produces.
pub fn square_rgb(img: &Image, cfg: &ModelConfig) -> Result<Vec<u8>, Error> {
    let (padded, side) = letterbox(img, pad_colour(cfg))?;
    Ok(resize_bilinear(&padded, side, side, cfg.image_size, cfg.image_size))
}

/// The network's input for one photo: `3 × size × size` floats, channel
/// planes in R, G, B order, normalised with the model's mean and std.
pub fn prepare(img: &Image, cfg: &ModelConfig) -> Result<Vec<f32>, Error> {
    Ok(normalise(&square_rgb(img, cfg)?, cfg))
}

/// RGB bytes (`size × size × 3`) → normalised channel planes.
pub fn normalise(rgb: &[u8], cfg: &ModelConfig) -> Vec<f32> {
    let n = rgb.len() / 3;
    let mut out = vec![0.0f32; n * 3];
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        for c in 0..3 {
            out[c * n + i] = (f32::from(px[c]) / 255.0 - cfg.mean[c]) / cfg.std[c];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DecoderConfig, EncoderConfig};

    fn cfg(size: usize) -> ModelConfig {
        ModelConfig {
            image_size: size,
            encoder: EncoderConfig { dim: 8, depth: 1, heads: 2, mlp_dim: 16, patch: 14, registers: 0 },
            decoder: DecoderConfig { dim: 8, layers: 1, heads: 2, ffn_dim: 16 },
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        }
    }

    #[test]
    fn letterbox_centres_and_pads() {
        // 2×1 red-green image → 2×2: the offset is half the padding,
        // rounded down, so a single padded row goes below.
        let px = [255, 0, 0, 0, 255, 0];
        let (out, side) =
            letterbox(&Image { width: 2, height: 1, channels: 3, pixels: &px }, [9, 9, 9]).unwrap();
        assert_eq!(side, 2);
        assert_eq!(out, vec![255, 0, 0, 0, 255, 0, 9, 9, 9, 9, 9, 9]);
        let rgba = [1, 2, 3, 99];
        let (out, _) = letterbox(&Image { width: 1, height: 1, channels: 4, pixels: &rgba }, [0; 3]).unwrap();
        assert_eq!(out, vec![1, 2, 3]);
        assert!(letterbox(&Image { width: 2, height: 2, channels: 3, pixels: &px }, [0; 3]).is_err());
    }

    #[test]
    fn resize_keeps_flat_colour_and_identity() {
        let flat: Vec<u8> = std::iter::repeat_n([10u8, 200, 77], 30 * 20).flatten().collect();
        let small = resize_bilinear(&flat, 30, 20, 7, 5);
        assert!(small.chunks(3).all(|p| p == [10, 200, 77]), "{small:?}");
        let big = resize_bilinear(&flat, 30, 20, 61, 43);
        assert!(big.chunks(3).all(|p| p == [10, 200, 77]));
        assert_eq!(resize_bilinear(&flat, 30, 20, 30, 20), flat);
    }

    #[test]
    fn padding_normalises_to_zero() {
        let c = cfg(14);
        let px = vec![0u8; 3 * 14 * 7];
        let v = prepare(&Image { width: 14, height: 7, channels: 3, pixels: &px }, &c).unwrap();
        assert_eq!(v.len(), 3 * 14 * 14);
        // Top-left corner is padding: about zero in every channel.
        for ch in 0..3 {
            assert!(v[ch * 196].abs() < 0.02, "{}", v[ch * 196]);
        }
        // The middle is black: strongly negative.
        assert!(v[7 * 14 + 7] < -2.0);
    }
}
