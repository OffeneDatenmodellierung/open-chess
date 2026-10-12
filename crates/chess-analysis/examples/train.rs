//! Train a (768 → H)×2 → 1 evaluation network on CPU from `FEN | score |
//! result` lines (see `examples/datagen.rs`) and write the quantised
//! weights `nnue::Network` loads.
//!
//! This is the small, dependency-free version: good for testing the
//! pipeline and for small networks. For real strength train on hundreds
//! of millions of positions with the `bullet` trainer on a GPU, which
//! reads the same data and writes the same weight layout.
//!
//! ```text
//! cargo run --release -p open-chess-analysis --example train -- \
//!     --data data.txt --hidden 128 --epochs 10 --out net.nnue
//! ```
//!
//! Loss: squared error between `sigmoid(eval / 400)` and a blend of the
//! search score and the game result (`--lambda`, the score's weight).

#![allow(clippy::many_single_char_names, clippy::needless_range_loop)]

use std::io::{BufRead, BufReader};

use cozy_chess::{Board, Color, Piece};
use open_chess_analysis::nnue::{INPUTS, Network, QA, QB, feature};

struct Sample {
    stm: Vec<u16>,
    ntm: Vec<u16>,
    /// Target win probability for the side to move.
    target: f32,
}

fn load(path: &str, lambda: f32) -> Vec<Sample> {
    let f = std::fs::File::open(path).expect("data file");
    let mut out = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        let [fen, score, result] = parts[..] else { continue };
        let (Ok(board), Ok(score), Ok(result)) =
            (Board::from_fen(fen, false), score.parse::<f32>(), result.parse::<f32>())
        else {
            continue;
        };
        let white = board.side_to_move() == Color::White;
        let (cp, res) = if white { (score, result) } else { (-score, 1.0 - result) };
        let target = lambda / (1.0 + (-cp / 400.0).exp()) + (1.0 - lambda) * res;
        let stm_color = board.side_to_move();
        let mut stm = Vec::with_capacity(32);
        let mut ntm = Vec::with_capacity(32);
        for sq in board.occupied() {
            let (Some(p), Some(c)) = (board.piece_on(sq), board.color_on(sq)) else { continue };
            stm.push(feature(stm_color, p, c, sq) as u16);
            ntm.push(feature(!stm_color, p, c, sq) as u16);
        }
        out.push(Sample { stm, ntm, target });
    }
    out
}

struct Adam {
    m: Vec<f32>,
    v: Vec<f32>,
}

impl Adam {
    fn new(n: usize) -> Adam {
        Adam { m: vec![0.0; n], v: vec![0.0; n] }
    }

    fn step(&mut self, w: &mut [f32], g: &[f32], lr: f32, t: i32, clip: f32) {
        let (b1, b2) = (0.9f32, 0.999f32);
        let c1 = 1.0 - b1.powi(t);
        let c2 = 1.0 - b2.powi(t);
        for i in 0..w.len() {
            if g[i] == 0.0 && self.m[i] == 0.0 {
                continue;
            }
            self.m[i] = b1 * self.m[i] + (1.0 - b1) * g[i];
            self.v[i] = b2 * self.v[i] + (1.0 - b2) * g[i] * g[i];
            w[i] -= lr * (self.m[i] / c1) / ((self.v[i] / c2).sqrt() + 1e-8);
            w[i] = w[i].clamp(-clip, clip);
        }
    }
}

