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
    crate::crf::backend::cpu::simd::sub_i32(a, b, out)
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
