//! DCT 变换域量化编码路径（frame_type=6）
//!
//! 对标 JPEG/AVIF 的变换编码管线：
//! 平面数据 → 分块 lifting DCT（4×4 / 8×8，v1.10 起可变）→ 死区量化
//! （flat / 感知矩阵）→ RLE+CABAC
//!
//! DCT 将能量集中到低频系数，量化后高频系数大量归零，
//! RLE 零行程效率远高于空间域标量量化。
//! 解码端对称还原：RLE+CABAC → 反量化 → 逆 zigzag → 逆 DCT。
//!
//! **迁移说明（P4）**：正变换（`dct_plane_forward_bs`）、感知矩阵（`qm`）
//! 与单点量化（`quant_scalar`）已迁移到 `core/transform`；本文件保留
//! frame_type=6 候选编排（解包 → 变换 → 量化 → 交织）。逆变换在
//! `core::transform::reconstruct`（P1 迁入）。

use crate::crf::core::transform::is_valid_rect;
use crate::crf::core::transform::plane::{dct_plane_forward, dct_plane_forward_bs};
use crate::crf::core::transform::qm::{
    quantize_coeffs_with_matrix, DCT_PERCEPTUAL_QM, DCT_PERCEPTUAL_QM8, DCT_PERCEPTUAL_QM_TALL,
    DCT_PERCEPTUAL_QM_WIDE,
};
use crate::crf::core::transform::quant::quantize_residuals;

/// 块形状合法集（v1.12：{bw, bh} 对，bw/bh ∈ {4, 8}）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub const SUPPORTED_BLOCK_SHAPES: [(usize, usize); 4] = [(4, 4), (8, 8), (8, 4), (4, 8)];

/// DCT 域量化：DCT 正变换 → 死区标量量化
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_quantize(pixels: &[i32], width: usize, height: usize, q_step: u8) -> Vec<i32> {
    let coeffs = dct_plane_forward(pixels, width, height);
    quantize_residuals(&coeffs, q_step)
}

/// 统一的交织数据 DCT 域量化入口（v1.12 矩形泛化）
///
/// `(block_w, block_h)` ∈ {(4,4),(8,8),(8,4),(4,8)} 选择变换几何；
/// `use_qm` 选择 flat / 感知矩阵量化。输入为交织布局 [c0,c1,c2,...] 的
/// 空间域样本（RCT 后的 YCoCg 差分），先按分量解包为独立平面，各平面
/// 单独 DCT + 量化，再重新交织输出——与解码端
/// [`dct_dequantize_inverse_interleaved_bs`] 严格对称。
///
/// 正确性关键：DCT 分块要求空间连续性，交织数据若按单平面索引处理
/// 会跨分量混叠采样（历史缺陷：仅前 1/3 样本被有效变换，色度整体湮灭）。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub fn dct_quantize_interleaved_bs(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
    block_w: usize,
    block_h: usize,
    use_qm: bool,
    allow_q1_scale: bool,
) -> Vec<i32> {
    assert!(
        is_valid_rect(block_w, block_h),
        "非法块形状 {}×{}",
        block_w,
        block_h
    );
    let quant_plane = |plane: &[i32]| -> Vec<i32> {
        let coeffs = dct_plane_forward_bs(plane, width, height, block_w, block_h);
        if use_qm && (q_step != 1 || allow_q1_scale) {
            let table: &[u32] = match (block_w, block_h) {
                (8, 8) => &DCT_PERCEPTUAL_QM8,
                (4, 4) => &DCT_PERCEPTUAL_QM,
                (8, 4) => &DCT_PERCEPTUAL_QM_WIDE,
                _ => &DCT_PERCEPTUAL_QM_TALL,
            };
            quantize_coeffs_with_matrix(
                &coeffs,
                width,
                height,
                q_step,
                table,
                block_w,
                block_h,
                allow_q1_scale,
            )
        } else if q_step <= 1 {
            coeffs // Q=1 量化恒等，跳过全平面扫描
        } else {
            quantize_residuals(&coeffs, q_step)
        }
    };

    if components <= 1 {
        return quant_plane(pixels);
    }
    let n = width * height;
    let mut planes: Vec<Vec<i32>> = vec![vec![0i32; n]; components];
    for (px, chunk) in pixels.chunks_exact(components).enumerate() {
        for (c, &v) in chunk.iter().enumerate() {
            planes[c][px] = v;
        }
    }
    for plane in planes.iter_mut().take(components) {
        *plane = quant_plane(plane);
    }
    crate::crf::core::transform::reconstruct::interleave(&planes, components)
}

