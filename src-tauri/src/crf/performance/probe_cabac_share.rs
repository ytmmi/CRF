//! cabac/MA 变体胜出率诊断探针（P0-A 后续诊断）
//!
//! **背景**：§69/§70 显示 MA 树优化（深度/训练参数）在 CABAC 内显著（−4.35%），
//! 但端到端仅 −0.07~−0.57%——根因是 `encode_frame_adaptive` 的候选链
//! （banded/planar/cabac/dct/intra_transform…）中 MA 所在的 cabac 变体胜出率低。
//! 本探针量化三件事：
//! 1. 差分帧 frame_type 胜出分布（cabac 占比）；
//! 2. cabac 内 MA / Gradient / Uniform 三变体胜出率；
//! 3. cabac 未胜出时与最终胜出者的**字节差距比例**（差距小则 MA 改进可能翻盘）。
//!
//! 口径：每差分帧路径-G 语义（RGB 域 `frame−golden` 后 RCT），无损；
//! cabac 候选按生产语义重建（SATD 最优模式 + 开环预测 + 三变体竞争）。
//! 零码流改动。CLI：`--probe-cabac-share [root]`。
//! `CRF_PROBE_ALL_FRAMES=1` 覆盖全部差分帧（默认每组前 2 帧）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData, PredictionMode};
use crate::crf::core::entropy::context::ma_max_depth;
use crate::crf::core::prediction::cost::satd_for_mode_sampled;
use crate::crf::core::prediction::intra::apply_prediction_into;
use crate::crf::encoder::frame::candidate::{encode_frame_adaptive, ADAPTIVE_CANDIDATES};
use crate::crf::encoder::frame::{assemble_frame, FrameQuant};
use crate::crf::encoder::rle_cabac::{
    encode_cabac_variant, encode_frame_rle_cabac_adaptive_limited, encode_ma_variant, CabacEncoder,
};
use crate::crf::performance::bench::load_frames;

/// frame_type → 名称（与解码端 dispatcher 一致）
fn frame_type_name(t: u8) -> &'static str {
    match t {
        0 => "golomb-block",
        1 => "rle",
        2 => "banded",
        3 => "planar",
        4 => "palette",
        5 => "cabac",
        6 => "dct",
        7 => "intrabc",
        8 => "intra_transform",
        _ => "unknown",
    }
}

