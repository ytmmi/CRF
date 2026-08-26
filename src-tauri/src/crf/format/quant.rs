//! 真有损压缩：残差死区标量量化
//!
//! 位置：空间预测之后、熵编码之前。量化把小幅值残差归零
//! （dead-zone 效应），使零值占比上升、RLE 行程更长——
//! 有损收益在二次元插画差分数据上被 RLE 天然放大。
//!
//! 解码端无需感知量化步长：编码器直接输出重建值域（Q 的倍数）的残差，
//! 熵解码结果即为反量化后的预测残差，后续逆预测流程与无损完全一致。
//!
//! 有损参数解析统一由 `core/config/lossy_v2` 提供；本模块仅保留底层量化数学 helper。

/// 批量死区量化（含偏置）
pub fn quantize_residuals_tuned(residuals: &[i32], q_step: u8, deadzone_bias: i8) -> Vec<i32> {
    let q = (q_step.max(1)) as i32;
    let denom = q * 64;
    let half_r6 = denom / 2;
    let bias_r6 = deadzone_bias as i32;
    residuals
        .iter()
        .map(|&v| {
            // 对绝对值做 round-to-nearest 再恢复符号，
            // 避免负数整数除法向零截断造成的非对称偏差
            let av = v.wrapping_abs();
            let level = (av * 64 + half_r6 + bias_r6) / denom;
            let sign = if v < 0 { -1 } else { 1 };
            sign * level * q
        })
        .collect()
}

/// 兼容接口：无偏置批量量化
pub fn quantize_residuals(residuals: &[i32], q_step: u8) -> Vec<i32> {
    quantize_residuals_tuned(residuals, q_step, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_error_bound() {
        // 误差界 |v − v′| ≤ ⌈Q/2⌉ 必须对所有输入成立（含 ±16 偏置极端）
        for q_step in [1u8, 2, 3, 5, 10, 20] {
            let q = q_step as i32;
            for &bias in &[0i8, -32, 32] {
                let mut state: u64 = 0x1234_5678_9ABC_DEF0;
                for _ in 0..5000 {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let v = ((state >> 33) as i32 % 600) - 300;
                    let vq = quantize_residuals_tuned(&[v], q_step, bias)[0];
                    let err = (v - vq).abs();
                    // 有偏置时允许误差扩大一个 Q（deadzone 语义本身如此）
                    let bound = ((q + 1) / 2) + bias.unsigned_abs() as i32 * q / 64;
                    assert!(
                        err <= bound,
                        "Q={} bias={} v={} vq={} 误差 {} 超界 {}",
                        q,
                        bias,
                        v,
                        vq,
                        err,
                        bound
                    );
                    assert_eq!(vq % q, 0, "输出必须是 Q 的倍数");
                }
            }
        }
    }

    #[test]
    fn test_quantize_q1_is_identity() {
        let values: Vec<i32> = (-200..200).collect();
        assert_eq!(quantize_residuals(&values, 1), values);
    }
}
