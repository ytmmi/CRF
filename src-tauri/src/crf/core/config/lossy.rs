//! lossy —— 真有损精细调参（原 format/quant.rs，P4 架构迁移）
//!
//! 规划文档 §3.2 / §6.3。参考 JPEG/AVIF/WebP/H.264/AV1 的率失真工具设计，
//! 提供色度步长、关键帧间隔、死区偏置、色度半分辨率等精细参数。
//! 配置层禁止执行像素循环、分配编码 buffer、读写文件或选择具体 frame type。

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
    /// 死区偏置（-32..=32，/64 定点）：**注意本字段仅作用于批量标量量化
    /// 路径（quantize_residuals_tuned），其公式无符号翻转——正 bias 抬高
    /// 双向归零阈值（更难归零、更精确）；负 bias 降低阈值（更易归零）。
    /// 闭环路径（quant_scalar_biased，prediction.rs）的符号约定相反：
    /// 正 bias 单侧加宽负残差死区。两条路径的 bias 不可以互换理解，
    /// 标定时必须以实测产物为准（见 optimization-review §12）。**
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
    /// 色度死区偏置独立通道（P1 色度精细化，规划 §5.3 deadzone_chroma 位）：
    /// None = 继承 `deadzone_bias`（§3.3 继承语义，默认）；
    /// Some(x) = Co/Cg 平面显式使用 x。
    /// **作用路径为 planar 子平面的闭环量化（quant_scalar_biased）**，
    /// 符号语义见其文档：正 bias 单侧加宽负残差死区（负向 ±1~±3 归零），
    /// 负 bias 单侧加宽正向死区。PNG1000 q95 实测 ±4 均为纯收益
    /// （体积 −9.4%，质量 +0.3dB，见 optimization-review §12 扫描表）。
    pub chroma_deadzone_bias: Option<i8>,
}

impl Default for LossyTuning {
    fn default() -> Self {
        LossyTuning {
            chroma_quant_percent: 130,
            keyframe_interval: 10,
            // 亮度 deadzone（§17 标定）：+4 在 q90(step=2) 多组 −5~11%
            // 且质量提升；q95(step=1)/q75(step=5) 因 denom 整数除法
            // 吸收而中性。正 bias 作用于闭环路径单侧加宽负残差死区。
            deadzone_bias: 4,
            chroma_half_res: true,
            anchor_quality_percent: 100,
            noise_adaptive: false,
            noise_tau_x100: 150,
            golden_lossless: true,
            // P1 标定采纳（optimization-review §13）：19 组分层实测 ±4 均
            // 为纯收益（体积均值 −2.4%，质量 max 降 −0.27dB/典型 +0.3dB），
            // −4 在主要收益锚点（PNG1000/c 组）优于 +4。
            chroma_deadzone_bias: Some(-4),
        }
    }
}

impl LossyTuning {
    /// 由可选调参解析完整配置（None → 全默认）
    pub fn resolve(tuning: Option<&Self>) -> Self {
        tuning.cloned().unwrap_or_default()
    }

    /// 色度步长 = clamp(亮度Q × chroma% , 1, 255)
    ///
    /// P1 色度通道解耦（规划 §4.2 策略二）：整数截断会使小步长区间的
    /// 比例完全失效——如 Q=1 × 130% → floor=1 == 亮度步长，色度量化
    /// 与亮度完全相同。此时按分量级 Q 选择升一级，使百分比真实生效；
    /// 其余档位保持 floor 结果不变（行为面最小）。
    pub fn chroma_step(&self, luma_q: u8) -> u8 {
        let scaled = (luma_q as u32 * self.chroma_quant_percent as u32) / 100;
        if self.chroma_quant_percent > 100 && scaled <= luma_q as u32 && luma_q < 255 {
            return luma_q + 1;
        }
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
///
/// **迁移说明（P4）**：原位于 `format/quant.rs`，随配置解析迁入 core/config。
#[inline]
pub fn is_q95_perceptual(quality: u8) -> bool {
    quality == 95
}

/// 质量档位 → 量化步长（原 format/quant.rs，P4 迁入）
pub fn quant_step_from_quality(quality: u8) -> u8 {
    let q = quality.min(100) as u32;
    ((100 - q).div_ceil(5) as u8).clamp(1, 20)
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
    fn test_chroma_step_percent_take_effect_at_small_q() {
        // P1 色度通道解耦：整数截断导致比例失效的小 Q 区间必须升一级
        let t = LossyTuning::default(); // chroma_quant_percent = 130
                                        // 失效场景：floor 结果 == 亮度步长 → 升级使百分比真实生效
        assert_eq!(t.chroma_step(1), 2, "Q=1 × 130% 不得截断回 Q=1");
        assert_eq!(t.chroma_step(2), 3, "Q=2 × 130% floor=2 == luma，应升级");
        assert_eq!(t.chroma_step(3), 4);
        // 比例已真实生效的档位保持 floor 结果（行为面最小）
        assert_eq!(t.chroma_step(4), 5, "Q=4 × 130% = 5.2 > 4，无需升级");
        assert_eq!(t.chroma_step(10), 13);

        // 比例 ≤100 时永不触发升级（色度不粗于亮度是显式意图）
        let mild = LossyTuning {
            chroma_quant_percent: 100,
            ..Default::default()
        };
        assert_eq!(mild.chroma_step(1), 1);
        let fine = LossyTuning {
            chroma_quant_percent: 80,
            ..Default::default()
        };
        assert_eq!(fine.chroma_step(10), 8, "比例<100 时色度更细");
    }

    #[test]
    fn test_chroma_deadzone_bias_default_and_inherit() {
        // P1 标定采纳（§13）：默认 Some(-4)；显式 None 仍表达"继承全局偏置"
        let t = LossyTuning::default();
        assert_eq!(t.chroma_deadzone_bias, Some(-4), "标定采纳的默认档");
        let resolved = LossyTuning::resolve(None);
        assert_eq!(resolved.chroma_deadzone_bias, Some(-4));
        // 显式 None（继承）与显式正值在 clone/resolve 后保留
        let inherit = LossyTuning {
            chroma_deadzone_bias: None,
            ..Default::default()
        };
        assert!(LossyTuning::resolve(Some(&inherit))
            .chroma_deadzone_bias
            .is_none());
        let explicit = LossyTuning {
            chroma_deadzone_bias: Some(4),
            ..Default::default()
        };
        assert_eq!(
            LossyTuning::resolve(Some(&explicit)).chroma_deadzone_bias,
            Some(4)
        );
    }

    #[test]
    fn test_chroma_step() {
        let t = LossyTuning::default();
        assert_eq!(t.chroma_step(10), 13); // 10×130%
        assert_eq!(t.chroma_step(20), 26);
    }
}
