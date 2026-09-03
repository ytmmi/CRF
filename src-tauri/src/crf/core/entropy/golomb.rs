//! Golomb-Rice 参数 k 的自适应选择（规划文档 §3.7 golomb.rs）
//!
//! 定义 Golomb-Rice/Exp-Golomb 的 k 选择语法：基于值分布的
//! [`adaptive_k`]/[`block_adaptive_k`] 估计，以及基于非零值直方图精确竞争的
//! [`best_k_by_histogram`]。三者均为纯数学，编解码端共享同一「k 选择契约」。

use super::scan::zigzag_encode;

/// 自适应选择 Golomb-Rice 参数 k
///
/// 针对二次元插画差分图优化：
/// - 零值占比高（~70%），绝对值≤1占比高（~80%）
/// - zigzag编码后，大部分值集中在0-2
/// - 使用中位数+众数联合估算，而非仅均值
pub fn adaptive_k(values: &[u32]) -> u8 {
    if values.is_empty() {
        return 0;
    }

    // 估算几何分布参数 p = 2^(-k)，最优 k = ceil(-log2(p))
    // 对于Golomb-Rice，最优k ≈ floor(log2(mean))
    // 但二次元插画差分图的zigzag值分布高度偏斜，均值会偏大

    // 方法1：基于均值
    let sum: u64 = values.iter().map(|&v| v as u64).sum();
    let mean = sum as f64 / values.len() as f64;

    // 方法2：基于零值比例估算
    // 如果零值占比 > 50%，则最优k很小
    let zero_count = values.iter().filter(|&&v| v == 0).count();
    let zero_ratio = zero_count as f64 / values.len() as f64;

    // 方法3：基于中位数
    // 中位数比均值更鲁棒，适合有少量大值的分布
    let mut sorted = values.to_vec();
    let mid = sorted.len() / 2;
    sorted.select_nth_unstable(mid);
    let median = sorted[mid] as f64;

    // 综合策略：
    // - 零值占比高时，使用更小的k（甚至k=0）
    // - 否则使用中位数估算
    if zero_ratio > 0.6 {
        // 零值占比超过60%，使用k=0（一元编码）
        // 这对二次元插画差分图是最优的
        0
    } else if median <= 1.0 {
        0
    } else if median <= 2.0 {
        1
    } else if median <= 4.0 {
        2
    } else {
        // 使用均值估算
        if mean == 0.0 {
            0
        } else {
            (mean.log2().floor() as u8).min(16)
        }
    }
}

/// 块级自适应 Golomb-Rice 参数 k
///
/// 将图像分成 block_size x block_size 的块，为每个块选择最优 k
/// 适用于原始帧（高熵）的压缩优化
pub fn block_adaptive_k(
    values: &[u32],
    width: usize,
    height: usize,
    components: usize,
    block_size: usize,
) -> Vec<u8> {
    let mut k_values = Vec::new();

    for by in (0..height).step_by(block_size) {
        for bx in (0..width).step_by(block_size) {
            let mut block_values = Vec::new();

            // 收集块内所有像素值
            for y in by..std::cmp::min(by + block_size, height) {
                for x in bx..std::cmp::min(bx + block_size, width) {
                    for c in 0..components {
                        let idx = (y * width + x) * components + c;
                        if idx < values.len() {
                            block_values.push(values[idx]);
                        }
                    }
                }
            }

            // 为该块计算最优 k
            let k = adaptive_k(&block_values);
            k_values.push(k);
        }
    }

    k_values
}

/// 基于非零值直方图的多 k 精确竞争
///
/// 单遍 O(N) 统计非零 zigzag 值直方图（零行程不走 Golomb，不影响 k 选择），
/// 对每个候选 k 精确计算非零值 Golomb 总位长 Σ(⌊u/2^k⌋ + 1 + k)，取最小者。
/// 相比单一 abs-mean 估计，可修正残差分布非对称时的次优偏差。
///
/// 候选集：k ∈ {0..=6}（覆盖 |v| ≤ 255 的全部合理区间；位深更高时
/// 大幅值由 exp-Golomb 行程/调色板等路径承接）。
///
/// P4 已从 `encoder/rle_golomb.rs` 迁入（原 `pub(crate) fn best_k_by_histogram`）。
pub fn best_k_by_histogram(values: &[i32]) -> u8 {
    use std::collections::HashMap;

    // 单遍统计非零 zigzag 值直方图
    let mut hist: HashMap<u32, u64> = HashMap::new();
    let mut nonzero: u64 = 0;
    for &v in values {
        if v == 0 {
            continue; // 零走 RLE 行程，与 k 无关
        }
        *hist.entry(zigzag_encode(v)).or_insert(0) += 1;
        nonzero += 1;
    }
    if nonzero == 0 {
        return 0;
    }

    let mut best_k = 0u8;
    let mut best_len = u64::MAX;
    for k in 0u8..=6 {
        let ku = k as u64;
        let mut total: u64 = 0;
        for (&u, &cnt) in &hist {
            // Golomb-Rice(k) 位长：商 q=⌊u/2^k⌋ 个 '1' + 终止 '0' + k 位余数
            let uu = u as u64;
            total += cnt * ((uu >> ku) + 1 + ku);
        }
        if total < best_len {
            best_len = total;
            best_k = k;
        }
    }
    best_k
}