fn screlu(x: f32) -> f32 {
    let c = x.clamp(0.0, 1.0);
    c * c
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let data = get("--data").unwrap_or_else(|| "data.txt".into());
    let h: usize = get("--hidden").and_then(|v| v.parse().ok()).unwrap_or(128);
    let epochs: usize = get("--epochs").and_then(|v| v.parse().ok()).unwrap_or(10);
    let lambda: f32 = get("--lambda").and_then(|v| v.parse().ok()).unwrap_or(0.75);
    let mut lr: f32 = get("--lr").and_then(|v| v.parse().ok()).unwrap_or(0.001);
    let batch: usize = get("--batch").and_then(|v| v.parse().ok()).unwrap_or(16_384);
    let out = get("--out").unwrap_or_else(|| "net.nnue".into());

    let mut samples = load(&data, lambda);
    let held = samples.len() / 20;
    let valid: Vec<Sample> = samples.split_off(samples.len() - held);
    eprintln!("{} training positions, {} held out", samples.len(), valid.len());

    // Parameters, f32. Seeded uniform initialisation.
    let mut seed: u64 = 7;
    let mut rnd = |scale: f32| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed % 20_001) as f32 / 10_000.0 - 1.0) * scale
    };
    let mut l0w: Vec<f32> = (0..INPUTS * h).map(|_| rnd(0.1)).collect();
    let mut l0b: Vec<f32> = (0..h).map(|_| rnd(0.1)).collect();
    let mut l1w: Vec<f32> = (0..2 * h).map(|_| rnd(1.0 / (h as f32).sqrt())).collect();
    let mut l1b = [0.0f32];
    let (mut a0w, mut a0b, mut a1w, mut a1b) =
        (Adam::new(l0w.len()), Adam::new(h), Adam::new(2 * h), Adam::new(1));
    // Input weights must survive quantisation to i16 at QA.
    let clip0 = 32_000.0 / QA as f32;
    let clip1 = 32_000.0 / QB as f32;

    let forward = |l0w: &[f32],
                   l0b: &[f32],
                   l1w: &[f32],
                   l1b: f32,
                   s: &Sample,
                   au: &mut Vec<f32>,
                   at: &mut Vec<f32>|
     -> f32 {
        au.clear();
        au.extend_from_slice(l0b);
        at.clear();
        at.extend_from_slice(l0b);
        for &f in &s.stm {
            let col = &l0w[f as usize * h..(f as usize + 1) * h];
            for i in 0..h {
                au[i] += col[i];
            }
        }
        for &f in &s.ntm {
            let col = &l0w[f as usize * h..(f as usize + 1) * h];
            for i in 0..h {
                at[i] += col[i];
            }
        }
        let mut o = l1b;
        for i in 0..h {
            o += screlu(au[i]) * l1w[i] + screlu(at[i]) * l1w[h + i];
        }
        o
    };

    let (mut au, mut at) = (Vec::with_capacity(h), Vec::with_capacity(h));
    let mut t = 0;
    let mut order: Vec<usize> = (0..samples.len()).collect();
    for epoch in 1..=epochs {
        // Shuffle.
        for i in (1..order.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        let mut total = 0.0f64;
        for chunk in order.chunks(batch) {
            let mut g0w = vec![0.0f32; l0w.len()];
            let mut g0b = vec![0.0f32; h];
            let mut g1w = vec![0.0f32; 2 * h];
            let mut g1b = 0.0f32;
            for &idx in chunk {
                let s = &samples[idx];
                let o = forward(&l0w, &l0b, &l1w, l1b[0], s, &mut au, &mut at);
                let p = 1.0 / (1.0 + (-o).exp());
                let err = p - s.target;
                total += f64::from(err * err);
                let d = 2.0 * err * p * (1.0 - p) / chunk.len() as f32;
                g1b += d;
                let mut du = vec![0.0f32; h];
                let mut dt = vec![0.0f32; h];
                for i in 0..h {
                    g1w[i] += d * screlu(au[i]);
                    g1w[h + i] += d * screlu(at[i]);
                    if au[i] > 0.0 && au[i] < 1.0 {
                        du[i] = d * l1w[i] * 2.0 * au[i];
                    }
                    if at[i] > 0.0 && at[i] < 1.0 {
                        dt[i] = d * l1w[h + i] * 2.0 * at[i];
                    }
                    g0b[i] += du[i] + dt[i];
                }
                for &f in &s.stm {
                    let col = &mut g0w[f as usize * h..(f as usize + 1) * h];
                    for i in 0..h {
                        col[i] += du[i];
                    }
                }
                for &f in &s.ntm {
                    let col = &mut g0w[f as usize * h..(f as usize + 1) * h];
                    for i in 0..h {
                        col[i] += dt[i];
                    }
                }
            }
            t += 1;
            a0w.step(&mut l0w, &g0w, lr, t, clip0);
            a0b.step(&mut l0b, &g0b, lr, t, clip0);
            a1w.step(&mut l1w, &g1w, lr, t, clip1);
            a1b.step(&mut l1b, &[g1b], lr, t, clip1);
        }
        let mut vloss = 0.0f64;
        for s in &valid {
            let o = forward(&l0w, &l0b, &l1w, l1b[0], s, &mut au, &mut at);
            let p = 1.0 / (1.0 + (-o).exp());
            vloss += f64::from((p - s.target).powi(2));
        }
        eprintln!(
            "epoch {epoch}: train loss {:.5}, held-out loss {:.5}",
            total / samples.len() as f64,
            vloss / valid.len().max(1) as f64
        );
        if epoch * 10 >= epochs * 7 {
            lr *= 0.5;
        }
    }

    // Quantise and save.
    let q = |x: f32, s: i32| (x * s as f32).round().clamp(-32_767.0, 32_767.0) as i16;
    let net = Network::from_parts(
        h,
        l0w.iter().map(|&x| q(x, QA)).collect(),
        l0b.iter().map(|&x| q(x, QA)).collect(),
        l1w.iter().map(|&x| q(x, QB)).collect(),
        q(l1b[0], QA * QB),
    );
    std::fs::write(&out, net.to_bytes()).expect("write network");
    // Sanity: the start position should be close to level.
    let start = net.evaluate_board(&Board::default());
    eprintln!("wrote {out} (hidden {h}); start position {start} cp");
    let _ = Piece::Pawn;
}
