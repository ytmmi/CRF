//! 预测模式代价估计（RDO 排序用）
//!
//! 规划文档 §3.5 cost.rs：模式代价是"选择"职责，与空间预测执行
//! （apply/undo）分离。P4 已从 `format/cost.rs` 迁入。

use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::predict_at;

/// 4×4 Hadamard SATD（变换绝对值和）。
///
/// SATD 比逐像素 SAD 更能反映预测残差的频域能量：低频偏差和边缘
/// 会得到与实际熵编码更接近的代价。实现使用 i64 累加，避免极值输入
/// 在调试构建中溢出；返回值不做 `/2` 归一化（所有候选共用同一比例，
/// 不影响排序）。
#[inline]
fn satd4x4(block: &[i32; 16]) -> u64 {
    let mut rows = [[0i64; 4]; 4];
    for r in 0..4 {
        let x0 = block[r * 4] as i64;
        let x1 = block[r * 4 + 1] as i64;
        let x2 = block[r * 4 + 2] as i64;
        let x3 = block[r * 4 + 3] as i64;
        let a0 = x0 + x1;
        let a1 = x0 - x1;
        let a2 = x2 + x3;
        let a3 = x2 - x3;
        rows[r] = [a0 + a2, a1 + a3, a0 - a2, a1 - a3];
    }

    let mut total = 0u64;
    // c 为 Hadamard 列索引：需同时读取 4 行的同一列，无法用行迭代器替代。
    #[allow(clippy::needless_range_loop)]
    for c in 0..4 {
        let a0 = rows[0][c] + rows[1][c];
        let a1 = rows[0][c] - rows[1][c];
        let a2 = rows[2][c] + rows[3][c];
        let a3 = rows[2][c] - rows[3][c];
        for value in [a0 + a2, a1 + a3, a0 - a2, a1 - a3] {
            total = total.saturating_add(value.unsigned_abs());
        }
    }
    total
}

/// 按 4×4 Hadamard SATD 估计预测模式代价。
///
/// 与旧的行采样 SAD 一样，该函数只用于候选预筛，不改变最终编码结果。
/// 每隔 8×8 区域取一个 4×4 块并按实际采样比例恢复全帧量级；边缘块
/// 以零填充，所有分量分别变换后求和。这样既维持约 1/4 的采样密度，
/// RGB 交织布局也能保持与实际预测一致的通道语义。
pub fn satd_for_mode_sampled(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> u64 {
    if mode == PredictionMode::None || width == 0 || height == 0 || components == 0 {
        return 0;
    }
    let stride = width.saturating_mul(components);
    if stride == 0 || pixels.len() < stride.saturating_mul(height) {
        return 0;
    }

    const BLOCK_SIZE: usize = 4;
    const BLOCK_STRIDE: usize = 8;
    let mut satd = 0u64;
    let mut sampled_values = 0u64;
    let mut block = [0i32; 16];
    for by in (0..height).step_by(BLOCK_STRIDE) {
        for bx in (0..width).step_by(BLOCK_STRIDE) {
            for c in 0..components {
                block.fill(0);
                for dy in 0..BLOCK_SIZE {
                    let y = by + dy;
                    if y >= height {
                        break;
                    }
                    for dx in 0..BLOCK_SIZE {
                        let x = bx + dx;
                        if x >= width {
                            break;
                        }
                        let index = y * stride + x * components + c;
                        let predicted =
                            predict_at(pixels, index, x, y, stride, components, width, mode);
                        block[dy * 4 + dx] = pixels[index].wrapping_sub(predicted);
                        sampled_values += 1;
                    }
                }
                satd = satd.saturating_add(satd4x4(&block));
            }
        }
    }
    let total_values = width.saturating_mul(height).saturating_mul(components) as u64;
    satd.saturating_mul(total_values)
        .checked_div(sampled_values)
        .unwrap_or(0)
}

/// 采样预测残差的平均绝对值，仅用于候选启用阈值。
///
/// 模式排序由 SATD 完成；这里保留原有 1/4 行采样尺度，避免将 SATD
/// 的频域增益误当成像素幅度，改变 CABAC/DCT 候选的启用边界。
pub(crate) fn residual_activity_for_mode_sampled(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> f64 {
    const ROW_STRIDE: usize = 4;
    if mode == PredictionMode::None || width == 0 || height == 0 || components == 0 {
        return 0.0;
    }
    let stride = width.saturating_mul(components);
    if stride == 0 || pixels.len() < stride.saturating_mul(height) {
        return 0.0;
    }
    let mut total = 0u64;
    let mut count = 0u64;
    for y in (0..height).step_by(ROW_STRIDE) {
        for x in 0..width {
            for c in 0..components {
                let index = y * stride + x * components + c;
                let predicted = predict_at(pixels, index, x, y, stride, components, width, mode);
                total = total
                    .saturating_add(pixels[index].wrapping_sub(predicted).unsigned_abs() as u64);
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satd_is_zero_for_zero_residual() {
        let pixels = vec![0i32; 8 * 8];
        assert_eq!(
            satd_for_mode_sampled(&pixels, 8, 8, 1, PredictionMode::DC),
            0
        );
    }

    #[test]
    fn satd_detects_high_frequency_residuals() {
        // 两块 SAD 均为 16；脉冲的能量分散到全部 Hadamard 系数，
        // 因而 SATD 高于只有 DC 系数的常量块。
        let flat = vec![1i32; 4 * 4];
        let mut impulse = vec![0i32; 4 * 4];
        impulse[0] = 16;
        assert!(
            satd4x4(flat.as_slice().try_into().unwrap())
                < satd4x4(impulse.as_slice().try_into().unwrap())
        );
    }

    #[test]
    fn satd_handles_partial_blocks_and_invalid_input() {
        let pixels = vec![10i32; 3 * 2];
        assert!(satd_for_mode_sampled(&pixels, 3, 2, 1, PredictionMode::Average) > 0);
        assert_eq!(
            satd_for_mode_sampled(&pixels, 4, 4, 1, PredictionMode::Average),
            0
        );
    }
}
