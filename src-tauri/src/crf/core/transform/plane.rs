//! 分块 DCT 正变换（矩形形状，规划文档 §3.6）
//!
//! P4 已从 `encoder/dct_path/mod.rs` 迁入。对整帧平面执行分块 lifting DCT
//! 正变换，块形状 {4×4, 8×8, 8×4, 4×8}；右/下边界残缺块原样透传。
//! 正逆两端共用同一几何判定，确保逐块对称（逆变换见 [`super::reconstruct`]）。

use crate::crf::core::transform::{
    dct4x4_forward_into, dct8x8_forward_into, dct_rect_forward_into, is_valid_rect,
};

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
    transform: impl Fn(&[i32], &mut [i32]),
) {
    let n = bw * bh;
    let mut block = [0i32; 64];
    for r in 0..bh {
        for c in 0..bw {
            block[r * bw + c] = src[(by + r) * width + (bx + c)];
        }
    }
    let mut transformed = [0i32; 64];
    transform(&block[..n], &mut transformed[..n]);
    for r in 0..bh {
        for c in 0..bw {
            dst[(by + r) * width + (bx + c)] = transformed[r * bw + c];
        }
    }
}

/// 对整帧平面执行分块 DCT 正变换（矩形形状：bw/bh ∈ {4, 8}）
///
/// 仅完整块参与变换；右/下边界的残缺块原样透传——
/// 残缺块若强行 clamp 填充，正向会丢弃虚拟位置的系数而逆向却
/// 读回错误数据，破坏可逆性（历史缺陷：非对齐尺寸边界列/行必损）。
/// 正逆两端用同一几何判定，确保逐块对称。
pub fn dct_plane_forward_bs(
    pixels: &[i32],
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
    let mut out = pixels.to_vec();
    for by in (0..height).step_by(block_h) {
        for bx in (0..width).step_by(block_w) {
            if by + block_h > height || bx + block_w > width {
                continue; // 残缺块透传
            }
            let kernel = |blk: &[i32], dst: &mut [i32]| match (block_w, block_h) {
                (8, 8) => dct8x8_forward_into(blk, dst),
                (8, _) => dct_rect_forward_into(blk, dst, 8, 4),
                _ => match block_h {
                    8 => dct_rect_forward_into(blk, dst, 4, 8),
                    _ => dct4x4_forward_into(blk, dst),
                },
            };
            transform_block(pixels, &mut out, width, bx, by, block_w, block_h, kernel);
        }
    }
    out
}

/// 对整帧平面执行 4×4 分块 DCT 正变换（v1.10 前语义的兼容包装）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn dct_plane_forward(pixels: &[i32], width: usize, height: usize) -> Vec<i32> {
    dct_plane_forward_bs(pixels, width, height, 4, 4)
}
