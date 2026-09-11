//! ringing 控制信号探针（P4.7 前置验证，S0）
//!
//! 背景：V2 `perceptual.ringingControlX100` 字段已定义但无内核消费。本探针
//! 回答 P4.7 是否值得投入新信号——ringing 的物理机制是高对比边缘附近大步长
//! 量化后重建帧的 Gibbs 振荡，风险区是**边缘的邻域扩散区**而非边缘本身。
//!
//! 已有 P4.4 `edge_protection` 用「稀疏大梯度」（nz_avg > 2·全帧参考）分类
//! 边缘条带并减步长。若 ringing 风险区（Laplacian 高响应条带）几乎全部落在
//! edge 条带或其 ±1 邻域内，则 ringing 不是独立信号，`ringing_control` 的
//! 正确落点是「edge 邻域收窄」扩展或直接证伪；若 lap_high 条带大量独立分布，
//! ringing 才值得作为第四分类新信号投入。
//!
//! 方法：对 golden 差分帧（frame[i] − frame[0]，与路径 G 一致）做 RCT，逐
//! BAND_HEIGHT 条带统计：
//! - grad_avg / nz_avg（与 [`estimate_band_activity_steps`] 相同的梯度统计）
//!   → edge 分类判定（nz_avg > 2·reference，reference = 全帧条带均值）；
//! - lap_p90：Y 分量 8 邻域 Laplacian 响应绝对值的 P90 分位。
//!
//! 纯统计探针，不调编码器、不改产物，仅由 `--probe-ringing <dir>` CLI 分派。

use crate::crf::core::bitstream::constants::BAND_HEIGHT;
use crate::crf::core::color::rct;
use crate::crf::performance::bench::load_frames;

/// 单条带的 Y 分量 8 邻域 Laplacian 响应绝对值 P90 分位。
///
/// 核与 [`crate::crf::core::perceptual::noise::laplacian_percentile_diag`]
/// 同式：`resp = 4c + tl+tr+bl+br − 2(t+b+l+r)`；仅统计条带内部像素
/// （首尾行让给邻域读取，边界安全）。
fn band_laplacian_p90(ycocg: &[i32], width: usize, height: usize, band: usize) -> u32 {
    let y0 = band * BAND_HEIGHT;
    let y1 = (y0 + BAND_HEIGHT).min(height);
    if width < 3 || y1 <= y0 + 2 {
        return 0;
    }
    let mut hist = [0u32; 4096];
    let mut count = 0u32;
    for y in (y0 + 1)..(y1 - 1) {
        let row = &ycocg[y * width * 3..(y + 1) * width * 3];
        let row_up = &ycocg[(y - 1) * width * 3..y * width * 3];
        let row_dn = &ycocg[(y + 1) * width * 3..(y + 2) * width * 3];
        for x in 1..(width - 1) {
            let c = row[x * 3] as i64;
            let t = row_up[x * 3] as i64;
            let b = row_dn[x * 3] as i64;
            let l = row[(x - 1) * 3] as i64;
            let r = row[(x + 1) * 3] as i64;
            let tl = row_up[(x - 1) * 3] as i64;
            let tr = row_up[(x + 1) * 3] as i64;
            let bl = row_dn[(x - 1) * 3] as i64;
            let br = row_dn[(x + 1) * 3] as i64;
            let resp = 4 * c + tl + tr + bl + br - 2 * (t + b + l + r);
            hist[resp.unsigned_abs().min(4095) as usize] += 1;
            count += 1;
        }
    }
    let target = count / 10 * 9; // P90
    let mut acc = 0u32;
    for (bi, &cnt) in hist.iter().enumerate() {
        acc += cnt;
        if acc > target {
            return bi as u32;
        }
    }
    0
}

