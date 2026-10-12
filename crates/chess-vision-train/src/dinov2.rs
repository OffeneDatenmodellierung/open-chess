//! Meta's DINOv2 weights (Apache-2.0) as the encoder's starting point.
//!
//! Two forms are read: Meta's own checkpoints (`dinov2_vits14_pretrain.pth`
//! and the like, from dl.fbaipublicfiles.com), whose names are ours, and
//! Hugging Face's (`facebook/dinov2-small` `model.safetensors`), whose names
//! are mapped, with query, key and value joined into one projection. The
//! position embedding is resized from the checkpoint's grid to ours.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use candle_core::{DType, Device, Tensor};

/// The checkpoint's tensors under our encoder's names (without the
/// `encoder.` prefix), the position embedding resized to `grid`.
pub fn load(path: &Path, grid: usize) -> Result<HashMap<String, Tensor>> {
    let raw: Vec<(String, Tensor)> = match path.extension().and_then(|e| e.to_str()) {
        Some("safetensors") => candle_core::safetensors::load(path, &Device::Cpu)?.into_iter().collect(),
        _ => candle_core::pickle::read_all(path)
            .with_context(|| format!("reading {} as a PyTorch checkpoint", path.display()))?,
    };
    let hf = raw.iter().any(|(k, _)| k.contains("embeddings.patch_embeddings"));
    let mut out = if hf {
        from_hugging_face(raw)?
    } else {
        raw.into_iter().filter(|(k, _)| k != "mask_token").collect()
    };
    let pos = out.remove("pos_embed").context("no position embedding in the checkpoint")?;
    out.insert("pos_embed".into(), resize_pos_embed(&pos.to_dtype(DType::F32)?, grid)?);
    Ok(out)
}

fn from_hugging_face(raw: Vec<(String, Tensor)>) -> Result<HashMap<String, Tensor>> {
    let raw: HashMap<String, Tensor> =
        raw.into_iter().map(|(k, v)| (k.strip_prefix("dinov2.").unwrap_or(&k).to_owned(), v)).collect();
    let get = |k: &str| {
        raw.get(k).cloned().with_context(|| format!("{k} missing from the Hugging Face checkpoint"))
    };
    let mut out = HashMap::new();
    out.insert("cls_token".to_owned(), get("embeddings.cls_token")?);
    out.insert("pos_embed".to_owned(), get("embeddings.position_embeddings")?);
    out.insert("patch_embed.proj.weight".to_owned(), get("embeddings.patch_embeddings.projection.weight")?);
    out.insert("patch_embed.proj.bias".to_owned(), get("embeddings.patch_embeddings.projection.bias")?);
    if let Ok(r) = get("embeddings.register_tokens") {
        out.insert("register_tokens".to_owned(), r);
    }
    out.insert("norm.weight".to_owned(), get("layernorm.weight")?);
    out.insert("norm.bias".to_owned(), get("layernorm.bias")?);
    let mut i = 0;
    while raw.contains_key(&format!("encoder.layer.{i}.norm1.weight")) {
        let l = |s: &str| format!("encoder.layer.{i}.{s}");
        let b = |s: &str| format!("blocks.{i}.{s}");
        for part in ["weight", "bias"] {
            out.insert(b(&format!("norm1.{part}")), get(&l(&format!("norm1.{part}")))?);
            out.insert(b(&format!("norm2.{part}")), get(&l(&format!("norm2.{part}")))?);
            out.insert(b(&format!("attn.proj.{part}")), get(&l(&format!("attention.output.dense.{part}")))?);
            out.insert(b(&format!("mlp.fc1.{part}")), get(&l(&format!("mlp.fc1.{part}")))?);
            out.insert(b(&format!("mlp.fc2.{part}")), get(&l(&format!("mlp.fc2.{part}")))?);
            let qkv = ["query", "key", "value"].map(|n| get(&l(&format!("attention.attention.{n}.{part}"))));
            let [q, k, v] = qkv;
            out.insert(b(&format!("attn.qkv.{part}")), Tensor::cat(&[q?, k?, v?], 0)?);
        }
        out.insert(b("ls1.gamma"), get(&l("layer_scale1.lambda1"))?);
        out.insert(b("ls2.gamma"), get(&l("layer_scale2.lambda1"))?);
        i += 1;
    }
    if i == 0 {
        bail!("no transformer layers in the Hugging Face checkpoint");
    }
    Ok(out)
}

/// Bicubic weights (a = -0.75, as PyTorch's `interpolate`).
fn cubic(x: f64) -> f64 {
    let a = -0.75;
    let x = x.abs();
    if x <= 1.0 {
        ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a
    } else {
        0.0
    }
}