/// 运行诊断探针。`root` 为 test/png 根目录或单个组目录。
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

    const MAX_TYPES: usize = 9;
    let mut wins = [0usize; MAX_TYPES];
    let mut total = 0usize;
    let mut cabac_frames = 0usize;
    let mut cabac_wins = 0usize;
    let mut ma_wins = 0usize;
    let mut g_wins = 0usize;
    let mut u_wins = 0usize;
    let mut ratios: Vec<f64> = Vec::new();

    println!("=== cabac/MA 变体胜出率诊断探针 ===");
    println!("root: {root}\n");

    let all_frames = std::env::var("CRF_PROBE_ALL_FRAMES")
        .map(|v| v == "1")
        .unwrap_or(false);
    let take = if all_frames { usize::MAX } else { 2 };

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let golden = &frames[0].pixels;
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let width = frames[0].width as usize;
        let height = frames[0].height as usize;
        let stride = width * components;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        for frame in frames.iter().skip(1).take(take) {
            // 路径 G 差分帧语义：RGB 域 diff(frame − golden) 后 RCT。
            let mut diff_rgb = vec![0i32; golden.len()];
            crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
            let diff = rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;
            let eff = ImageData {
                width: width as u16,
                height: height as u16,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels: diff,
            };

            // SATD 排名 → 最优模式（与生产 encode_frame_adaptive 一致）
            let mut ranked: Vec<(u64, PredictionMode)> = ADAPTIVE_CANDIDATES
                .iter()
                .map(|&m| {
                    (
                        satd_for_mode_sampled(&eff.pixels, width, height, components, m),
                        m,
                    )
                })
                .collect();
            ranked.sort_by_key(|&(s, _)| s);
            let best_mode = ranked[0].1;

            // 生产 cabac 预筛（P6）：avg_abs_res >= 0.5 才运行 cabac 候选
            let avg_abs_res = crate::crf::core::prediction::cost::residual_activity_for_mode_sampled(
                &eff.pixels,
                width,
                height,
                components,
                best_mode,
            );
            let cabac_runs = avg_abs_res >= 0.5;

            // 最终多候选竞争结果
            let out = encode_frame_adaptive(
                &eff,
                CompressionType::GolombRice,
                8,
                false,
                FrameQuant::lossless(),
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
            let ft = out.data.get(8).copied().unwrap_or(0) as usize;
            let final_len = out.data.len();
            if ft < MAX_TYPES {
                wins[ft] += 1;
            }

            if cabac_runs {
                // 开环预测残差（无损路径）
                let mut residuals = vec![0i32; eff.pixels.len()];
                apply_prediction_into(
                    &eff.pixels,
                    &mut residuals,
                    width,
                    height,
                    components,
                    best_mode,
                );
                let k = CabacEncoder::adaptive(&residuals).k();

                // cabac 内三变体字节（body total，含 flags/树头）
                let ma = encode_ma_variant(&residuals, k, Some(stride), usize::MAX, ma_max_depth())
                    .map_err(|e| e.to_string())?
                    .map(|(_, t)| t);
                let g = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 1)
                    .map_err(|e| e.to_string())?
                    .map(|(_, t)| t);
                let u = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 2)
                    .map_err(|e| e.to_string())?
                    .map(|(_, t)| t);
                if let (Some(ma), Some(g), Some(u)) = (ma, g, u) {
                    let m = ma.min(g).min(u);
                    if m == ma {
                        ma_wins += 1;
                    } else if m == g {
                        g_wins += 1;
                    } else {
                        u_wins += 1;
                    }
                }

                // cabac 完整帧（帧头 + k + body），与最终胜出者同口径
                let (payload, k2) =
                    encode_frame_rle_cabac_adaptive_limited(&residuals, Some(stride), usize::MAX)
                        .map_err(|e| e.to_string())?
                        .ok_or("cabac payload 为空")?;
                let mut full = Vec::with_capacity(payload.len() + 1);
                full.push(k2);
                full.extend_from_slice(&payload);
                let cabac_frame = assemble_frame(&full, &eff, k2, 5).map_err(|e| e.to_string())?;
                let cabac_len = cabac_frame.len();
                cabac_frames += 1;
                if ft == 5 {
                    cabac_wins += 1;
                } else if final_len > 0 {
                    ratios.push(cabac_len as f64 / final_len as f64);
                }
            }
            total += 1;
        }
    }

    println!("--- 差分帧 frame_type 胜出分布（共 {total} 帧）---");
    for (t, &w) in wins.iter().enumerate().take(MAX_TYPES) {
        if w > 0 {
            println!(
                "  {:<15} {:>3}  ({:>5.1}%)",
                frame_type_name(t as u8),
                w,
                w as f64 / total.max(1) as f64 * 100.0
            );
        }
    }

    println!("\n--- cabac 内三变体胜出（{cabac_frames} 次 cabac 候选）---");
    let denom = (ma_wins + g_wins + u_wins).max(1);
    println!(
        "  MA {} ({:.1}%) / Gradient {} ({:.1}%) / Uniform {} ({:.1}%)",
        ma_wins,
        ma_wins as f64 / denom as f64 * 100.0,
        g_wins,
        g_wins as f64 / denom as f64 * 100.0,
        u_wins,
        u_wins as f64 / denom as f64 * 100.0
    );

    println!("\n--- cabac 未胜出时与最终胜出者的字节差距（cabac_len / final_len）---");
    if ratios.is_empty() {
        println!("  （cabac 全部胜出或无数据）");
    } else {
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = ratios.len();
        let (min, med, max) = (ratios[0], ratios[n / 2], ratios[n - 1]);
        let cnt = |th: f64| ratios.iter().filter(|&&r| r <= th).count();
        println!("  {n} 帧未胜出：min {min:.3} / median {med:.3} / max {max:.3}");
        println!(
            "  差距 ≤1.02: {} ({:.0}%) | ≤1.05: {} ({:.0}%) | ≤1.10: {} ({:.0}%) | ≤1.20: {} ({:.0}%)",
            cnt(1.02),
            cnt(1.02) as f64 / n as f64 * 100.0,
            cnt(1.05),
            cnt(1.05) as f64 / n as f64 * 100.0,
            cnt(1.10),
            cnt(1.10) as f64 / n as f64 * 100.0,
            cnt(1.20),
            cnt(1.20) as f64 / n as f64 * 100.0
        );
    }

    println!("\n--- 诊断 ---");
    println!(
        "  cabac 胜出率 {:.1}%（{cabac_wins} / {total}）",
        cabac_wins as f64 / total.max(1) as f64 * 100.0
    );
    println!(
        "  解读：cabac 未胜出差距若普遍 >1.10，MA 改进难翻盘；若集中在 ≤1.05，\n  则 MA/上下文改进有翻盘空间（提升 cabac 胜出率即可放大端到端收益）。"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_type_names_cover_all() {
        assert_eq!(frame_type_name(5), "cabac");
        assert_eq!(frame_type_name(2), "banded");
        assert_eq!(frame_type_name(8), "intra_transform");
    }
}
