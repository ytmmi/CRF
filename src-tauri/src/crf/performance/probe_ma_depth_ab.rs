//! MA 树深度 A/B 真实编码字节探针（P0-A 实测版）
//!
//! **背景**：`probe_ma_depth` 只测条件熵（下界代理），忽略树头开销——
//! 深度 6 满树头可达 380 字节（vs 深度 3 ≤75 字节），熵降未必换得真实字节降。
//! 本探针按**生产口径**实测每差分帧的真实编码字节：depth-3 基线 vs 更深深度，
//! 并把结果放进整帧三变体竞争（MA/Gradient/Uniform 取最小）中评估。
//!
//! **口径**（每差分帧，与生产路径-G 完全一致）：
//! `diff_rgb = frame − golden`（RGB 域）→ `rct_forward` → RCT 残差，
//! k 与生产相同（`CabacEncoder::adaptive(residuals).k`）。对每个深度 d
//! 调用 `encode_ma_variant`（含树头字节），同时编码 Gradient/Uniform 两个
//! 竞争基线。**主指标是整帧竞争结果** `min(MA_d, G, U)` 而非孤立 MA 字节；
//! MA 单独增量仅作诊断参考。
//!
//! 零码流改动、零外部数据集。CLI：`--probe-ma-depth-ab [root]`。
//! 深度列表可用 `CRF_PROBE_MA_DEPTHS`（默认 `3,4,5,6`）配置。

use crate::crf::core::color::rct;
use crate::crf::encoder::rle_cabac::{encode_cabac_variant, encode_ma_variant, CabacEncoder};
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

fn depth_list() -> Vec<usize> {
    let raw = std::env::var("CRF_PROBE_MA_DEPTHS").unwrap_or_else(|_| "3,4,5,6".to_string());
    let mut v: Vec<usize> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&d| (1..=6).contains(&d))
        .collect();
    v.sort_unstable();
    v.dedup();
    if v.is_empty() {
        v.push(3);
    }
    v
}

fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_MA_DEPTH_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// 单帧各候选的总字节（整帧竞争口径）：
/// `base` = min(MA_d3, G, U)，`best` = min(MA_dN*, G, U)，
/// `ma_bytes` = 各深度 MA 变体单独字节（含树头）。
struct FrameStats {
    base: usize,
    best: usize,
    ma_bytes: Vec<usize>,
}

