//! 率失真优化量化（RDOQ / Trellis，v1.10 新增）
//!
//! ## 定位
//!
//! 死区标量量化按「最近邻舍入」逐点独立决策；Trellis 在候选量化级中
//! 以「码率 + λ·失真」联合代价择优——小幅值系数在率代价占优时主动
//! 归零（换取更长零游程），大幅值保持精度。对标 x264 Trellis /
//! AV1 optimize_b 思想，适配 CRF 的 RLE+CABAC 熵编码特性。
//!
//! ## 简化设计（工程折衷）
//!
//! - **两状态上下文 DP**：率模型的上下文仅保留「前一系数是否为零」
//!   （游程延续 vs 打断）→ DP 轨道数为 2；每位置 ≤4 候选级，
//!   Viterbi 前向 O(N·4·2)，回溯 O(N)；
//! - **率模型**：与 RLE+CABAC 位流结构同构的经验位长表
//!   （escape 标志 + 符号 + Golomb-Rice 商余），绝对值无需精确，
//!   相对排序正确即可；
//! - **λ = 0.85·Q²**（x264 传统），随位置实际步长 Q_pos 缩放；
//! - **解码端零感知**：输出仍为各位置 Q_pos 倍数的自描述残差，
//!   与 flat/矩阵量化产物同构。

use crate::crf::encoder::dct_path::qm::quantize_coeffs_with_matrix;
use crate::crf::encoder::dct_path::quant_scalar;
use crate::crf::transform::is_valid_rect;

/// λ 系数分子/分母（λ = 850/100 · Q_pos² = 8.5Q²）
///
/// 标定依据：率项单位为 bit×100。λ=8.5Q² 时，「偏移一个量化级
/// （失真 ≥ Q²）换省 1 bit」需 8.5Q² < 100 才成立——即仅 Q≤3 的
/// 低步长档允许小幅值偏移；±1 级内的半级舍入差（≈Q²/4）则可被
/// 游程收益吸收。这恢复了 x264 λ≈0.85Q² 在其率模型下的等效行为。
const LAMBDA_NUM: i64 = 850;
const LAMBDA_DEN: i64 = 100;

/// Golomb-Rice(k=2) 经验位长：商一元码 + 余数固定位
#[inline]
fn rice2_bits(abs_level: u32) -> i64 {
    let q = (abs_level >> 2) as i64;
    q + 1 + 2
}

/// 单系数率估计（bit）：`prev_zero` 为游程上下文
///
/// 与 RLE+CABAC 位流同构：零游程 escape 位 + exp-Golomb 行程；
/// 非零值 escape 位 + 符号 + 商前缀 + 余数。仅需相对排序正确。
#[inline]
fn rate_bits(level: i32, prev_zero: bool) -> i64 {
    if level == 0 {
        if prev_zero {
            0 // 游程延续：exp-Golomb 同桶内边际成本为零
        } else {
            5 // 开启新零游程：escape + 短行程头
        }
    } else if prev_zero {
        4 + rice2_bits(level.unsigned_abs()) // 打断游程：escape + 符号 + 幅值
    } else {
        2 + rice2_bits(level.unsigned_abs()) // 连续非零：符号 + 幅值
    }
}