/// 单帧的逐条带统计：返回 (grad_avg, nz_avg, lap_p90) 三元组数组。
fn frame_band_stats(ycocg: &[i32], width: usize, height: usize) -> Vec<(u64, u64, u32)> {
    let bands = height.div_ceil(BAND_HEIGHT).max(1);
    let mut grad_sum = vec![0u64; bands];
    let mut grad_cnt = vec![0u64; bands];
    let mut non_zero_cnt = vec![0u64; bands];
    let stride = width * 3;
    for (y, row) in ycocg.chunks_exact(stride).enumerate() {
        let band = (y / BAND_HEIGHT).min(bands - 1);
        for x in 1..width {
            let g = row[x * 3].wrapping_sub(row[(x - 1) * 3]).unsigned_abs() as u64;
            grad_sum[band] += g;
            grad_cnt[band] += 1;
            if g > 0 {
                non_zero_cnt[band] += 1;
            }
        }
    }
    let mut out = Vec::with_capacity(bands);
    for band in 0..bands {
        let avg = grad_sum[band].checked_div(grad_cnt[band]).unwrap_or(0);
        let nz_avg = grad_sum[band].checked_div(non_zero_cnt[band]).unwrap_or(0);
        let lap = band_laplacian_p90(ycocg, width, height, band);
        out.push((avg, nz_avg, lap));
    }
    out
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.len() < 2 {
        return Err(format!("{dir}: 至少需要 2 帧"));
    }
    let width = frames[0].width as usize;
    let height = frames[0].height as usize;
    let bands = height.div_ceil(BAND_HEIGHT).max(1);
    println!("=== ringing signal probe (S0) ===");
    println!(
        "group: {dir}  ({}x{}, {} 帧, {bands} 条带/帧)\n",
        width,
        height,
        frames.len()
    );

    // 收集全部差分帧的条带统计，供全组 lap_p90 中位数参考
    let mut per_frame: Vec<Vec<(u64, u64, u32)>> = Vec::with_capacity(frames.len() - 1);
    let mut all_laps: Vec<u32> = Vec::new();
    for i in 1..frames.len() {
        let diff: Vec<i32> = frames[i]
            .pixels
            .iter()
            .zip(&frames[0].pixels)
            .map(|(a, b)| a - b)
            .collect();
        let ycocg = rct::rct_forward(&diff, 3).map_err(|e| e.to_string())?;
        let stats = frame_band_stats(&ycocg, width, height);
        all_laps.extend(stats.iter().map(|&(_, _, lap)| lap));
        per_frame.push(stats);
    }
    all_laps.sort_unstable();
    let lap_median = all_laps[all_laps.len() / 2];
    let lap_threshold = (lap_median.saturating_mul(2)).max(8);
    println!("lap_p90 全组中位数 = {lap_median}，lap_high 阈值 = {lap_threshold}\n");

    let mut total_bands = 0usize;
    let mut edge_bands = 0usize;
    let mut edge_f_bands = 0usize;
    let mut lap_high_bands = 0usize;
    let mut lap_high_on_edge = 0usize; // lap_high 条带本身是 edge（f64）
    let mut lap_high_in_edge_neighborhood = 0usize; // lap_high 落在 edge ±1 邻域（f64）
    let mut lap_high_outside = 0usize; // lap_high 完全独立分布

    println!("frame  band  grad_avg  nz_avg   ref   edge  edge_f64  lap_p90  lap_high");
    for (fi, stats) in per_frame.iter().enumerate() {
        // 全帧参考：production 语义（§28 修复后 = (Σavg/bands).max(1)，
        // 与 estimate_band_activity_steps 逐式一致）与 f64 参考并存，
        // 以对照「修复后分类激活」与「信号独立性」两个问题。
        let sum: u64 = stats.iter().map(|&(avg, _, _)| avg).sum();
        let reference = (sum / (stats.len() as u64).max(1)).max(1);
        let reference_f = sum as f64 / (stats.len() as f64).max(1.0);
        for (band, &(avg, nz_avg, lap)) in stats.iter().enumerate() {
            let edge = reference > 0 && nz_avg > reference.saturating_mul(2);
            let edge_f = reference_f > 0.0 && (nz_avg as f64) > reference_f * 2.0;
            let lap_high = lap >= lap_threshold;
            total_bands += 1;
            if edge {
                edge_bands += 1;
            }
            if edge_f {
                edge_f_bands += 1;
            }
            if lap_high {
                lap_high_bands += 1;
                if edge {
                    lap_high_on_edge += 1;
                }
                // 邻域判定用 production 修复后语义（reference ≥ 1）
                let is_edge_at =
                    |idx: usize| -> bool { stats[idx].1 > reference.saturating_mul(2) };
                let prev_edge = band > 0 && is_edge_at(band - 1);
                let next_edge = band + 1 < stats.len() && is_edge_at(band + 1);
                if edge || prev_edge || next_edge {
                    lap_high_in_edge_neighborhood += 1;
                } else {
                    lap_high_outside += 1;
                }
            }
            println!(
                "  {:>3}  {:>4}  {:>8}  {:>6}  {:>4}  {:>5}  {:>7}  {:>7}  {:>8}",
                fi + 1,
                band,
                avg,
                nz_avg,
                if reference > 0 {
                    reference.to_string()
                } else {
                    "-".into()
                },
                if edge { "YES" } else { "-" },
                if edge_f { "YES" } else { "-" },
                lap,
                if lap_high { "HIGH" } else { "-" },
            );
        }
    }

    println!("\n--- 汇总 ---");
    println!("条带总数: {total_bands}");
    println!(
        "edge 条带（production §28 修复后语义）: {edge_bands}  ({:.1}%)",
        edge_bands as f64 / total_bands as f64 * 100.0
    );
    println!(
        "edge 条带（f64 参考对照）: {edge_f_bands}  ({:.1}%)",
        edge_f_bands as f64 / total_bands as f64 * 100.0
    );
    println!(
        "lap_high 条带: {lap_high_bands}  ({:.1}%)",
        lap_high_bands as f64 / total_bands as f64 * 100.0
    );
    if lap_high_bands > 0 {
        println!(
            "  ├─ 本身是 edge（production 语义）: {lap_high_on_edge}  ({:.1}%)",
            lap_high_on_edge as f64 / lap_high_bands as f64 * 100.0
        );
        println!(
            "  ├─ 落在 edge ±1 邻域（production 语义）: {lap_high_in_edge_neighborhood}  ({:.1}%)",
            lap_high_in_edge_neighborhood as f64 / lap_high_bands as f64 * 100.0
        );
        println!(
            "  └─ 独立分布（edge 邻域之外）: {lap_high_outside}  ({:.1}%)",
            lap_high_outside as f64 / lap_high_bands as f64 * 100.0
        );
    }
    println!("\n判定参考：");
    println!("  production 语义（§28 修复后）edge 应激活（>0）——验证 reference 修复生效；");
    println!("  lap_high 独立占比高 ⟹ ringing 是独立信号，值得第四分类；");
    println!("  lap_high 几乎全在 edge 邻域 ⟹ ringing_control 落点为 edge 邻域扩展或证伪。");
    Ok(())
}