/// A ViT position embedding `(1, 1 + g², c)` resized to `(1, 1 + grid², c)`,
/// the class token's kept.
pub fn resize_pos_embed(pos: &Tensor, grid: usize) -> Result<Tensor> {
    let (_, n_tok, c) = pos.dims3()?;
    let g = ((n_tok - 1) as f64).sqrt().round() as usize;
    if g * g + 1 != n_tok {
        bail!("position embedding of {n_tok} tokens isn't a square grid plus the class token");
    }
    if g == grid {
        return Ok(pos.clone());
    }
    let cls = pos.narrow(1, 0, 1)?;
    let patches = pos.narrow(1, 1, g * g)?.flatten_all()?.to_vec1::<f32>()?;
    // Along each row (g, g) → (g, grid), then down each column → (grid, grid).
    let mut wide = vec![0.0f32; g * grid * c];
    for row in 0..g {
        let line = resize_1d(&patches[row * g * c..(row + 1) * g * c], g, grid, c);
        wide[row * grid * c..(row + 1) * grid * c].copy_from_slice(&line);
    }
    let mut out = vec![0.0f32; grid * grid * c];
    for col in 0..grid {
        let column: Vec<f32> = (0..g)
            .flat_map(|row| wide[(row * grid + col) * c..(row * grid + col + 1) * c].to_vec())
            .collect();
        let tall = resize_1d(&column, g, grid, c);
        for row in 0..grid {
            out[(row * grid + col) * c..(row * grid + col + 1) * c]
                .copy_from_slice(&tall[row * c..(row + 1) * c]);
        }
    }
    let p = Tensor::from_vec(out, (1, grid * grid, c), pos.device())?;
    Ok(Tensor::cat(&[&cls, &p], 1)?)
}

/// `n` positions of `c` channels resized to `m` (bicubic, edges clamped,
/// pixel centres aligned as PyTorch's `align_corners=False`).
fn resize_1d(src: &[f32], n: usize, m: usize, c: usize) -> Vec<f32> {
    let scale = n as f64 / m as f64;
    let mut out = vec![0.0f32; m * c];
    for o in 0..m {
        let centre = (o as f64 + 0.5) * scale - 0.5;
        let base = centre.floor() as i64;
        let t = centre - base as f64;
        for k in -1..=2 {
            let i = (base + k).clamp(0, n as i64 - 1) as usize;
            let w = cubic(t - k as f64) as f32;
            for ch in 0..c {
                out[o * c + ch] += src[i * c + ch] * w;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_keeps_smooth_fields_and_the_class_token() {
        // Channel 0: a ramp along x; channel 1: constant. Bicubic with
        // a = -0.75 (PyTorch's) keeps constants exactly and ramps closely
        // and in order (only a = -0.5 would keep ramps exactly).
        let g = 8;
        let mut v = vec![7.0f32, -7.0]; // the class token
        for _y in 0..g {
            for x in 0..g {
                v.push(x as f32);
                v.push(2.5);
            }
        }
        let pos = Tensor::from_vec(v, (1, 1 + g * g, 2), &Device::Cpu).unwrap();
        let out = resize_pos_embed(&pos, 16).unwrap();
        assert_eq!(out.dims(), &[1, 1 + 256, 2]);
        let o = out.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert_eq!(&o[..2], &[7.0, -7.0]);
        let at = |x: usize, y: usize, ch: usize| o[2 + (y * 16 + x) * 2 + ch];
        for y in 0..16 {
            for x in 0..16 {
                assert!((at(x, y, 1) - 2.5).abs() < 1e-4);
                if x > 0 {
                    assert!(at(x, y, 0) > at(x - 1, y, 0), "not increasing at {x},{y}");
                }
            }
            // Output x = 8 sits at 3.75 in the 8-wide grid.
            assert!((at(8, y, 0) - 3.75).abs() < 0.1, "{}", at(8, y, 0));
        }
        assert_eq!(resize_pos_embed(&out, 16).unwrap().dims(), out.dims());
    }

    #[test]
    fn hugging_face_names_map_to_ours() {
        let t = |shape: &[usize]| Tensor::zeros(shape, DType::F32, &Device::Cpu).unwrap();
        let mut raw = vec![
            ("embeddings.cls_token".to_owned(), t(&[1, 1, 4])),
            ("embeddings.mask_token".to_owned(), t(&[1, 4])),
            ("embeddings.position_embeddings".to_owned(), t(&[1, 5, 4])),
            ("embeddings.patch_embeddings.projection.weight".to_owned(), t(&[4, 3, 14, 14])),
            ("embeddings.patch_embeddings.projection.bias".to_owned(), t(&[4])),
            ("layernorm.weight".to_owned(), t(&[4])),
            ("layernorm.bias".to_owned(), t(&[4])),
        ];
        for part in ["weight", "bias"] {
            let shape: &[usize] = if part == "weight" { &[4, 4] } else { &[4] };
            for n in ["norm1", "norm2"] {
                raw.push((format!("encoder.layer.0.{n}.{part}"), t(&[4])));
            }
            for n in [
                "attention.attention.query",
                "attention.attention.key",
                "attention.attention.value",
                "attention.output.dense",
            ] {
                raw.push((format!("encoder.layer.0.{n}.{part}"), t(shape)));
            }
            raw.push((
                format!("encoder.layer.0.mlp.fc1.{part}"),
                t(if part == "weight" { &[16, 4] } else { &[16] }),
            ));
            raw.push((
                format!("encoder.layer.0.mlp.fc2.{part}"),
                t(if part == "weight" { &[4, 16] } else { &[4] }),
            ));
        }
        raw.push(("encoder.layer.0.layer_scale1.lambda1".to_owned(), t(&[4])));
        raw.push(("encoder.layer.0.layer_scale2.lambda1".to_owned(), t(&[4])));
        let m = from_hugging_face(raw).unwrap();
        assert_eq!(m["blocks.0.attn.qkv.weight"].dims(), &[12, 4]);
        assert_eq!(m["blocks.0.attn.qkv.bias"].dims(), &[12]);
        assert!(
            m.contains_key("blocks.0.ls2.gamma")
                && m.contains_key("norm.bias")
                && !m.contains_key("mask_token")
        );
    }
}