/// 单块 Trellis DP（Viterbi 三遍式：候选展开 → 前向 → 回溯）
///
/// `block`：块内原始 DCT 系数（行优先）；`qpos`：逐位置有效步长；
/// 返回优化后的量化系数块（各位置 Q_pos 倍数）。
fn trellis_block(block: &[i32], qpos: &[i32]) -> Vec<i32> {
    let n = block.len();

    // Pass 1：逐位候选集 {near−1, near, near+1, 0} 去重 + 失真预计算
    let mut cand_levels: Vec<Vec<i32>> = Vec::with_capacity(n);
    let mut cand_errs: Vec<Vec<i64>> = Vec::with_capacity(n);
    for (p, &orig) in block.iter().enumerate() {
        let qp = qpos[p].max(1);
        let near_f = (orig as f64 / qp as f64).round();
        let near = near_f.clamp(-4096.0, 4096.0) as i32;
        let mut lv_set = vec![near.saturating_sub(1), near, near.saturating_add(1), 0];
        lv_set.sort_unstable();
        lv_set.dedup();
        let errs: Vec<i64> = lv_set
            .iter()
            .map(|&lv| {
                let d = orig as i64 - lv as i64 * qp as i64;
                d * d
            })
            .collect();
        cand_levels.push(lv_set);
        cand_errs.push(errs);
    }

    // Pass 2：前向 Viterbi
    // dp[t]：处理完当前位、当前位归属轨 t（0=非零, 1=零）的最小累积成本；
    // parent[p][t'] 编码 = (胜出候选 idx << 1) | 来源轨。
    // 精度关键：率项统一乘 LAMBDA_DEN 放大（而非 λ 向下取整），
    // 否则 Q=1 时 λ=85/100 整数除法归零、失真项被完全忽略。
    const INF: i64 = i64::MAX / 4;
    let mut dp = [INF, 0i64]; // 虚拟起始位视为零轨（游程延续语义）
    let mut parents: Vec<[u8; 2]> = Vec::with_capacity(n);
    for p in 0..n {
        let _qp = qpos[p].max(1);
        let mut ndp = [INF; 2];
        let mut nparent = [0u8; 2];
        for (ci, &lv) in cand_levels[p].iter().enumerate() {
            let t_new = (lv == 0) as usize;
            for (t_src, &c) in dp.iter().enumerate() {
                if c >= INF {
                    continue;
                }
                let total =
                    c + LAMBDA_NUM * cand_errs[p][ci] + rate_bits(lv, t_src == 1) * LAMBDA_DEN;
                if total < ndp[t_new] {
                    ndp[t_new] = total;
                    nparent[t_new] = ((ci as u8) << 1) | (t_src as u8);
                }
            }
        }
        dp = ndp;
        parents.push(nparent);
    }

    // Pass 3：终点选优 + 回溯还原各级决策
    let mut out = vec![0i32; n];
    let mut t = if dp[0] <= dp[1] { 0usize } else { 1usize };
    for p in (0..n).rev() {
        let code = parents[p][t];
        let ci = (code >> 1) as usize;
        let t_src = (code & 1) as usize;
        out[p] = cand_levels[p][ci] * qpos[p].max(1);
        t = t_src;
    }
    out
}

/// Trellis 量化的整平面入口（v1.12 矩形泛化：`block_w`/`block_h` ∈ {4,8}）
///
/// 完整块走 Trellis DP；边界残缺块沿用基础步长平量化，
/// 与矩阵版量化器的几何透传行为一致。`qm=None` 时退化为 flat 矩阵
/// 量化（权重恒 ×1.0，无 Trellis——flat 场景收益归零，避免白跑 DP）。
#[allow(clippy::too_many_arguments)]
pub fn trellis_quantize_coeffs(
    coeffs: &[i32],
    width: usize,
    height: usize,
    base_q: u8,
    qm: Option<&[u32]>,
    block_w: usize,
    block_h: usize,
) -> Vec<i32> {
    assert!(
        is_valid_rect(block_w, block_h),
        "非法块形状 {}×{}",
        block_w,
        block_h
    );
    // Q=1 近无损保护档：Trellis 直接短路（λ 失真主导下任何偏移都是纯损失，
    // 且与矩阵版量化器的 Q=1 恒等语义对齐）
    if base_q <= 1 {
        return quantize_coeffs_with_matrix(
            coeffs,
            width,
            height,
            base_q,
            &vec![64; block_w * block_h],
            block_w,
            block_h,
            false,
        );
    }
    match qm {
        None => quantize_coeffs_with_matrix(
            coeffs,
            width,
            height,
            base_q,
            &vec![64; block_w * block_h],
            block_w,
            block_h,
            false,
        ),
        Some(table) => {
            let mut out = coeffs.to_vec();
            for by in (0..height).step_by(block_h) {
                for bx in (0..width).step_by(block_w) {
                    let full = by + block_h <= height && bx + block_w <= width;
                    if !full {
                        // 残缺区域：基础步长平量化（几何与其他路径一致）
                        for r in 0..block_h {
                            for c in 0..block_w {
                                let y = by + r;
                                let x = bx + c;
                                if y >= height || x >= width {
                                    continue;
                                }
                                let idx = y * width + x;
                                out[idx] = quant_scalar(coeffs[idx], base_q.max(1) as i32);
                            }
                        }
                        continue;
                    }
                    let bs2 = block_w * block_h;
                    let mut block = vec![0i32; bs2];
                    let mut qpos = vec![1i32; bs2];
                    for r in 0..block_h {
                        for c in 0..block_w {
                            let idx = (by + r) * width + (bx + c);
                            block[r * block_w + c] = coeffs[idx];
                            let w = table[r * block_w + c];
                            qpos[r * block_w + c] = if base_q <= 1 {
                                1
                            } else {
                                ((base_q as u32 * w) / 64).clamp(1, 255) as i32
                            };
                        }
                    }
                    let quantized = trellis_block(&block, &qpos);
                    for r in 0..block_h {
                        for c in 0..block_w {
                            out[(by + r) * width + (bx + c)] = quantized[r * block_w + c];
                        }
                    }
                }
            }
            out
        }
    }
}

