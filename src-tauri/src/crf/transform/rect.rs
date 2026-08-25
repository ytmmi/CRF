//! 矩形变换核（4×8 / 8×4，v1.12 新增）
//!
//! ## 定位
//!
//! 二次元插画的纹理常具**非对称方向性**：水平发丝（水平频率细、垂直粗）
//! 宜用宽块（w=8,h=4），垂直裙褶反之。矩形变换通过「行/列采用不同点数
//! 的可逆核」提供方向性频率分辨率。
//!
//! ## 构造
//!
//! 二维 DCT 的行列分离结构天然支持矩形：`w×h 块 = 行过 w 点一维核 →
//! 列过 h 点一维核`。一维核完全复用既有的 dct4/dct8 lifting 结构
//! （严格可逆），因此矩形的可逆性由构造直接保证。
//!
//! 频率槽位语义：输出按行优先排列，行索引对应**垂直频率**（由列变换
//! 的 h 点核决定）、列索引对应**水平频率**（由行变换的 w 点核决定）。

use super::dct4::{dct4_fwd, dct4_inv};
use super::dct8::{dct8_fwd, dct8_inv};

/// 矩形形状合法性检查
#[inline]
pub fn is_valid_rect(bw: usize, bh: usize) -> bool {
    matches!((bw, bh), (4, 4) | (8, 8) | (8, 4) | (4, 8))
}

/// 二维矩形 DCT 正变换：行过 `bw` 点核、列过 `bh` 点核
///
/// `block` 为行优先 `bw*bh` 元素；输出同布局。
pub fn dct_rect_forward(block: &[i32], bw: usize, bh: usize) -> Vec<i32> {
    debug_assert!(is_valid_rect(bw, bh));
    let n = bw * bh;
    let mut tmp = vec![0i32; n];

    // 行变换（水平频率，bw 点一维核）
    for r in 0..bh {
        let base = r * bw;
        if bw == 8 {
            let (y0, y1, y2, y3, y4, y5, y6, y7) = dct8_fwd(
                block[base],
                block[base + 1],
                block[base + 2],
                block[base + 3],
                block[base + 4],
                block[base + 5],
                block[base + 6],
                block[base + 7],
            );
            tmp[base..base + 8].copy_from_slice(&[y0, y1, y2, y3, y4, y5, y6, y7]);
        } else {
            let (y0, y1, y2, y3) = dct4_fwd(
                block[base],
                block[base + 1],
                block[base + 2],
                block[base + 3],
            );
            tmp[base..base + 4].copy_from_slice(&[y0, y1, y2, y3]);
        }
    }

    // 列变换（垂直频率，bh 点一维核）：逐列收集→变换→写回
    let mut out = vec![0i32; n];
    for c in 0..bw {
        if bh == 8 {
            let col: [i32; 8] = [
                tmp[c],
                tmp[bw + c],
                tmp[2 * bw + c],
                tmp[3 * bw + c],
                tmp[4 * bw + c],
                tmp[5 * bw + c],
                tmp[6 * bw + c],
                tmp[7 * bw + c],
            ];
            let (y0, y1, y2, y3, y4, y5, y6, y7) = dct8_fwd(
                col[0], col[1], col[2], col[3], col[4], col[5], col[6], col[7],
            );
            out[c] = y0;
            out[bw + c] = y1;
            out[2 * bw + c] = y2;
            out[3 * bw + c] = y3;
            out[4 * bw + c] = y4;
            out[5 * bw + c] = y5;
            out[6 * bw + c] = y6;
            out[7 * bw + c] = y7;
        } else {
            let (y0, y1, y2, y3) = dct4_fwd(tmp[c], tmp[bw + c], tmp[2 * bw + c], tmp[3 * bw + c]);
            out[c] = y0;
            out[bw + c] = y1;
            out[2 * bw + c] = y2;
            out[3 * bw + c] = y3;
        }
    }
    out
}

/// 二维矩形 DCT 逆变换（列逆 → 行逆，与正变换严格互逆）
pub fn dct_rect_inverse(block: &[i32], bw: usize, bh: usize) -> Vec<i32> {
    debug_assert!(is_valid_rect(bw, bh));
    let n = bw * bh;
    let mut tmp = vec![0i32; n];

    // 列逆变换（撤销后执行的列正变换）
    for c in 0..bw {
        if bh == 8 {
            let (x0, x1, x2, x3, x4, x5, x6, x7) = dct8_inv(
                block[c],
                block[bw + c],
                block[2 * bw + c],
                block[3 * bw + c],
                block[4 * bw + c],
                block[5 * bw + c],
                block[6 * bw + c],
                block[7 * bw + c],
            );
            for (r, v) in [x0, x1, x2, x3, x4, x5, x6, x7].iter().enumerate() {
                tmp[r * bw + c] = *v;
            }
        } else {
            let (x0, x1, x2, x3) = dct4_inv(
                block[c],
                block[bw + c],
                block[2 * bw + c],
                block[3 * bw + c],
            );
            for (r, v) in [x0, x1, x2, x3].iter().enumerate() {
                tmp[r * bw + c] = *v;
            }
        }
    }

    // 行逆变换
    let mut out = vec![0i32; n];
    for r in 0..bh {
        let base = r * bw;
        if bw == 8 {
            let (x0, x1, x2, x3, x4, x5, x6, x7) = dct8_inv(
                tmp[base],
                tmp[base + 1],
                tmp[base + 2],
                tmp[base + 3],
                tmp[base + 4],
                tmp[base + 5],
                tmp[base + 6],
                tmp[base + 7],
            );
            out[base..base + 8].copy_from_slice(&[x0, x1, x2, x3, x4, x5, x6, x7]);
        } else {
            let (x0, x1, x2, x3) = dct4_inv(tmp[base], tmp[base + 1], tmp[base + 2], tmp[base + 3]);
            out[base..base + 4].copy_from_slice(&[x0, x1, x2, x3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随机数据往返：两种矩形 × 500 块逐位一致
    #[test]
    fn test_rect_roundtrip_random() {
        let mut state = 0xFEED_FACEu64;
        for (bw, bh) in [(8usize, 4usize), (4usize, 8usize)] {
            for _ in 0..500 {
                let mut block = vec![0i32; bw * bh];
                for v in block.iter_mut() {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    *v = ((state >> 33) as i32 % 512) - 256;
                }
                let restored = dct_rect_inverse(&dct_rect_forward(&block, bw, bh), bw, bh);
                assert_eq!(block, restored, "{bw}×{bh} 往返失败");
            }
        }
    }

    /// 能量集中性：水平渐变在宽块（w=8）下的水平低频应更集中；
    /// 至少验证 DC 主导这一基本性质对两种矩形都成立。
    #[test]
    fn test_rect_energy_compaction() {
        for (bw, bh) in [(8usize, 4usize), (4usize, 8usize)] {
            let smooth: Vec<i32> = (0..bw * bh)
                .map(|i| {
                    let x = i % bw;
                    let y = i / bw;
                    (40 + x * 3 + y / 2) as i32
                })
                .collect();
            let coeffs = dct_rect_forward(&smooth, bw, bh);
            let dc = coeffs[0].unsigned_abs() as u64;
            let hf_mean: u64 = (1..coeffs.len())
                .map(|i| coeffs[i].unsigned_abs() as u64)
                .sum::<u64>()
                / (coeffs.len() as u64 - 1);
            assert!(
                dc > hf_mean * 5,
                "{bw}×{bh} DC({dc}) 应显著大于高频均值({hf_mean})"
            );
        }
    }
}
