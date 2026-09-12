//! banded 条带级熵编码升级诊断探针（P0-C 前置）
//!
//! **背景**：§71 诊断显示差分帧最大胜出者是 banded（38.6%），但其熵编码为
//! **RLE+Golomb**（较弱），而 cabac 变体用自适应算术编码（CABAC，强熵编码）
//! 却受限于**帧级单一预测模式**。本探针量化「banded 的条带级模式选择 +
//! CABAC 熵编码」相对现状（条带级模式 + RLE+Golomb）的字节潜力：
//! 若条带残差改用 CABAC 显著更小，则「条带级模式 + CABAC」是 banded 的升级方向。
//!
//! 口径（每差分帧，路径-G 语义：RGB 域 `frame−golden` 后 RCT，无损）：
//! 对条带高度 {32, 64}，逐条带做 8 候选精确 SAD 排序，取 top-2 模式，
//! 分别用 RLE+Golomb 与 CABAC 编码残差流，各自取最小后累加（含条带头 6B/条带
//! + band_count 2B）。对比 `banded_rle` vs `banded_cabac` 总字节。
//!
//! 零码流改动。CLI：`--probe-banded-cabac [root]`。
//! `CRF_PROBE_ALL_FRAMES=1` 覆盖全部差分帧（默认每组前 2 帧）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::apply_prediction_band_into;
use crate::crf::encoder::frame::candidate::ADAPTIVE_CANDIDATES;
use crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive;
use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;
use crate::crf::performance::bench::load_frames;

const BAND_HEIGHTS: [usize; 2] = [32, 64];

/// 对单帧、指定条带高度，返回 `(rle_total, cabac_total)`（含条带头）。
fn banded_entropy_totals(
    pixels: &[i32],
    width: usize,
    components: usize,
    band_height: usize,
) -> Result<(usize, usize), String> {
    let height = pixels.len() / (width * components);
    let band_count = height.div_ceil(band_height);
    let stride = width * components;
    let mut rle_total = 2usize; // band_count u16
    let mut cabac_total = 2usize;

    for b in 0..band_count {
        let y_start = b * band_height;
        let y_end = (y_start + band_height).min(height);
        let sample_count = (y_end - y_start) * stride;

        // 8 候选精确 SAD 排序
        let mut cands: Vec<(u64, PredictionMode)> = ADAPTIVE_CANDIDATES
            .iter()
            .map(|&m| {
                let mut res = vec![0i32; sample_count];
                apply_prediction_band_into(pixels, &mut res, width, components, m, y_start, y_end);
                let sad: u64 = res.iter().map(|&v| v.unsigned_abs() as u64).sum();
                (sad, m)
            })
            .collect();
        cands.sort_by_key(|&(s, _)| s);

        // top-2 模式分别试两种熵编码，各取最小
        let mut rle_best = usize::MAX;
        let mut cabac_best = usize::MAX;
        for &(_, m) in cands.iter().take(2) {
            let mut res = vec![0i32; sample_count];
            apply_prediction_band_into(pixels, &mut res, width, components, m, y_start, y_end);
            let (rle, _) =
                encode_frame_rle_golomb_adaptive(&res).map_err(|e| e.to_string())?;
            rle_best = rle_best.min(rle.len());
            let (cab, _) = encode_frame_rle_cabac_adaptive(&res, Some(stride))
                .map_err(|e| e.to_string())?;
            cabac_best = cabac_best.min(cab.len());
        }
        rle_total += 6 + rle_best; // mode u8 + k u8 + len u32
        cabac_total += 6 + cabac_best;
    }
    Ok((rle_total, cabac_total))
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

    println!("=== banded 条带级熵编码升级诊断探针 ===");
    println!("root: {root}\n");

    let all_frames = std::env::var("CRF_PROBE_ALL_FRAMES")
        .map(|v| v == "1")
        .unwrap_or(false);
    let take = if all_frames { usize::MAX } else { 2 };

    // 全局累加（按最优条带高度）
    let mut g_rle = 0u64;
    let mut g_cabac = 0u64;
    let mut frames = 0usize;
    // 逐帧明细（前若干帧）
    println!("--- 逐帧（rle vs cabac，含条带头；取 {BAND_HEIGHTS:?} 最优）---");

    for dir in &groups {
        let fs = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let golden = &fs[0].pixels;
        let components = fs[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let width = fs[0].width as usize;
        if fs.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }
        for frame in fs.iter().skip(1).take(take) {
            let mut diff_rgb = vec![0i32; golden.len()];
            crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
            let diff = rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;

            let mut best_rle = usize::MAX;
            let mut best_cabac = usize::MAX;
            for &bh in &BAND_HEIGHTS {
                if bh != 32 && (golden.len() / (width * components)) < 128 {
                    continue; // 小图 64 行退化为单条带（与生产一致）
                }
                let (rle, cabac) = banded_entropy_totals(&diff, width, components, bh)?;
                best_rle = best_rle.min(rle);
                best_cabac = best_cabac.min(cabac);
            }
            let pct = (best_cabac as f64 - best_rle as f64) / best_rle as f64 * 100.0;
            if frames < 20 {
                println!(
                    "  {:<12} rle={:>10} cabac={:>10} ({pct:+.2}%)",
                    dir.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    best_rle,
                    best_cabac
                );
            }
            g_rle += best_rle as u64;
            g_cabac += best_cabac as u64;
            frames += 1;
        }
    }

    println!("\n=== 汇总（{frames} 差分帧）===");
    let pct = if g_rle == 0 {
        0.0
    } else {
        (g_cabac as f64 - g_rle as f64) / g_rle as f64 * 100.0
    };
    println!("banded(RLE+Golomb): {g_rle} B");
    println!("banded(+CABAC):     {g_cabac} B  ({pct:+.2}%)");
    println!(
        "\n判定：条带残差改 CABAC 相对 RLE+Golomb {:+.2}% —— {}",
        pct,
        if pct <= -3.0 {
            "显著收益（≥3%），banded+CABAC 值得实施"
        } else {
            "收益有限（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn band_heights_present() {
        assert_eq!(super::BAND_HEIGHTS, [32, 64]);
    }
}
