//! Another plausible photo of the same board: a different camera angle and
//! distance, light, focus and compression.
//!
//! The geometry is one perspective warp from the photo onto a new canvas:
//! sometimes padded out to a phone's shape or a border, turned to any side
//! of the board and tilted, sometimes as if seen at an angle. The whole
//! photo always stays in the frame, so no square its label has is ever cut
//! off, and nothing is mirrored (that would change the position).
//!
//! **Padded and unpadded.** The reader letterboxes every photo to a square
//! with the mean colour, so a phone's 4:3 photo arrives with bars and a
//! square one without. ChessReD is all square: half the photos here are
//! padded to a phone's shape in that colour, the photo anywhere along it,
//! so the network learns the bars are nothing.

use image::{Rgb, RgbImage};

/// A small, seeded generator (splitmix64): the same seed gives the same
/// augmentation, so a run can be repeated.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

/// Phone photo shapes, width / height.
pub const PHONE_SHAPES: [f64; 6] = [4.0 / 3.0, 3.0 / 4.0, 16.0 / 9.0, 9.0 / 16.0, 3.0 / 2.0, 2.0 / 3.0];

type Pt = (f64, f64);

/// Solve the 8 unknowns of the homography taking `from[i]` to `to[i]`.
fn homography(from: &[Pt; 4], to: &[Pt; 4]) -> Option<[f64; 8]> {
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ((x, y), (u, v)) = (from[i], to[i]);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let pivot = (col..8).max_by(|&r, &s| a[r][col].abs().total_cmp(&a[s][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let pivot_row = a[col];
        for (r, row) in a.iter_mut().enumerate() {
            if r != col {
                let f = row[col] / pivot_row[col];
                for (x, p) in row.iter_mut().zip(pivot_row.iter()).skip(col) {
                    *x -= f * p;
                }
            }
        }
    }
    let mut h = [0.0; 8];
    for i in 0..8 {
        h[i] = a[i][8] / a[i][i];
    }
    Some(h)
}

fn apply(h: &[f64; 8], (x, y): Pt) -> Pt {
    let w = h[6] * x + h[7] * y + 1.0;
    ((h[0] * x + h[1] * y + h[2]) / w, (h[3] * x + h[4] * y + h[5]) / w)
}

fn bilinear(img: &RgbImage, x: f64, y: f64) -> [f64; 3] {
    let (w, h) = (i64::from(img.width()), i64::from(img.height()));
    let x0 = (x.floor() as i64).clamp(0, w - 1);
    let y0 = (y.floor() as i64).clamp(0, h - 1);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (x - x0 as f64).clamp(0.0, 1.0);
    let fy = (y - y0 as f64).clamp(0.0, 1.0);
    let p = |xx: i64, yy: i64| img.get_pixel(xx as u32, yy as u32).0;
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = [0.0; 3];
    for k in 0..3 {
        let top = f64::from(a[k]) * (1.0 - fx) + f64::from(b[k]) * fx;
        let bot = f64::from(c[k]) * (1.0 - fx) + f64::from(d[k]) * fx;
        out[k] = top * (1.0 - fy) + bot * fy;
    }
    out
}

/// Draw `img` so that its corners land on `corners` (top-left, top-right,
/// bottom-right, bottom-left) of a `w × h` canvas filled with `fill`.
pub fn warp(img: &RgbImage, corners: &[Pt; 4], w: u32, h: u32, fill: [u8; 3]) -> RgbImage {
    let (iw, ih) = (f64::from(img.width()), f64::from(img.height()));
    let src = [(0.0, 0.0), (iw, 0.0), (iw, ih), (0.0, ih)];
    let mut out = RgbImage::from_pixel(w, h, Rgb(fill));
    let Some(back) = homography(corners, &src) else { return out };
    for (x, y, px) in out.enumerate_pixels_mut() {
        let (sx, sy) = apply(&back, (f64::from(x) + 0.5, f64::from(y) + 0.5));
        if sx >= 0.0 && sy >= 0.0 && sx < iw && sy < ih {
            let c = bilinear(img, sx - 0.5, sy - 0.5);
            *px = Rgb([c[0].round() as u8, c[1].round() as u8, c[2].round() as u8]);
        }
    }
    out
}

fn bounds(pts: &[Pt]) -> (f64, f64, f64, f64) {
    pts.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |(a, b, c, d), &(x, y)| {
        (a.min(x), b.min(y), c.max(x), d.max(y))
    })
}

/// Where the photo goes: its four corners on a canvas, and the canvas size.
pub fn placement(w: f64, h: f64, rng: &mut Rng) -> ([Pt; 4], f64, f64) {
    let mut photo = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let (mut cw, mut ch) = (w, h);
    let shift = |p: &mut [Pt; 4], dx: f64, dy: f64| p.iter_mut().for_each(|q| *q = (q.0 + dx, q.1 + dy));
    if rng.chance(0.5) {
        // Padded out to a phone's shape, the photo anywhere along it.
        let aspect = rng.pick(&PHONE_SHAPES);
        if w / h < aspect {
            cw = h * aspect;
        } else {
            ch = w / aspect;
        }
        let (dx, dy) = (rng.range(0.0, cw - w), rng.range(0.0, ch - h));
        shift(&mut photo, dx, dy);
    } else if rng.chance(0.3) {
        let pad = w.max(h) * rng.range(0.02, 0.15);
        shift(&mut photo, pad, pad);
        cw += 2.0 * pad;
        ch += 2.0 * pad;
    }
    // Turned to any side of the board, a little tilted, the canvas growing
    // to hold it all.
    let angle = (rng.pick(&[0.0, 90.0, 180.0, 270.0]) + rng.range(-12.0, 12.0)).to_radians();
    let (sin, cos) = angle.sin_cos();
    let (mx, my) = (cw / 2.0, ch / 2.0);
    let turn = |(x, y): Pt| (mx + (x - mx) * cos - (y - my) * sin, my + (x - mx) * sin + (y - my) * cos);
    let canvas = [(0.0, 0.0), (cw, 0.0), (cw, ch), (0.0, ch)].map(turn);
    photo = photo.map(turn);
    let (x0, y0, x1, y1) = bounds(&canvas);
    shift(&mut photo, -x0, -y0);
    let (mut cw, mut ch) = (x1 - x0, y1 - y0);
    // Sometimes seen at an angle: each corner of the photo moved a little,
    // then the canvas fitted round it with a margin.
    if rng.chance(0.4) {
        let s = cw.max(ch) * 0.06;
        for p in &mut photo {
            *p = (p.0 + rng.range(-s, s), p.1 + rng.range(-s, s));
        }
        let (a, b, c, d) = bounds(&photo);
        let m = cw.max(ch) * 0.03;
        let (ox, oy) = ((-a + m).max(0.0), (-b + m).max(0.0));
        shift(&mut photo, ox, oy);
        cw = (cw + ox).max(c + ox + m);
        ch = (ch + oy).max(d + oy + m);
    }
    (photo, cw, ch)
}

/// Light, colour, focus and compression.
fn photometric(img: &mut RgbImage, rng: &mut Rng) {
    let brightness = if rng.chance(0.8) { rng.range(0.6, 1.4) } else { 1.0 };
    let contrast = if rng.chance(0.8) { rng.range(0.6, 1.4) } else { 1.0 };
    let saturation = if rng.chance(0.8) { rng.range(0.5, 1.5) } else { 1.0 };
    let n = f64::from(img.width()) * f64::from(img.height());
    let mean = img
        .pixels()
        .map(|p| f64::from(p.0[0]) * 0.299 + f64::from(p.0[1]) * 0.587 + f64::from(p.0[2]) * 0.114)
        .sum::<f64>()
        / n.max(1.0);
    for p in img.pixels_mut() {
        let [r, g, b] = p.0.map(|v| f64::from(v) * brightness);
        let grey = r * 0.299 + g * 0.587 + b * 0.114;
        let c = [r, g, b].map(|v| {
            let sat = grey + (v - grey) * saturation;
            ((sat - mean * brightness) * contrast + mean * brightness).round().clamp(0.0, 255.0) as u8
        });
        p.0 = c;
    }
    if rng.chance(0.25) {
        *img = image::imageops::blur(img, rng.range(0.3, 1.5) as f32);
    }
    if rng.chance(0.5) {
        let mut buf = Vec::new();
        let q = 40 + rng.below(56) as u8;
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q).encode_image(img).is_ok()
            && let Ok(back) = image::load_from_memory_with_format(&buf, image::ImageFormat::Jpeg)
        {
            *img = back.to_rgb8();
        }
    }
}