/// 三分量交织数据的 DCT 域量化（frame_type=6 编码入口）
///
/// 4×4 flat 版兼容包装（v1.10 前语义；新代码请用 dct_quantize_interleaved_bs）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_quantize_interleaved(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
) -> Vec<i32> {
    dct_quantize_interleaved_bs(
        pixels, width, height, components, q_step, 4, 4, false, false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // 逆变换经公共路径访问（规划 §3.6：不保留 dct_path 转发层）
    use crate::crf::core::transform::reconstruct::{
        dct_dequantize_inverse, dct_dequantize_inverse_interleaved,
        dct_dequantize_inverse_interleaved_bs,
    };

    #[test]
    fn test_dct_quant_roundtrip_q1() {
        // Q=1 时量化恒等 → DCT 往返应精确还原
        let pixels: Vec<i32> = (0..256).map(|i| ((i * 37) % 200) - 100).collect();
        let quantized = dct_quantize(&pixels, 16, 16, 1);
        let restored = dct_dequantize_inverse(&quantized, 16, 16);
        assert_eq!(pixels, restored, "Q=1 应精确还原");
    }

    #[test]
    fn test_dct_quant_energy_compaction() {
        // 平滑图像 DCT 后高频系数量化归零率应很高
        let smooth: Vec<i32> = (0..64 * 64)
            .map(|i| {
                let x = i % 64;
                let y = i / 64;
                100 + x / 8 + y / 16
            })
            .collect();
        let q = 5u8;
        let quantized = dct_quantize(&smooth, 64, 64, q);
        let zeros = quantized.iter().filter(|&&v| v == 0).count();
        let zero_pct = zeros as f64 / quantized.len() as f64;
        println!("平滑图 Q={} 零系数占比: {:.1}%", q, zero_pct * 100.0);
        assert!(
            zero_pct > 0.3,
            "平滑图像零系数占比应 >30%，实际 {:.1}%",
            zero_pct * 100.0
        );
    }

    /// 构造三分量交织测试数据：Y 渐变 + Co/Cg 色块与边缘
    fn make_interleaved_rgb(w: usize, h: usize) -> Vec<i32> {
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let yy = ((x * 3 + y) % 256) as i32;
                let co: i32 = if x < w / 2 { 90 } else { -70 };
                let cg: i32 = if y < h / 2 { 45 } else { -45 };
                px.push(yy);
                px.push(co);
                px.push(cg);
            }
        }
        px
    }

    #[test]
    fn test_dct_interleaved_roundtrip_q1() {
        // Q=1 量化恒等 → 三分量交织 DCT 往返应逐位一致。
        // 历史缺陷回归锚点：交织数据曾被当单平面处理（后 2/3 清零），
        // 该测试在缺陷版本下必然失败。
        let pixels = make_interleaved_rgb(37, 23); // 非 4 对齐尺寸覆盖边界
        let q = dct_quantize_interleaved(&pixels, 37, 23, 3, 1);
        let back = dct_dequantize_inverse_interleaved(&q, 37, 23, 3);
        assert_eq!(pixels, back, "Q=1 三分量交织往返应精确还原");
    }

    #[test]
    fn test_dct_interleaved_lossy_psnr_floor() {
        // Q=5 有损往返：各分量信息均不得湮灭（历史缺陷下色度清零，
        // PSNR 跌破 20dB；正确实现应显著高于该下限）
        let pixels = make_interleaved_rgb(64, 64);
        let q = dct_quantize_interleaved(&pixels, 64, 64, 3, 5);
        let back = dct_dequantize_inverse_interleaved(&q, 64, 64, 3);
        let mut sq_sum = 0.0f64;
        for (a, b) in pixels.iter().zip(back.iter()) {
            let d = (a - b) as f64;
            sq_sum += d * d;
        }
        let mse = sq_sum / pixels.len() as f64;
        let psnr = if mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        } else {
            f64::INFINITY
        };
        assert!(
            psnr > 30.0,
            "Q=5 三分量交织往返 PSNR={:.2}dB 低于下限 30dB",
            psnr
        );
    }

    /// v1.9 感知矩阵：Q=1 时权重恒等（1×w/64 ≥1 但 w≥64 ⇒ Q_pos=1），
    /// 矩阵版必须与 flat 版同样实现精确还原。
    #[test]
    fn test_matrix_quant_q1_identity() {
        let pixels = make_interleaved_rgb(37, 23);
        let qm = dct_quantize_interleaved_bs(&pixels, 37, 23, 3, 1, 4, 4, true, false);
        let back = dct_dequantize_inverse_interleaved_bs(&qm, 37, 23, 3, 4, 4);
        assert_eq!(pixels, back, "Q=1 感知矩阵版应精确还原");
    }

    /// v1.13 无损矩形补齐：Q=1 下矩形 {8×4, 4×8} flat 版交织往返
    /// 必须逐位恒等（frame_type=6 无损候选的合法性与回归锚点）。
    #[test]
    fn test_rect_q1_identity_lossless_variants() {
        for (bw, bh) in [(8usize, 4usize), (4usize, 8usize)] {
            // 非 4 对齐尺寸覆盖残缺块透传边界
            let pixels = make_interleaved_rgb(37, 23);
            let q = dct_quantize_interleaved_bs(&pixels, 37, 23, 3, 1, bw, bh, false, false);
            let back = dct_dequantize_inverse_interleaved_bs(&q, 37, 23, 3, bw, bh);
            assert_eq!(pixels, back, "Q=1 矩形 {bw}×{bh} 交织往返应精确还原");
        }
    }

    /// v1.9 感知矩阵行为验证：
    /// ① 高频系数步长严格大于低频（权重单调性传导到输出格点间距）；
    /// ② 平滑块上矩阵版零系数占比不低于 flat 版（体积收益来源）；
    /// ③ PSNR 相对 flat 的劣化受控（≤1.5dB，主观质量由高频掩蔽兜底）。
    #[test]
    fn test_matrix_vs_flat_tradeoff() {
        // 平滑渐变 + 少量纹理，64×64 三分量
        let w = 64usize;
        let h = 64usize;
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let yy = 100 + (x / 4) as i32 + (y / 8) as i32;
                let tex = if (x % 16 < 8) && (y % 16 < 8) {
                    ((x * y) % 7) as i32
                } else {
                    0
                };
                px.push((yy + tex).clamp(0, 255));
                px.push(((x / 32) as i32 - 1) * 60);
                px.push(((y / 32) as i32 - 1) * 45);
            }
        }
        let q = 5u8;
        let flat = dct_quantize_interleaved(&px, w, h, 3, q);
        let mat = dct_quantize_interleaved_bs(&px, w, h, 3, q, 4, 4, true, false);

        let zeros = |v: &[i32]| v.iter().filter(|&&c| c == 0).count();
        assert!(
            zeros(&mat) >= zeros(&flat),
            "矩阵版零系数占比不应低于 flat 版"
        );

        let psnr_of = |recon: &[i32]| -> f64 {
            let back = dct_dequantize_inverse_interleaved(recon, w, h, 3);
            let mse: f64 = px
                .iter()
                .zip(back.iter())
                .map(|(a, b)| {
                    let d = (a - b) as f64;
                    d * d
                })
                .sum::<f64>()
                / px.len() as f64;
            if mse > 0.0 {
                10.0 * (255.0f64 * 255.0 / mse).log10()
            } else {
                f64::INFINITY
            }
        };
        let p_flat = psnr_of(&flat);
        let p_mat = psnr_of(&mat);
        assert!(
            p_flat - p_mat <= 1.5,
            "矩阵版 PSNR 劣化超限：flat={:.2} matrix={:.2}",
            p_flat,
            p_mat
        );
    }
}
