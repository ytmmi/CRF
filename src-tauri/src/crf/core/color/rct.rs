//! 可逆色彩空间变换（原 format/rct.rs，P4 架构迁移）
//!
//! 规划文档 §3.4 / `color/rct.rs`。参考 AV1 / HEVC Range Extensions /
//! JPEG-XL 无损模式的标准做法：RGB 三通道高度相关，变换到亮度/色度
//! 分离的 YCoCg-R 域后各分量残差能量更集中、分布更尖峰，显著降低
//! 后续熵编码的输出大小。变换为纯整数运算，数学上严格可逆（无损）。
//!
//! **迁移说明（P4）**：本模块原位于 `format/rct.rs`，现迁移到
//! `core/color/rct`。`format/rct.rs` 保留为 `pub use` 转发层。
//! SIMD 实现仍在 `format/simd.rs` 中，P5 迁移到 `backend/cpu`。

use crate::crf::error::{CrfError, CrfResult};

/// 判断该色彩格式是否可应用 RCT
///
/// 仅对 3 分量格式（RGB/YUV444 族）有意义；Gray 无通道相关性可去除。
pub fn rct_applicable(components: usize) -> bool {
    components == 3
}

/// 正向 YCoCg-R 变换（原位语义：返回新向量）
///
///   Co = R - B
///   t  = B + (Co >> 1)        （>> 为 floor 移位，负数同样正确）
///   Cg = G - t
///   Y  = t + (Cg >> 1)
///
/// 输出布局与输入一致（逐像素 [Y, Co, Cg] 对应原 [R, G, B] 槽位）。
pub fn rct_forward(pixels: &[i32], components: usize) -> CrfResult<Vec<i32>> {
    let mut out = pixels.to_vec();
    rct_forward_in_place(&mut out, components)?;
    Ok(out)
}

/// 正向 YCoCg-R 变换（**真正原地**，不分配新缓冲）
///
/// 与 [`rct_forward`] 逐位一致，但直接改写传入切片——供已持有可变差分/
/// 首帧缓冲的调用方省去一次全帧 `to_vec` 克隆（P1：消除重复分配/RCT）。
/// 校验语义与 [`rct_forward`] 完全相同（仅 3 分量）。
pub fn rct_forward_in_place(pixels: &mut [i32], components: usize) -> CrfResult<()> {
    if !rct_applicable(components) {
        return Err(CrfError::InvalidCodingParams(
            "RCT requires 3-component color format".to_string(),
        ));
    }
    // 后端统一入口（backend::ops）：AVX2 可用时核心算术向量化
    // （逐位一致，见 backend/cpu/simd.rs；scalar/GPU 接入时仅改 ops 内部）
    crate::crf::backend::ops::rct_forward(pixels);
    Ok(())
}

/// 逆向 YCoCg-R 变换（精确还原原始 RGB）
///
///   t  = Y - (Cg >> 1)
///   G  = Cg + t
///   B  = t - (Co >> 1)
///   R  = Co + B
pub fn rct_inverse(pixels: &[i32], components: usize) -> CrfResult<Vec<i32>> {
    if !rct_applicable(components) {
        return Err(CrfError::InvalidCodingParams(
            "RCT requires 3-component color format".to_string(),
        ));
    }

    let mut out = pixels.to_vec();
    // 后端统一入口（backend::ops）：AVX2 可用时核心算术向量化
    crate::crf::backend::ops::rct_inverse(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rct_roundtrip_random() {
        // 随机数据（含负值——残差帧场景）必须逐字节还原
        let mut state: u64 = 0x243F6A8885A308D3; // 固定种子保证可重复
        let mut pixels = Vec::with_capacity(300 * 3);
        for _ in 0..300 {
            for ch in 0..3 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                // 混合 [-255, 255] 与 [0, 255] 两种取值域
                let v = (state >> 33) as i32 % 256;
                let v = if ch % 2 == 0 { v } else { v - 255 };
                pixels.push(v);
            }
        }

        let fwd = rct_forward(&pixels, 3).unwrap();
        let back = rct_inverse(&fwd, 3).unwrap();
        assert_eq!(pixels, back, "YCoCg-R 必须严格可逆");
    }

    #[test]
    fn test_rct_roundtrip_boundaries() {
        // 边界值全覆盖：0 与 255 的所有组合
        for r in [0i32, 255] {
            for g in [0i32, 255] {
                for b in [0i32, 255] {
                    let px = vec![r, g, b];
                    let back = rct_inverse(&rct_forward(&px, 3).unwrap(), 3).unwrap();
                    assert_eq!(px, back);
                }
            }
        }
    }

    #[test]
    fn test_rct_decorrelates_natural_pixels() {
        // 典型自然图像关系：R≈G+ε, B≈G-δ → Co/Cg 幅度远小于原始动态范围
        let mut total_before = 0i64;
        let mut total_after = 0i64;
        for g in 100..156i32 {
            let r = g + (g % 7) - 3;
            let b = g - (g % 5) + 2;
            let src = vec![r, g, b];
            total_before += src[0].abs() as i64 + src[1].abs() as i64 + src[2].abs() as i64;
            let dst = rct_forward(&src, 3).unwrap();
            total_after += dst.iter().map(|&v| v.abs() as i64).sum::<i64>();
        }
        assert!(
            total_after < total_before,
            "去相关后总幅度({})应低于原域({})",
            total_after,
            total_before
        );
    }

    #[test]
    fn test_rct_rejects_gray() {
        let px = vec![10i32, 20, 30];
        assert!(rct_forward(&px, 1).is_err());
        assert!(rct_inverse(&px, 1).is_err());
    }
}