/// Another photo of the same board.
pub fn augment(img: &RgbImage, rng: &mut Rng, fill: [u8; 3]) -> RgbImage {
    let (corners, cw, ch) = placement(f64::from(img.width()), f64::from(img.height()), rng);
    let mut out = warp(img, &corners, cw.ceil() as u32, ch.ceil() as u32, fill);
    photometric(&mut out, rng);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo whose board reaches the very edges, a red mark in each corner.
    fn marked(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([90, 120, 60]));
        for (x0, y0) in [(0, 0), (w - 10, 0), (0, h - 10), (w - 10, h - 10)] {
            for y in y0..y0 + 10 {
                for x in x0..x0 + 10 {
                    img.put_pixel(x, y, Rgb([255, 0, 0]));
                }
            }
        }
        img
    }

    fn corners_kept(img: &RgbImage) -> usize {
        let red: Vec<(f64, f64)> = img
            .enumerate_pixels()
            .filter(|(_, _, p)| i32::from(p.0[0]) - i32::from(p.0[1].max(p.0[2])) > 40)
            .map(|(x, y, _)| (f64::from(x), f64::from(y)))
            .collect();
        if red.is_empty() {
            return 0;
        }
        let n = red.len() as f64;
        let (cx, cy) = (red.iter().map(|p| p.0).sum::<f64>() / n, red.iter().map(|p| p.1).sum::<f64>() / n);
        let quads: std::collections::HashSet<(bool, bool)> =
            red.iter().map(|&(x, y)| (x > cx, y > cy)).collect();
        quads.len()
    }

    #[test]
    fn no_square_is_ever_cut_off() {
        let img = marked(120, 90);
        for seed in 0..300 {
            let mut rng = Rng::new(seed);
            let (corners, w, h) = placement(120.0, 90.0, &mut rng);
            for (x, y) in corners {
                assert!(
                    x >= -1e-6 && y >= -1e-6 && x <= w + 1e-6 && y <= h + 1e-6,
                    "seed {seed}: {corners:?} in {w}x{h}"
                );
            }
            let out = warp(&img, &corners, w.ceil() as u32, h.ceil() as u32, [124, 116, 104]);
            assert_eq!(corners_kept(&out), 4, "seed {seed}");
        }
    }

    #[test]
    fn padding_reaches_phone_shapes_and_the_identity_is_exact() {
        let img = marked(100, 100);
        let shapes: std::collections::HashSet<i64> = (0..400)
            .map(|s| {
                let (_, w, h) = placement(100.0, 100.0, &mut Rng::new(s));
                (w / h * 100.0).round() as i64
            })
            .collect();
        assert!(shapes.len() > 10, "{shapes:?}");
        let id = warp(&img, &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)], 100, 100, [0, 0, 0]);
        assert_eq!(id, img);
    }

    #[test]
    fn same_seed_same_photo() {
        let img = marked(64, 48);
        let a = augment(&img, &mut Rng::new(7), [124, 116, 104]);
        let b = augment(&img, &mut Rng::new(7), [124, 116, 104]);
        assert_eq!(a, b);
        assert_eq!(corners_kept(&a), 4);
    }
}
