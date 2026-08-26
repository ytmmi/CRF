//! 预测模式代价估计（RDO 排序用）
//!
//! 从 `format/prediction.rs` 拆出（P4 整理，规划文档 §3.5 cost.rs）：
//! 模式代价是"选择"职责，与空间预测执行（apply/undo）分离，
//! 便于后续迁入 `core/prediction/cost.rs`。

use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::predict_at;

/// 行采样 SAD 快速估计（用于预测模式排序，非精确值）
/// 每隔 4 行统计一行的残差绝对值和并按比例放大。
/// 排序用途下与精确 SAD 的相对序高度一致，评估开销降为约 1/4。
pub fn sad_for_mode_sampled(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> u64 {
    const ROW_STRIDE: usize = 4;
    if mode == PredictionMode::None {
        return 0;
    }
    let stride = width * components;
    let mut sad: u64 = 0;
    let mut sampled_rows = 0usize;
    let mut y = 0usize;
    while y < height {
        for x in 0..width {
            for c in 0..components {
                let idx = y * stride + x * components + c;
                let predicted = predict_at(pixels, idx, x, y, stride, components, width, mode);
                sad += pixels[idx].wrapping_sub(predicted).unsigned_abs() as u64;
            }
        }
        sampled_rows += 1;
        y += ROW_STRIDE;
    }
    if sampled_rows == 0 {
        0
    } else {
        // 按采样行占比放大回全帧量级
        sad.saturating_mul(height as u64 / sampled_rows as u64)
    }
}
