//! 闭环预测 + 死区量化（真有损模式核心）
//!
//! 规划文档 §3.6/§6.3：闭环量化依赖空间预测的 [`crate::crf::core::prediction::intra::predict_at`]，
//! 但自身是"量化"职责，与纯空间预测（apply/undo）分离。P4 已从 `format/closed_loop.rs` 迁入。

use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::predict_at;

/// 死区标量量化单点（round-to-nearest，输出为 Q 的倍数）
#[inline]
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
fn quant_scalar(v: i32, q: i32) -> i32 {
    let half = q / 2;
    if v >= 0 {
        (v + half) / q * q
    } else {
        -(((-v) + half) / q * q)
    }
}

/// 带死区偏置的单点量化：bias 为 /64 定点偏置。
/// **正 bias 单侧加宽负残差死区（负向 ±1~±3 映射为 0），负 bias 单侧加宽正向死区——与批量路径 quantize_residuals_tuned 的双向语义相反，标定以实测为准（optimization-review §12）。**
#[inline]
fn quant_scalar_biased(v: i32, q: i32, bias_r6: i32) -> i32 {
    let denom = q * 64;
    let half_r6 = denom / 2;
    let b = if v >= 0 { bias_r6 } else { -bias_r6 };
    // 对绝对值做 round-to-nearest 再恢复符号（避免负数截断非对称）
    let level = (v.wrapping_abs() * 64 + half_r6 + b) / denom;
    let sign = if v < 0 { -1 } else { 1 };
    sign * level * q
}

/// 闭环预测 + 死区量化（真有损模式核心）
///
/// 编码端逐像素从**重建缓冲**取预测邻居（与解码端完全一致），
/// 从根源上杜绝开环预测的误差级联漂移。
/// 每像素重建误差恰为该点残差的量化误差，≤ ⌊Q/2⌋。
///
/// 返回（量化残差流, 重建帧）；熵编码量化残差，逆预测作用于其上即得重建帧。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn closed_loop_predict_quant(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    q_step: u8,
    deadzone_bias: i8,
) -> (Vec<i32>, Vec<i32>) {
    closed_loop_predict_quant_banded(
        pixels,
        width,
        height,
        components,
        mode,
        q_step,
        deadzone_bias,
        None,
    )
}

/// 闭环预测 + 死区量化 + **逐条带自适应步长**（噪声归一化执行器）
///
/// [`closed_loop_predict_quant`] 的超集：`band_steps` 提供每
/// BAND_HEIGHT 行条带的有效量化步长覆盖表——失真水平高的条带
/// （天气颗粒/色调漂移）死区自动加宽，失真低的条带保持基础步长
/// 精细度。空间预测天然完成"中心化"（常数漂移被邻居预测抵消），
/// 因此无需零中心性判定，直接按条带放大死区即可。
///
/// 正确性约束：
/// - 条带按光栅顺序处理，与编码端条带划分（BAND_HEIGHT 行）对齐；
/// - `band_steps.len()` 须为 ceil(height / BAND_HEIGHT)，越界部分回退基础步长；
/// - 解码端无感：输出仍为各条带 Q_eff 倍数的自描述残差，undo_prediction
///   不需要知道任何步长信息，格式零改动。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub fn closed_loop_predict_quant_banded(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    q_step: u8,
    deadzone_bias: i8,
    band_steps: Option<&[u8]>,
) -> (Vec<i32>, Vec<i32>) {
    let n = pixels.len();
    let mut residuals = vec![0i32; n];
    let mut recon = vec![0i32; n];
    closed_loop_predict_quant_banded_into(
        pixels,
        &mut residuals,
        &mut recon,
        width,
        height,
        components,
        mode,
        q_step,
        deadzone_bias,
        band_steps,
    );
    (residuals, recon)
}

/// 闭环预测量化到调用方提供的整帧 Scratch Buffer。
///
/// 与 [`closed_loop_predict_quant_banded`] 逐位一致，但不分配返回向量；
/// 编码器可在多个候选之间复用 `residuals`/`recon` 的容量。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub(crate) fn closed_loop_predict_quant_banded_into(
    pixels: &[i32],
    residuals: &mut [i32],
    recon: &mut [i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    q_step: u8,
    deadzone_bias: i8,
    band_steps: Option<&[u8]>,
) {
    assert_eq!(pixels.len(), residuals.len(), "量化残差缓冲长度不匹配");
    assert_eq!(pixels.len(), recon.len(), "闭环重建缓冲长度不匹配");
    let stride = width * components;

    // 行 → 条带有效步长的快速查表；None 时全帧统一
    let band_h = crate::crf::core::bitstream::constants::BAND_HEIGHT;
    let step_of_band = |y: usize| -> u8 {
        match band_steps {
            Some(t) if !t.is_empty() => t[(y / band_h).min(t.len() - 1)].max(1),
            _ => q_step.max(1),
        }
    };

    if mode == PredictionMode::None {
        // 无空间预测：直接对原始值量化
        for y in 0..height {
            let q = step_of_band(y);
            for x in 0..width {
                for c in 0..components {
                    let idx = y * stride + x * components + c;
                    let vq = quant_scalar_biased(pixels[idx], q as i32, deadzone_bias as i32);
                    residuals[idx] = vq;
                    recon[idx] = vq;
                }
            }
        }
        return;
    }

    for y in 0..height {
        let q_row = step_of_band(y);
        for x in 0..width {
            for c in 0..components {
                let idx = y * stride + x * components + c;
                let predicted = predict_at(&recon, idx, x, y, stride, components, width, mode);
                let raw = pixels[idx] - predicted;
                let quantized = quant_scalar_biased(raw, q_row as i32, deadzone_bias as i32);
                residuals[idx] = quantized;
                recon[idx] = predicted + quantized;
            }
        }
    }
}
