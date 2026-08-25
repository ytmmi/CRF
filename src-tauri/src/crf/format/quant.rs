//! 真有损压缩：残差死区标量量化
//!
//! 位置：空间预测之后、熵编码之前。量化把小幅值残差归零
//! （dead-zone 效应），使零值占比上升、RLE 行程更长——
//! 有损收益在二次元插画差分数据上被 RLE 天然放大。
//!
//! 解码端无需感知量化步长：编码器直接输出重建值域（Q 的倍数）的残差，
//! 熵解码结果即为反量化后的预测残差，后续逆预测流程与无损完全一致。

/// 真有损精细调节参数（参考 JPEG/AVIF/WebP/H.264/AV1 的率失真工具）
#[derive(Debug, Clone)]
pub struct LossyTuning {
    /// 色度量化步长放大百分比（Co/Cg 平面步长 × chroma/100）
    ///
    /// 人眼对色度失真远不敏感：>100 时色度更粗、体积更小、观感几乎不变。
    /// 参考 JPEG 4:2:0 子采样 / HEVC chroma_qp_offset。
    pub chroma_quant_percent: u16,
    /// 关键帧间隔：每 N 帧插入一个无损刷新帧，阻断差分链量化误差累积；
    /// 0 表示仅首帧无损。参考 H.264/AV1 IDR interval。
    pub keyframe_interval: u8,
    /// 死区偏置（-32..=32，/64 定点）：正值更激进归零小残差
    /// （体积更小、暗部/纹理细节略降），负值保细节。参考 x264 deadzone。
    pub deadzone_bias: i8,
    /// 色度半分辨率（对标 AVIF/HEVC yuv420p）：Co/Cg 平面 2×2 均值下采样，
    /// 解码端双线性上采样。人眼对色度分辨率不敏感，可大幅缩减色度比特。
    pub chroma_half_res: bool,
    /// 锚点帧（首帧/间隔帧）质量百分比：100 = 与普通帧同等量化强度
    /// （对标 AVIF 全帧统一 CRF，无"首帧必须无损"概念）；
    /// <100 锚点帧质量更高（步长缩小）；0 = 锚点帧仍无损。
    pub anchor_quality_percent: u32,
    /// JPEG 源噪声感知：差分残差按条带估计噪声水平做软阈值归一化，
    /// 滤除有损源压缩噪声（|v| ≤ τ·σ 归零），真实信号零损伤。
    /// 仅作用于 input_original_frames 路径的差分帧；无损模式忽略。
    pub noise_adaptive: bool,
    /// 噪声阈值系数 τ×100（软阈值 = τ·σ，σ 为 Immerkær 条带估计值）。
    /// 150 = 1.5σ（约 87% 高斯噪声单侧覆盖）；越大滤噪越激进。
    pub noise_tau_x100: u16,
    /// golden 首帧无损开关（v1.12，默认 true 保持既有语义）：
    /// false 时首帧也按 anchor_quality_percent 量化——超小序列（2 帧）
    /// 中首帧占体积 53~97%，放开后有损档位收益大幅提升；
    /// 代价是全部帧的还原都携带首帧量化误差（展示用途可接受）。
    pub golden_lossless: bool,
}

impl Default for LossyTuning {
    fn default() -> Self {
        LossyTuning {
            chroma_quant_percent: 130,
            keyframe_interval: 10,
            deadzone_bias: 0,
            chroma_half_res: true,
            anchor_quality_percent: 100,
            noise_adaptive: false,
            noise_tau_x100: 150,
            golden_lossless: true,
        }
    }
}

impl LossyTuning {
    /// 由可选调参解析完整配置（None → 全默认）
    pub fn resolve(tuning: Option<&Self>) -> Self {
        tuning.cloned().unwrap_or_default()
    }

    /// 色度步长 = clamp(亮度Q × chroma% , 1, 255)
    pub fn chroma_step(&self, luma_q: u8) -> u8 {
        let scaled = (luma_q as u32 * self.chroma_quant_percent as u32) / 100;
        scaled.clamp(1, 255) as u8
    }

    /// 锚点帧步长 = clamp(全局Q × anchor% , 1, 255)；anchor%=0 时返回 0（无损）
    pub fn anchor_step(&self, global_q: u8) -> u8 {
        if self.anchor_quality_percent == 0 {
            return 0; // 无损锚点
        }
        let scaled = (global_q as u32 * self.anchor_quality_percent) / 100;
        scaled.clamp(1, 255) as u8
    }
}

/// 质量档位 → 量化步长映射
///
/// `Q = clamp((100 − q + 4) / 5, 1, 20)`：
/// - q = 100..96 → Q = 1（近无损）
/// - q = 90 / 75 / 50 / 25 / 1 → Q = 2 / 5 / 10 / 15 / 20
///
/// **q95 特殊语义（v1.12 视觉无损档，对标 AVIF cq18 / HEIF crf30）**：
/// 映射同样返回 Q = 1，但配合「矩阵缩放许可」（见 FrameQuant.q1_matrix_scale）——
/// 仅 DCT 变换域候选参与竞争，低频系数完全保留（Q_pos=1），高频按感知
/// 矩阵轻微粗化（最高频 Q_pos=2）。空间域候选在 Q=1 下退化为无损体积，
/// 由字节竞争自动让位给 DCT 候选。
#[inline]
pub fn is_q95_perceptual(quality: u8) -> bool {
    quality == 95
}

pub fn quant_step_from_quality(quality: u8) -> u8 {
    let q = quality.min(100) as u32;
    ((100 - q).div_ceil(5) as u8).clamp(1, 20)
}

/// 死区标量量化（四舍五入到最近量化级 + 可选偏置，输出为 Q 的倍数）
///
/// bias 为 /64 定点偏置：正 bias 等效提高归零阈值，
/// 使小残差更倾向于映射到 0（deadzone 变宽）。
#[inline]
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
fn quant_scalar_biased(v: i32, q: i32, bias_r6: i32) -> i32 {
    let half_r6 = (q * 64) / 2; // ⌊Q/2⌋ 的 /64 定点表示
    let shifted = v * 64 + half_r6 + if v >= 0 { bias_r6 } else { -bias_r6 };
    let level = shifted / (q * 64);
    level * q
}

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
    fn test_quant_step_mapping() {
        assert_eq!(quant_step_from_quality(100), 1);
        assert_eq!(quant_step_from_quality(96), 1);
        assert_eq!(quant_step_from_quality(90), 2);
        assert_eq!(quant_step_from_quality(75), 5);
        assert_eq!(quant_step_from_quality(50), 10);
        assert_eq!(quant_step_from_quality(25), 15);
        assert_eq!(quant_step_from_quality(1), 20);
    }

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

    #[test]
    fn test_chroma_step() {
        let t = LossyTuning::default();
        assert_eq!(t.chroma_step(10), 13); // 10×130%
        assert_eq!(t.chroma_step(20), 26);
    }
}
