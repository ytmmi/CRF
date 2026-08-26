//! backend 统一运算入口（kernel 调度边界）
//!
//! 规划文档 §2 / §7.5。算法层（encoder/decoder/core）只能通过本模块
//! 调用后端运算，不得直接依赖具体后端实现（`cpu::simd` / `gpu`）。
//! 本模块为当前唯一实现（CPU SIMD，带运行时 AVX2 分派）的统一入口；
//! scalar / GPU 后端接入时仅需在本模块内部扩展分派，不改动调用方。
//!
//! 正确性约束：所有运算与 scalar 参考实现逐位一致（`cpu::simd` 已保证）。

/// out[i] = a[i] - b[i]（SIMD 分派）
#[inline]
pub fn sub_i32(a: &[i32], b: &[i32], out: &mut [i32]) {
    #[cfg(feature = "nvidia-cuda")]
    if try_nvidia_sub_i32(a, b, out) {
        return;
    }
    crate::crf::backend::cpu::simd::sub_i32(a, b, out)
}

#[cfg(feature = "nvidia-cuda")]
fn try_nvidia_sub_i32(a: &[i32], b: &[i32], out: &mut [i32]) -> bool {
    use std::sync::{Mutex, OnceLock};
    const GPU_THRESHOLD: usize = 1_048_576;
    if a.len() != b.len() || a.len() != out.len() || a.len() < GPU_THRESHOLD {
        return false;
    }
    enum GpuState {
        Uninitialized,
        Ready(crate::crf::backend::gpu::NvidiaCudaBackend),
        Disabled,
    }
    static BACKEND: OnceLock<Mutex<GpuState>> = OnceLock::new();
    let state = BACKEND.get_or_init(|| Mutex::new(GpuState::Uninitialized));
    let Ok(mut guard) = state.lock() else {
        return false;
    };
    if matches!(*guard, GpuState::Uninitialized) {
        *guard = match crate::crf::backend::gpu::NvidiaCudaBackend::new(0) {
            Some(backend) => GpuState::Ready(backend),
            None => GpuState::Disabled,
        };
    }
    let GpuState::Ready(backend) = &*guard else {
        return false;
    };
    match backend.diff_i32(a, b) {
        Ok(values) => {
            out.copy_from_slice(&values);
            true
        }
        Err(_) => {
            // 将设备/驱动/kernel 失败视为不可恢复，后续调用直接走 CPU。
            *guard = GpuState::Disabled;
            false
        }
    }
}

/// YCoCg-R 正向变换（3 分量交织，原地）
#[inline]
pub fn rct_forward(pixels: &mut [i32]) {
    crate::crf::backend::cpu::simd::rct_forward_interleaved(pixels)
}

/// YCoCg-R 逆向变换（3 分量交织，原地）
#[inline]
pub fn rct_inverse(pixels: &mut [i32]) {
    crate::crf::backend::cpu::simd::rct_inverse_interleaved(pixels)
}

/// 软阈值（单平面）：|v| <= t 的置零
#[inline]
pub fn soft_threshold_plane(pixels: &mut [i32], t: i32) {
    crate::crf::backend::cpu::simd::soft_threshold_plane(pixels, t)
}

#[cfg(all(test, feature = "nvidia-cuda"))]
mod tests {
    use super::*;

    #[test]
    fn large_diff_uses_gpu_or_cpu_fallback_with_identical_result() {
        let a = vec![17i32; 1_048_576];
        let b = vec![-9i32; 1_048_576];
        let mut out = vec![0i32; a.len()];
        sub_i32(&a, &b, &mut out);
        assert!(out.iter().all(|&value| value == 26));
    }
}
