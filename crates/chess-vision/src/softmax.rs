//! Softmax over the last dimension, fused, with a backward pass: one kernel
//! forward (candle's own) and only its output kept for the backward pass.
//!
//! candle's fused `softmax_last_dim` has no backward pass, so training used
//! softmax built from ordinary operations: five full-size copies of every
//! attention matrix (scores, the maximum subtracted, its exponential, the
//! sums, the quotient), all kept alive by the graph until the backward pass.
//! At 392 px with a batch of 16 that is about 1.2 GB per encoder layer.
//! This keeps one, and runs one kernel instead of five.

use candle_core::{CpuStorage, CustomOp1, D, Layout, Result, Shape, Tensor};

struct Softmax;

/// Softmax over the last dimension of a contiguous tensor, differentiable.
pub fn softmax_last_dim(xs: &Tensor) -> Result<Tensor> {
    xs.contiguous()?.apply_op1(Softmax)
}

fn rows<T: Copy + PartialOrd + Default>(
    src: &[T],
    width: usize,
    exp: impl Fn(T, T) -> T,
    div: impl Fn(T, T) -> T,
    add: impl Fn(T, T) -> T,
) -> Vec<T> {
    let mut dst = vec![T::default(); src.len()];
    for (s, d) in src.chunks(width).zip(dst.chunks_mut(width)) {
        let max = s.iter().copied().fold(s[0], |m, x| if x > m { x } else { m });
        let mut sum = T::default();
        for (x, y) in s.iter().zip(d.iter_mut()) {
            *y = exp(*x, max);
            sum = add(sum, *y);
        }
        for y in d.iter_mut() {
            *y = div(*y, sum);
        }
    }
    dst
}

impl CustomOp1 for Softmax {
    fn name(&self) -> &'static str {
        "softmax-last-dim-trainable"
    }

    fn cpu_fwd(&self, storage: &CpuStorage, layout: &Layout) -> Result<(CpuStorage, Shape)> {
        let Some((start, end)) = layout.contiguous_offsets() else {
            candle_core::bail!("softmax: the input has to be contiguous")
        };
        let dims = layout.shape().dims();
        let width = dims[dims.len() - 1];
        let out = match storage {
            CpuStorage::F32(v) => {
                CpuStorage::F32(rows(&v[start..end], width, |x, m| (x - m).exp(), |a, b| a / b, |a, b| a + b))
            }
            CpuStorage::F64(v) => {
                CpuStorage::F64(rows(&v[start..end], width, |x, m| (x - m).exp(), |a, b| a / b, |a, b| a + b))
            }
            _ => candle_core::bail!("softmax: only f32 and f64 on the CPU"),
        };
        Ok((out, layout.shape().clone()))
    }

    /// candle's own kernel, as `candle_nn::ops::softmax_last_dim` calls it.
    #[cfg(feature = "metal")]
    fn metal_fwd(
        &self,
        storage: &candle_core::MetalStorage,
        layout: &Layout,
    ) -> Result<(candle_core::MetalStorage, Shape)> {
        use candle_core::DType;
        use candle_core::backend::BackendStorage;
        let device = storage.device();
        let encoder = device.command_encoder()?;
        encoder.set_label("softmax");
        let name = match storage.dtype() {
            DType::F32 => "softmax_f32",
            DType::F16 => "softmax_f16",
            DType::BF16 => "softmax_bf16",
            dtype => candle_core::bail!("softmax: {dtype:?} isn't supported"),
        };
        let n = layout.stride().len();
        if !(layout.is_contiguous() && layout.stride()[n - 1] == 1) {
            candle_core::bail!("softmax: the input has to be contiguous");
        }
        let last_dim = layout.dims()[layout.shape().rank() - 1];
        let elem_count = layout.shape().elem_count();
        let output = device
            .new_buffer_builder()
            .with_size_for(elem_count, storage.dtype())
            .with_label("softmax")
            .build()?;
        candle_metal_kernels::call_last_softmax(
            device.metal_device(),
            &encoder,
            device.kernels(),
            name,
            elem_count,
            last_dim,
            storage.buffer(),
            layout.start_offset() * storage.dtype().size_in_bytes(),
            &output,
        )
        .map_err(candle_core::Error::wrap)?;
        Ok((
            candle_core::MetalStorage::new(output, device.clone(), elem_count, storage.dtype()),
            layout.shape().clone(),
        ))
    }

    /// d softmax: `y · (g − Σ g·y)` over the last dimension.
    fn bwd(&self, _arg: &Tensor, res: &Tensor, grad_res: &Tensor) -> Result<Option<Tensor>> {
        let dot = (grad_res * res)?.sum_keepdim(D::Minus1)?;
        Ok(Some((res * grad_res.broadcast_sub(&dot)?)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, Var};

    #[test]
    fn matches_the_unfused_softmax_and_its_gradient() {
        let x = Var::from_vec(
            (0..2 * 3 * 7).map(|i| ((i * 37 % 11) as f32 - 5.0) * 0.4).collect::<Vec<_>>(),
            (2, 3, 7),
            &Device::Cpu,
        )
        .unwrap();
        let w =
            Tensor::from_vec((0..7).map(|i| i as f32 - 2.5).collect::<Vec<_>>(), 7, &Device::Cpu).unwrap();
        let fused = softmax_last_dim(x.as_tensor()).unwrap();
        let plain = candle_nn::ops::softmax(x.as_tensor(), D::Minus1).unwrap();
        let diff = (&fused - &plain).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(diff < 1e-6, "forward differs by {diff}");

        let loss = |y: &Tensor| y.broadcast_mul(&w).unwrap().sqr().unwrap().sum_all().unwrap();
        let g_fused = loss(&fused).backward().unwrap();
        let g_plain = loss(&plain).backward().unwrap();
        let (a, b) = (g_fused.get(x.as_tensor()).unwrap(), g_plain.get(x.as_tensor()).unwrap());
        let diff = (a - b).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(diff < 1e-5, "gradient differs by {diff}");
    }
}
