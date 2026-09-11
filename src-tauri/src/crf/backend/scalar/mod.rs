//! scalar 参考实现 —— 唯一正确性基准
//!
//! 所有 CPU SIMD/GPU 实现必须与本模块逐位一致。
//! 性能后端通过 [`crate::crf::backend::BackendKernel`] trait 接入，
//! scalar 实现作为 fallback 和对拍基准始终可用。
//!
//! **P0 状态**：P5 阶段开始承载性能 kernel 的逐位参考实现。

#![allow(dead_code)]

/// 带符号死区偏置的批量 level 量化参考实现。
///
/// 公式与 frame_type=8 原 `quant_scalar` 一致：正 bias 单侧加宽负残差
/// 死区，输出为有符号 level（反量化时再乘 `q_step`）。使用 i64 中间值
/// 使完整 i32 输入域的行为确定，CPU SIMD 的极值回退也复用此实现。
pub fn quantize_levels_biased(values: &[i32], out: &mut [i32], q_step: u8, deadzone_bias: i8) {
    assert_eq!(values.len(), out.len(), "量化输入/输出长度必须一致");
    let q = i64::from(q_step.max(1));
    let denom = q * 64;
    let half = denom / 2;
    let bias = i64::from(deadzone_bias);

    for (&value, quantized) in values.iter().zip(out.iter_mut()) {
        let magnitude = i64::from(value).abs();
        let signed_bias = if value >= 0 { bias } else { -bias };
        let level = (magnitude * 64 + half + signed_bias) / denom;
        let signed_level = if value < 0 { -level } else { level };
        *quantized = signed_level as i32;
    }
}
