//! The network: a DINOv2-style vision transformer turns the photo into
//! patch tokens, and 64 learned square queries read the board from them
//! through a small transformer decoder; one shared linear head gives each
//! square's 13 classes.
//!
//! This follows the method published in Seytre, "ChessQueries: Toward
//! Better Chess Board Recognition" (arXiv:2608.30762). It is written from
//! that description, not from the authors' code, and uses no weights of
//! theirs: their code and weights are licensed for noncommercial use only.
//!
//! Parameter names match Meta's DINOv2 checkpoints under `encoder.`, so
//! the trainer (`crates/chess-vision-train`) loads the Apache-2.0 DINOv2
//! weights straight in. The same code trains and reads.

use candle_core::{D, DType, Device, Module, Result, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, Init, Linear, VarBuilder, conv2d, linear, ops};

use crate::config::{DecoderConfig, EncoderConfig, ModelConfig};

/// Norm epsilon in the encoder (DINOv2's) and in the decoder.
const ENCODER_EPS: f64 = 1e-6;
const DECODER_EPS: f64 = 1e-5;

/// Reading with fixed weights, or training. candle's fused layer-norm and
/// softmax kernels are fast but have no backward pass: training uses the
/// same maths built from ordinary (differentiable) operations instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Infer,
    Train,
}

/// Layer normalisation over the last dimension.
struct Norm {
    weight: Tensor,
    bias: Tensor,
    eps: f64,
    mode: Mode,
}

