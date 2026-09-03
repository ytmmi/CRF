//! DCT 路径感知量化模块（v1.9 引入，v1.10 扩展 8×8）
//!
//! 感知权重表（/64 定点，按频率位置递增加粗高频步长）与
//! 矩阵版系数量化器。量化输出为各位置 Q_pos 倍数的自描述残差，
//! 解码端零感知（反量化隐含于残差值本身）。

use super::quant::quant_scalar;

/// 4×4 感知权重表：按 `freq = r + c` 递增，DC 权重恒为 64（×1.0）。
///
/// 人眼对比敏感度（CSF）随空间频率衰减，且插画差分能量集中于低频；
/// 最高频 ×2.25 为温和上限——避免线稿类内容出现可见的高频糊化。
pub const DCT_PERCEPTUAL_QM: [u32; 16] = [
    64, 68, 80, 96, //
    68, 80, 96, 112, //
    80, 96, 112, 128, //
    96, 112, 128, 144,
];

/// 8×8 感知权重表（v1.10）：同款 freq=r+c 对角带设计，
/// 频带更细（freq 0..14），高频上限保持 ×2.25 温和档。
#[rustfmt::skip]
pub const DCT_PERCEPTUAL_QM8: [u32; 64] = [
    64, 68, 72, 80,  88, 96, 104, 112,
    68, 72, 76, 84,  92,100, 108, 116,
    72, 76, 84, 92, 100,108, 112, 120,
    80, 84, 92,100, 108,112, 120, 128,
    88, 92,100,108, 112,120, 128, 132,
    96,100,108,112, 120,128, 132, 140,
   104,108,112,120, 128,132, 140, 144,
   112,116,120,128, 132,140, 144, 144,
];

/// 宽块感知权重表（v1.12）：w=8 × h=4，freq=r+c ∈ 0..10。
/// 水平频率分辨率细于垂直——适配水平走向纹理（发丝等）。
#[rustfmt::skip]
pub const DCT_PERCEPTUAL_QM_WIDE: [u32; 32] = [
    64, 68, 72, 80,  88, 96, 104, 112,
    68, 72, 76, 84,  92,100, 108, 116,
    72, 76, 84, 92, 100,108, 116, 124,
    80, 84, 92,100, 108,116, 124, 132,
];

/// 高块感知权重表（v1.12）：w=4 × h=8，为 WIDE 的转置。
/// 垂直频率分辨率细于水平——适配垂直走向纹理（裙褶等）。
#[rustfmt::skip]
pub const DCT_PERCEPTUAL_QM_TALL: [u32; 32] = [
    64, 68, 72, 80,
    68, 72, 76, 84,
    72, 76, 84, 92,
    80, 84, 92,100,
    88, 92,100,108,
    96,100,108,116,
   104,108,116,124,
   112,116,124,132,
];

/// 按感知矩阵量化 DCT 系数平面
///
/// 完整块内逐频率位置使用 `Q_pos = clamp(base_q·w/64, 1, 255)`；
/// 边界残缺块（透传的空间域样本）沿用基础步长，与 flat 版几何行为一致。
///
/// `allow_q1_scale`（v1.12 q95 视觉无损档）：base_q=1 时解除「量化恒等」
/// 保护——高频位置按矩阵权重得到 Q_pos=2，低频保持 1。仅 q95 档位启用。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub fn quantize_coeffs_with_matrix(
    coeffs: &[i32],
    width: usize,
    height: usize,
    base_q: u8,
    qm: &[u32],
    block_w: usize,
    block_h: usize,
    allow_q1_scale: bool,
) -> Vec<i32> {
    let mut out = coeffs.to_vec();
    for by in (0..height).step_by(block_h) {
        for bx in (0..width).step_by(block_w) {
            let full_block = by + block_h <= height && bx + block_w <= width;
            for r in 0..block_h {
                for c in 0..block_w {
                    if !full_block && (by + r >= height || bx + c >= width) {
                        continue; // 越界位置不存在
                    }
                    let idx = (by + r) * width + (bx + c);
                    let w = if full_block { qm[r * block_w + c] } else { 64 };
                    let q = if base_q <= 1 && !allow_q1_scale {
                        1i32
                    } else {
                        ((base_q as u32 * w) / 64).clamp(1, 255) as i32
                    };
                    out[idx] = quant_scalar(coeffs[idx], q);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权重表形状校验：DC=64、单调不减（沿对角带）、上界温和。
    #[test]
    fn test_qm_shape_invariants() {
        let tables: [(&[u32], usize, usize); 4] = [
            (&DCT_PERCEPTUAL_QM, 4, 4),
            (&DCT_PERCEPTUAL_QM8, 8, 8),
            (&DCT_PERCEPTUAL_QM_WIDE, 8, 4),
            (&DCT_PERCEPTUAL_QM_TALL, 4, 8),
        ];
        for (qm, bw, bh) in tables {
            assert_eq!(qm.len(), bw * bh);
            assert_eq!(qm[0], 64, "DC 权重必须为 ×1.0");
            for f in 0..(bw + bh - 2) {
                // 对角带内最大权重随 freq 单调不减
                let band_max = |fi: usize| -> u32 {
                    let mut m = 0u32;
                    for r in 0..bh {
                        let c = fi.checked_sub(r);
                        if let Some(c) = c {
                            if c < bw {
                                m = m.max(qm[r * bw + c]);
                            }
                        }
                    }
                    m
                };
                assert!(
                    band_max(f + 1) >= band_max(f),
                    "{bw}×{bh} freq {}→{} 带最大权重倒退",
                    f,
                    f + 1
                );
            }
            assert!(*qm.iter().max().unwrap() <= 144, "高频上限不得超过 ×2.25");
        }
    }
}
