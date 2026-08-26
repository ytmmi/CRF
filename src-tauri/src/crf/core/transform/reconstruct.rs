//! 逆变换与块重建（原 encoder/dct_path 的公共逆变换部分）
//!
//! 规划文档 §3.6。包含：
//! - 逆 DCT 平面变换（`dct_plane_inverse_bs`）；
//! - 交织数据逆变换（`dct_dequantize_inverse_interleaved_bs`）；
//! - 块提取/写回辅助（`transform_block`、`interleave`）。
//!
//! **迁移说明（P1）**：本模块的逆变换函数原位于 `encoder/dct_path/mod.rs`，
//! 因 `decoder/mod.rs` 生产路径直接调用
//! `crate::crf::encoder::dct_path::dct_dequantize_inverse_interleaved_bs` 导致
//! decoder → encoder 反向依赖。现将纯数学逆变换迁移到公共 `core/transform`，
//! encoder 与 decoder 均通过公共契约访问。
//!
//! `encoder/dct_path/mod.rs` 保留正变换和量化函数（编码端专用），逆变换
//! 改为 `pub use` 转发到本模块。`is_valid_rect` 来自已有的 `crate::crf::transform`
//! 顶层模块（P0 前即独立）。

use crate::crf::transform::dct4x4_inverse;
use crate::crf::transform::dct8x8_inverse;
use crate::crf::transform::{dct_rect_inverse, is_valid_rect};

/// 从 `src` 抽取 (bx,by) 处的完整矩形块 → `transform` → 写回 `dst` 同位置
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
fn transform_block(
    src: &[i32],
    dst: &mut [i32],
    width: usize,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
    transform: impl Fn(&[i32]) -> Vec<i32>,
) {
    let n = bw * bh;
    let mut block = vec![0i32; n];
    for r in 0..bh {
        for c in 0..bw {
            block[r * bw + c] = src[(by + r) * width + (bx + c)];
        }
    }
    let transformed = transform(&block);
    for r in 0..bh {
        for c in 0..bw {
            dst[(by + r) * width + (bx + c)] = transformed[r * bw + c];
        }
    }
}

/// 对整帧平面执行分块 DCT 逆变换（矩形形状）
///
/// 与编码端 `dct_plane_forward_bs` 同一几何判定：完整块逆变换，残缺块透传。
/// 残缺块若强行 clamp 填充，逆向会读回错误数据，破坏可逆性。
pub fn dct_plane_inverse_bs(
    coeffs: &[i32],
    width: usize,
    height: usize,
    block_w: usize,
    block_h: usize,
) -> Vec<i32> {
    assert!(
        is_valid_rect(block_w, block_h),
        "非法块形状 {}×{}",
        block_w,
        block_h
    );
    let mut out = coeffs.to_vec();
    for by in (0..height).step_by(block_h) {
        for bx in (0..width).step_by(block_w) {
            if by + block_h > height || bx + block_w > width {
                continue; // 残缺块透传（与正变换判定一致）
            }
            let kernel = |blk: &[i32]| match (block_w, block_h) {
                (8, 8) => dct8x8_inverse(blk),
                (8, _) => dct_rect_inverse(blk, 8, 4),
                _ => match block_h {
                    8 => dct_rect_inverse(blk, 4, 8),
                    _ => dct4x4_inverse(blk),
                },
            };
            transform_block(coeffs, &mut out, width, bx, by, block_w, block_h, kernel);
        }
    }
    out
}

/// 对整帧平面执行 4×4 分块 DCT 逆变换（兼容包装）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_plane_inverse(coeffs: &[i32], width: usize, height: usize) -> Vec<i32> {
    dct_plane_inverse_bs(coeffs, width, height, 4, 4)
}

/// DCT 域逆变换（反量化后调用）：量化系数平面 → 空间域重建
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_dequantize_inverse(coeffs: &[i32], width: usize, height: usize) -> Vec<i32> {
    dct_plane_inverse(coeffs, width, height)
}

/// 按分量平面的样本顺序重新交织为 [c0,c1,c2,...] 布局
pub(crate) fn interleave(planes: &[Vec<i32>], components: usize) -> Vec<i32> {
    let n = planes[0].len();
    let mut out = vec![0i32; n * components];
    for (px, chunk) in out.chunks_exact_mut(components).enumerate() {
        for (c, slot) in chunk.iter_mut().enumerate() {
            *slot = planes[c][px];
        }
    }
    out
}

/// 交织数据的逆变换：量化系数（交织）→ 空间域重建。
///
/// `(block_w, block_h)` 必须与编码端一致（由载荷 k 字节 bit5/bit7 标注）。
/// 与编码端 `dct_quantize_interleaved_bs` 严格对称。
pub fn dct_dequantize_inverse_interleaved_bs(
    coeffs: &[i32],
    width: usize,
    height: usize,
    components: usize,
    block_w: usize,
    block_h: usize,
) -> Vec<i32> {
    assert!(
        is_valid_rect(block_w, block_h),
        "非法块形状 {}×{}",
        block_w,
        block_h
    );
    if components <= 1 {
        return dct_plane_inverse_bs(coeffs, width, height, block_w, block_h);
    }
    let n = width * height;
    let mut planes: Vec<Vec<i32>> = vec![vec![0i32; n]; components];
    for (px, chunk) in coeffs.chunks_exact(components).enumerate() {
        for (c, &v) in chunk.iter().enumerate() {
            planes[c][px] = v;
        }
    }
    for plane in planes.iter_mut().take(components) {
        *plane = dct_plane_inverse_bs(plane, width, height, block_w, block_h);
    }
    interleave(&planes, components)
}

/// [dct_dequantize_inverse_interleaved_bs] 的 4×4 兼容包装（v1.10 前语义）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_dequantize_inverse_interleaved(
    coeffs: &[i32],
    width: usize,
    height: usize,
    components: usize,
) -> Vec<i32> {
    dct_dequantize_inverse_interleaved_bs(coeffs, width, height, components, 4, 4)
}