impl Norm {
    fn new(dim: usize, eps: f64, vb: VarBuilder, mode: Mode) -> Result<Self> {
        Ok(Self {
            weight: vb.get_with_hints(dim, "weight", Init::Const(1.0))?,
            bias: vb.get_with_hints(dim, "bias", Init::Const(0.0))?,
            eps,
            mode,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        match self.mode {
            Mode::Infer => ops::layer_norm(&x.contiguous()?, &self.weight, &self.bias, self.eps as f32),
            Mode::Train => {
                let mean = x.mean_keepdim(D::Minus1)?;
                let centred = x.broadcast_sub(&mean)?;
                let var = centred.sqr()?.mean_keepdim(D::Minus1)?;
                centred
                    .broadcast_div(&(var + self.eps)?.sqrt()?)?
                    .broadcast_mul(&self.weight)?
                    .broadcast_add(&self.bias)
            }
        }
    }
}

/// Scaled dot-product attention over `(batch, heads, tokens, head_dim)`.
fn attend(q: &Tensor, k: &Tensor, v: &Tensor, head_dim: usize, mode: Mode) -> Result<Tensor> {
    let scale = 1.0 / (head_dim as f64).sqrt();
    let w = (q.matmul(&k.t()?)? * scale)?;
    let w = match mode {
        Mode::Infer => ops::softmax_last_dim(&w)?,
        Mode::Train => ops::softmax(&w, D::Minus1)?,
    };
    w.matmul(v)
}

/// `(batch, tokens, dim)` → `(batch, heads, tokens, dim / heads)`.
fn split_heads(x: &Tensor, heads: usize) -> Result<Tensor> {
    let (b, n, c) = x.dims3()?;
    x.reshape((b, n, heads, c / heads))?.transpose(1, 2)?.contiguous()
}

/// The inverse of [`split_heads`].
fn merge_heads(x: &Tensor) -> Result<Tensor> {
    let (b, h, n, d) = x.dims4()?;
    x.transpose(1, 2)?.reshape((b, n, h * d))
}

// ---------------------------------------------------------------- encoder

struct EncoderAttention {
    qkv: Linear,
    proj: Linear,
    heads: usize,
    mode: Mode,
}

impl EncoderAttention {
    fn new(vb: VarBuilder, c: &EncoderConfig, mode: Mode) -> Result<Self> {
        Ok(Self {
            qkv: linear(c.dim, 3 * c.dim, vb.pp("qkv"))?,
            proj: linear(c.dim, c.dim, vb.pp("proj"))?,
            heads: c.heads,
            mode,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (b, n, c) = x.dims3()?;
        let hd = c / self.heads;
        let qkv = self.qkv.forward(x)?.reshape((b, n, 3, self.heads, hd))?.permute((2, 0, 3, 1, 4))?;
        let q = qkv.get(0)?.contiguous()?;
        let k = qkv.get(1)?.contiguous()?;
        let v = qkv.get(2)?.contiguous()?;
        self.proj.forward(&merge_heads(&attend(&q, &k, &v, hd, self.mode)?)?)
    }
}

struct EncoderBlock {
    norm1: Norm,
    attn: EncoderAttention,
    ls1: Tensor,
    norm2: Norm,
    fc1: Linear,
    fc2: Linear,
    ls2: Tensor,
}

impl EncoderBlock {
    fn new(vb: VarBuilder, c: &EncoderConfig, mode: Mode) -> Result<Self> {
        Ok(Self {
            norm1: Norm::new(c.dim, ENCODER_EPS, vb.pp("norm1"), mode)?,
            attn: EncoderAttention::new(vb.pp("attn"), c, mode)?,
            ls1: vb.get_with_hints(c.dim, "ls1.gamma", Init::Const(1.0))?,
            norm2: Norm::new(c.dim, ENCODER_EPS, vb.pp("norm2"), mode)?,
            fc1: linear(c.dim, c.mlp_dim, vb.pp("mlp.fc1"))?,
            fc2: linear(c.mlp_dim, c.dim, vb.pp("mlp.fc2"))?,
            ls2: vb.get_with_hints(c.dim, "ls2.gamma", Init::Const(1.0))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let a = self.attn.forward(&self.norm1.forward(x)?)?.broadcast_mul(&self.ls1)?;
        let x = (x + a)?;
        let m = self.fc2.forward(&self.fc1.forward(&self.norm2.forward(&x)?)?.gelu_erf()?)?;
        x + m.broadcast_mul(&self.ls2)?
    }
}

/// DINOv2's vision transformer, at one fixed resolution: the position
/// embedding in the weights file is already sized for the model's
/// `image_size` (the training script interpolates it when it exports).
struct Encoder {
    patch_embed: Conv2d,
    cls_token: Tensor,
    pos_embed: Tensor,
    registers: Option<Tensor>,
    blocks: Vec<EncoderBlock>,
    norm: Norm,
}

impl Encoder {
    fn new(vb: VarBuilder, c: &EncoderConfig, grid: usize, mode: Mode) -> Result<Self> {
        let conv = Conv2dConfig { stride: c.patch, ..Default::default() };
        Ok(Self {
            patch_embed: conv2d(3, c.dim, c.patch, conv, vb.pp("patch_embed.proj"))?,
            cls_token: vb.get_with_hints(
                (1, 1, c.dim),
                "cls_token",
                Init::Randn { mean: 0.0, stdev: 0.02 },
            )?,
            pos_embed: vb.get_with_hints(
                (1, 1 + grid * grid, c.dim),
                "pos_embed",
                Init::Randn { mean: 0.0, stdev: 0.02 },
            )?,
            registers: if c.registers > 0 {
                Some(vb.get_with_hints(
                    (1, c.registers, c.dim),
                    "register_tokens",
                    Init::Randn { mean: 0.0, stdev: 0.02 },
                )?)
            } else {
                None
            },
            blocks: (0..c.depth)
                .map(|i| EncoderBlock::new(vb.pp(format!("blocks.{i}")), c, mode))
                .collect::<Result<_>>()?,
            norm: Norm::new(c.dim, ENCODER_EPS, vb.pp("norm"), mode)?,
        })
    }

    /// `(batch, 3, size, size)` → the patch tokens, `(batch, grid², dim)`.
    fn forward(&self, images: &Tensor) -> Result<Tensor> {
        let b = images.dim(0)?;
        let patches = self.patch_embed.forward(images)?.flatten_from(2)?.transpose(1, 2)?;
        let dim = patches.dim(2)?;
        let cls = self.cls_token.broadcast_as((b, 1, dim))?;
        let mut x = Tensor::cat(&[&cls, &patches], 1)?.broadcast_add(&self.pos_embed)?;
        let skip = 1 + self.registers.as_ref().map_or(0, |r| r.dim(1).unwrap_or(0));
        if let Some(r) = &self.registers {
            let r = r.broadcast_as((b, r.dim(1)?, dim))?;
            let n = x.dim(1)?;
            x = Tensor::cat(&[&x.narrow(1, 0, 1)?, &r, &x.narrow(1, 1, n - 1)?], 1)?;
        }
        for blk in &self.blocks {
            x = blk.forward(&x)?;
        }
        let x = self.norm.forward(&x)?;
        let n = x.dim(1)?;
        x.narrow(1, skip, n - skip)
    }
}

// ---------------------------------------------------------------- decoder

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
    heads: usize,
    mode: Mode,
}

impl Attention {
    fn new(vb: VarBuilder, dim: usize, heads: usize, mode: Mode) -> Result<Self> {
        Ok(Self {
            q: linear(dim, dim, vb.pp("q"))?,
            k: linear(dim, dim, vb.pp("k"))?,
            v: linear(dim, dim, vb.pp("v"))?,
            out: linear(dim, dim, vb.pp("out"))?,
            heads,
            mode,
        })
    }

    fn forward(&self, q: &Tensor, k: &Tensor, v: &Tensor) -> Result<Tensor> {
        let hd = q.dim(2)? / self.heads;
        let q = split_heads(&self.q.forward(q)?, self.heads)?;
        let k = split_heads(&self.k.forward(k)?, self.heads)?;
        let v = split_heads(&self.v.forward(v)?, self.heads)?;
        self.out.forward(&merge_heads(&attend(&q, &k, &v, hd, self.mode)?)?)
    }
}

/// A pre-norm DETR decoder layer: the squares attend to each other, then
/// to the image, then a feed-forward step.
struct DecoderLayer {
    norm1: Norm,
    self_attn: Attention,
    norm2: Norm,
    cross_attn: Attention,
    norm3: Norm,
    fc1: Linear,
    fc2: Linear,
}

impl DecoderLayer {
    fn new(vb: VarBuilder, c: &DecoderConfig, mode: Mode) -> Result<Self> {
        Ok(Self {
            norm1: Norm::new(c.dim, DECODER_EPS, vb.pp("norm1"), mode)?,
            self_attn: Attention::new(vb.pp("self_attn"), c.dim, c.heads, mode)?,
            norm2: Norm::new(c.dim, DECODER_EPS, vb.pp("norm2"), mode)?,
            cross_attn: Attention::new(vb.pp("cross_attn"), c.dim, c.heads, mode)?,
            norm3: Norm::new(c.dim, DECODER_EPS, vb.pp("norm3"), mode)?,
            fc1: linear(c.dim, c.ffn_dim, vb.pp("ffn.fc1"))?,
            fc2: linear(c.ffn_dim, c.dim, vb.pp("ffn.fc2"))?,
        })
    }

    fn forward(&self, t: &Tensor, query_pos: &Tensor, mem_keys: &Tensor, mem: &Tensor) -> Result<Tensor> {
        let t2 = self.norm1.forward(t)?;
        let qk = t2.broadcast_add(query_pos)?;
        let t = (t + self.self_attn.forward(&qk, &qk, &t2)?)?;
        let t2 = self.norm2.forward(&t)?;
        let t = (&t + self.cross_attn.forward(&t2.broadcast_add(query_pos)?, mem_keys, mem)?)?;
        let t2 = self.norm3.forward(&t)?;
        &t + self.fc2.forward(&self.fc1.forward(&t2)?.gelu_erf()?)?
    }
}

/// A fixed 2D sine/cosine position code for a `grid × grid` patch layout,
/// `(grid², dim)`, row-major from the top-left patch. The first half of
/// each code encodes the row, the second half the column; within each,
/// pairs of (sin, cos) at geometrically spaced frequencies.
pub fn sine_positions(grid: usize, dim: usize) -> Vec<f32> {
    let half = dim / 2;
    let mut out = vec![0.0f32; grid * grid * dim];
    let tau = std::f64::consts::TAU;
    let code = |p: usize, i: usize| {
        let pos = (p as f64 + 0.5) / grid as f64 * tau;
        let freq = 10_000f64.powf((2 * (i / 2)) as f64 / half as f64);
        let a = pos / freq;
        (if i.is_multiple_of(2) { a.sin() } else { a.cos() }) as f32
    };
    for y in 0..grid {
        for x in 0..grid {
            let row = &mut out[(y * grid + x) * dim..][..dim];
            for i in 0..half {
                row[i] = code(y, i);
                row[half + i] = code(x, i);
            }
        }
    }
    out
}

struct Decoder {
    proj: Linear,
    proj_norm: Norm,
    /// The squares' embeddings (a1 = 0) and their files', ranks' and
    /// colours'. Summed on every forward pass, not once: in training the
    /// optimiser changes them in place, and a sum kept from when the
    /// network was built would never see those changes.
    square: Tensor,
    file: Tensor,
    rank: Tensor,
    colour: Tensor,
    file_idx: Tensor,
    rank_idx: Tensor,
    colour_idx: Tensor,
    mem_pos: Tensor,
    layers: Vec<DecoderLayer>,
    norm: Norm,
    head: Linear,
    dim: usize,
}

impl Decoder {
    fn new(
        vb: VarBuilder,
        c: &DecoderConfig,
        encoder_dim: usize,
        grid: usize,
        device: &Device,
        mode: Mode,
    ) -> Result<Self> {
        let square =
            vb.get_with_hints((64, c.dim), "square_embed", Init::Randn { mean: 0.0, stdev: 0.02 })?;
        let file = vb.get_with_hints((8, c.dim), "file_embed", Init::Randn { mean: 0.0, stdev: 0.02 })?;
        let rank = vb.get_with_hints((8, c.dim), "rank_embed", Init::Randn { mean: 0.0, stdev: 0.02 })?;
        let colour = vb.get_with_hints((2, c.dim), "colour_embed", Init::Randn { mean: 0.0, stdev: 0.02 })?;
        let idx = |f: &dyn Fn(u32) -> u32| Tensor::from_iter((0..64u32).map(f), device);
        let mem_pos = Tensor::from_vec(sine_positions(grid, c.dim), (grid * grid, c.dim), device)?
            .to_dtype(vb.dtype())?
            .unsqueeze(0)?;
        Ok(Self {
            proj: linear(encoder_dim, c.dim, vb.pp("proj"))?,
            proj_norm: Norm::new(c.dim, DECODER_EPS, vb.pp("proj_norm"), mode)?,
            file_idx: idx(&|s| s % 8)?,
            rank_idx: idx(&|s| s / 8)?,
            colour_idx: idx(&|s| (s % 8 + s / 8) % 2)?,
            square,
            file,
            rank,
            colour,
            mem_pos,
            layers: (0..c.layers)
                .map(|i| DecoderLayer::new(vb.pp(format!("layers.{i}")), c, mode))
                .collect::<Result<_>>()?,
            norm: Norm::new(c.dim, DECODER_EPS, vb.pp("norm"), mode)?,
            head: linear(c.dim, 13, vb.pp("head"))?,
            dim: c.dim,
        })
    }

    /// One position code per square: its own embedding plus its file's,
    /// its rank's and its colour's, `(1, 64, dim)`.
    fn query_pos(&self) -> Result<Tensor> {
        (&self.square
            + self.file.index_select(&self.file_idx, 0)?
            + self.rank.index_select(&self.rank_idx, 0)?
            + self.colour.index_select(&self.colour_idx, 0)?)?
        .unsqueeze(0)
    }

    /// Patch tokens → logits, `(batch, 64, 13)`.
    fn forward(&self, tokens: &Tensor) -> Result<Tensor> {
        let b = tokens.dim(0)?;
        let query_pos = self.query_pos()?;
        let mem = self.proj_norm.forward(&self.proj.forward(tokens)?)?;
        let mem_keys = mem.broadcast_add(&self.mem_pos)?;
        let mut t = Tensor::zeros((b, 64, self.dim), mem.dtype(), mem.device())?;
        for l in &self.layers {
            t = l.forward(&t, &query_pos, &mem_keys, &mem)?;
        }
        self.head.forward(&self.norm.forward(&t)?)
    }
}

/// The whole network.
pub struct Network {
    encoder: Encoder,
    decoder: Decoder,
}

impl Network {
    /// A network for reading (fast, fused kernels).
    pub fn new(vb: VarBuilder, cfg: &ModelConfig) -> Result<Self> {
        Self::build(vb, cfg, Mode::Infer)
    }

    /// A network for training: the same parameters and maths, through
    /// operations that carry gradients.
    pub fn new_trainable(vb: VarBuilder, cfg: &ModelConfig) -> Result<Self> {
        Self::build(vb, cfg, Mode::Train)
    }

    fn build(vb: VarBuilder, cfg: &ModelConfig, mode: Mode) -> Result<Self> {
        let grid = cfg.grid();
        let device = vb.device().clone();
        Ok(Self {
            encoder: Encoder::new(vb.pp("encoder"), &cfg.encoder, grid, mode)?,
            decoder: Decoder::new(vb.pp("decoder"), &cfg.decoder, cfg.encoder.dim, grid, &device, mode)?,
        })
    }

    /// Normalised images `(batch, 3, size, size)` → logits `(batch, 64, 13)`.
    pub fn logits(&self, images: &Tensor) -> Result<Tensor> {
        self.decode(&self.encode(images)?)
    }

    /// The encoder alone: images → patch tokens `(batch, grid², dim)`.
    /// (Training detaches these while the encoder is frozen.)
    pub fn encode(&self, images: &Tensor) -> Result<Tensor> {
        self.encoder.forward(images)
    }

    /// The decoder alone: patch tokens → logits `(batch, 64, 13)`.
    pub fn decode(&self, tokens: &Tensor) -> Result<Tensor> {
        self.decoder.forward(tokens)
    }

    /// Normalised images → per-square class probabilities, `(batch, 64, 13)`, f32.
    pub fn probabilities(&self, images: &Tensor) -> Result<Tensor> {
        ops::softmax(&self.logits(images)?.to_dtype(DType::F32)?, D::Minus1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_positions_are_distinct_and_bounded() {
        let p = sine_positions(4, 8);
        assert_eq!(p.len(), 16 * 8);
        assert!(p.iter().all(|v| v.abs() <= 1.0));
        let rows: Vec<&[f32]> = p.chunks(8).collect();
        for i in 0..16 {
            for j in i + 1..16 {
                assert_ne!(rows[i], rows[j], "patches {i} and {j} share a code");
            }
        }
        // Same row → same first half; same column → same second half.
        assert_eq!(rows[0][..4], rows[3][..4]);
        assert_eq!(rows[1][4..], rows[5][4..]);
    }
}