/// Trellis 量化的交织数据入口（frame_type=6 第二轮再竞争使用）
///
/// 与 `dct_path::dct_quantize_interleaved_bs(..., use_qm=true)` 的
/// 解包/变换几何完全一致，仅量化环节替换为 Trellis DP。
#[allow(clippy::too_many_arguments)]
pub(crate) fn trellis_quantize_interleaved(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
    block_w: usize,
    block_h: usize,
    qm_table: &[u32],
) -> Vec<i32> {
    let quant_plane = |plane: &[i32]| -> Vec<i32> {
        let coeffs = crate::crf::encoder::dct_path::dct_plane_forward_bs(
            plane, width, height, block_w, block_h,
        );
        trellis_quantize_coeffs(
            &coeffs,
            width,
            height,
            q_step,
            Some(qm_table),
            block_w,
            block_h,
        )
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
    crate::crf::encoder::dct_path::interleave(&planes, components)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q=1 保护档下 Trellis 必须保持恒等（λ 失真主导，任何偏移都劣化）
    #[test]
    fn test_trellis_q1_identity() {
        let coeffs: Vec<i32> = (-500..500).collect();
        let qm = [64u32; 16];
        let out = trellis_quantize_coeffs(&coeffs, 40, 25, 1, Some(&qm), 4, 4);
        assert_eq!(coeffs, out, "Q=1 时 Trellis 应精确还原");
    }

    /// 小幅值稀疏场：Trellis 相对同权重死区量化不应增加总绝对幅值
    /// （小系数倾向归零），且大系数保持原级不被湮灭。
    #[test]
    fn test_trellis_sparsifies_small_coeffs_keeps_large() {
        let mut coeffs = vec![0i32; 256];
        for (i, v) in coeffs.iter_mut().enumerate() {
            *v = match i % 8 {
                0 => 300,
                1 => 3,
                2 => -4,
                3 => 250,
                _ => 0,
            };
        }
        let qm = [64u32; 16];
        let baseline = quantize_coeffs_with_matrix(&coeffs, 16, 16, 5, &qm, 4, 4, false);
        let out = trellis_quantize_coeffs(&coeffs, 16, 16, 5, Some(&qm), 4, 4);
        let abs_sum_base: i64 = baseline.iter().map(|&v| v.unsigned_abs() as i64).sum();
        let abs_sum_out: i64 = out.iter().map(|&v| v.unsigned_abs() as i64).sum();
        assert!(
            abs_sum_out <= abs_sum_base,
            "Trellis 输出幅值总量({})不应超过死区量化基线({})",
            abs_sum_out,
            abs_sum_base
        );
        assert!(out.iter().any(|&v| v.unsigned_abs() > 200));
    }

    /// 输出必须是所在位置步长的倍数（自描述反量化语义不变式）
    #[test]
    fn test_trellis_output_multiple_of_step() {
        let w = 40usize;
        let h = 15usize;
        let coeffs: Vec<i32> = (0..w * h).map(|i| (((i * 7) % 601) as i32) - 300).collect();
        let qm = [
            64u32, 68, 80, 96, 68, 80, 96, 112, 80, 96, 112, 128, 96, 112, 128, 144,
        ];
        let out = trellis_quantize_coeffs(&coeffs, 40, 15, 5, Some(&qm), 4, 4);
        for by in (0..h).step_by(4) {
            for bx in (0..w).step_by(4) {
                // 与实现一致的块级完整性判定：残缺块整块走基础步长
                let full_block = by + 4 <= h && bx + 4 <= w;
                for r in 0..4usize {
                    for c in 0..4usize {
                        let y = by + r;
                        let x = bx + c;
                        if y >= h || x >= w {
                            continue;
                        }
                        let idx = y * w + x;
                        let expected_q = if full_block {
                            let wt = qm[r * 4 + c];
                            (((5u32 * wt) / 64).max(1)) as i32
                        } else {
                            5
                        };
                        if out[idx] % expected_q != 0 {
                            panic!(
                                "位置({},{}) idx={} 输出 {} 非 Q={} 倍数",
                                x, y, idx, out[idx], expected_q
                            );
                        }
                    }
                }
            }
        }
    }
}