/// 运行探针。`root` 为 test/png 根目录或单个组目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();
    if load_frames(root).map(|f| f.len() >= 2).unwrap_or(false) {
        groups = vec![std::path::PathBuf::from(root)];
    }

    let depths = depth_list();
    let limit = group_frame_limit();
    let d3_idx = depths.iter().position(|&d| d == 3);
    println!("=== MA 树深度 A/B 真实编码字节探针 ===");
    println!("root: {root}");
    println!(
        "扫描深度: {depths:?}  |  每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: RCT 残差 → 整帧竞争 min(MA_dN, Gradient, Uniform) 真实字节（越低越好）\n");

    // 全局聚合：整帧竞争字节 + 各深度 MA 单独字节
    let mut total_base = 0u64;
    let mut total_best = 0u64;
    let mut total_ma: Vec<u64> = vec![0; depths.len()];
    let mut frames_used = 0u64;

    for dir in &groups {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        if limit > 0 && frames.len() > limit {
            frames.truncate(limit);
        }
        let n = frames.len();
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let golden = &frames[0].pixels;
        let stride = frames[0].width as usize * components;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        let stats: Vec<Option<FrameStats>> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<Option<FrameStats>, String> {
                let frame = &frames[i];
                let mut diff_rgb = vec![0i32; golden.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let residuals =
                    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;
                let k = CabacEncoder::adaptive(&residuals).k();

                let ma_bytes: Vec<usize> = depths
                    .iter()
                    .map(|&d| {
                        encode_ma_variant(&residuals, k, Some(stride), usize::MAX, d)
                            .map(|o| o.map(|(_, total)| total).unwrap_or(usize::MAX))
                            .unwrap_or(usize::MAX)
                    })
                    .collect();
                let grad_total = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 1)
                    .map(|o| o.map(|(_, total)| total).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX);
                let uni_total = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 2)
                    .map(|o| o.map(|(_, total)| total).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX);

                let base = if let Some(&d3) = d3_idx.map(|ix| &ma_bytes[ix]) {
                    d3.min(grad_total).min(uni_total)
                } else {
                    ma_bytes
                        .first()
                        .copied()
                        .unwrap_or(usize::MAX)
                        .min(grad_total)
                        .min(uni_total)
                };
                let best = ma_bytes
                    .iter()
                    .copied()
                    .chain([grad_total, uni_total])
                    .min()
                    .unwrap_or(usize::MAX);
                Ok(Some(FrameStats {
                    base,
                    best,
                    ma_bytes,
                }))
            })
            .collect::<Result<Vec<Option<FrameStats>>, String>>()?;

        let mut g_base = 0u64;
        let mut g_best = 0u64;
        let mut g_ma: Vec<u64> = vec![0; depths.len()];
        let mut used = 0u64;
        for s in stats.iter().flatten() {
            g_base += s.base as u64;
            g_best += s.best as u64;
            for (k, &b) in s.ma_bytes.iter().enumerate() {
                g_ma[k] += b as u64;
            }
            used += 1;
        }
        if used == 0 {
            continue;
        }
        let pct = if g_base == 0 {
            0.0
        } else {
            (g_best as f64 - g_base as f64) / g_base as f64 * 100.0
        };
        print!("组 {:<12} ({} 帧): ", name, used);
        print!("竞争 base={} best={} ({pct:+.2}%) | ", g_base, g_best);
        for (k, &d) in depths.iter().enumerate() {
            let ma_bytes_g = g_ma[k];
            let mp = if g_base == 0 {
                0.0
            } else {
                (ma_bytes_g as f64 - g_base as f64) / g_base as f64 * 100.0
            };
            print!("MA_d{d}={ma_bytes_g}({mp:+.2}%) ");
        }
        println!();
        total_base += g_base;
        total_best += g_best;
        for (k, &b) in g_ma.iter().enumerate() {
            total_ma[k] += b;
        }
        frames_used += used;
    }

    if frames_used == 0 {
        println!("未找到可用图像组（需要 ≥2 帧的 RGB 组）");
        return Ok(());
    }

    println!("\n=== 汇总（整帧竞争真实字节，{} 帧）===", frames_used);
    let pct = if total_base == 0 {
        0.0
    } else {
        (total_best as f64 - total_base as f64) / total_base as f64 * 100.0
    };
    println!("base(MA_d3+G+U 竞争): {total_base} B");
    println!("best(MA_dN+G+U 竞争): {total_best} B  ({pct:+.2}%)");
    for (k, &d) in depths.iter().enumerate() {
        let ma_total = total_ma[k];
        let mp = if total_base == 0 {
            0.0
        } else {
            (ma_total as f64 - total_base as f64) / total_base as f64 * 100.0
        };
        println!("MA 变体单独（诊断） d{d}: {ma_total} B  ({mp:+.2}%)");
    }
    println!(
        "\n判定：更深 MA 深度整帧竞争相对 d3 {:+.2}% —— {}",
        pct,
        if pct <= -3.0 {
            "加深 MA 树有显著收益（≥3%），值得实施"
        } else {
            "加深 MA 树收益有限（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探针口径守卫：生产默认深度的 MA 变体字节必须与生产变体 0 逐位一致。
    #[test]
    fn test_probe_ma_default_depth_equals_variant0() {
        let width = 64usize;
        let height = 48usize;
        let stride = width * 3;
        let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let mut pixels = Vec::with_capacity(stride * height);
        for _ in 0..stride * height {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((state >> 33) as i32 % 64) - 32;
            pixels.push(if state >> 45 & 7 != 0 { 0 } else { v });
        }
        let k = CabacEncoder::adaptive(&pixels).k();
        let d = encode_ma_variant(
            &pixels,
            k,
            Some(stride),
            usize::MAX,
            crate::crf::core::entropy::context::ma_max_depth(),
        )
        .unwrap()
        .map(|(body, total)| (body, total))
        .expect("默认深度 MA 变体应成功");
        let v0 = encode_cabac_variant(&pixels, k, Some(stride), usize::MAX, 0)
            .unwrap()
            .map(|(body, total)| (body, total))
            .expect("生产 MA 变体应成功");
        assert_eq!(d, v0, "默认深度 MA 变体与生产变体 0 字节不一致");
    }
}
