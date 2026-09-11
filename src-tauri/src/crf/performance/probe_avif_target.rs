//! AVIF 对标标定探针（first-frame-optimization-plan §6.3）
//!
//! 对指定图像组按质量档位扫描，计算全局 PSNR / 平均 SSIM / 最差帧 PSNR / 完整字节，
//! 与 AVIF CQ18 基准对比，判定阶段 A（≤AVIF+10%）/ 阶段 B（≤AVIF）达标。
//!
//! **口径**：与 optimization-review §19 一致——使用**批量路径** `encode_sequence`
//! + `decode_from_bytes`（自包含还原，首帧 golden 语义），而非流式路径
//! （流式恒 golden 差分，有损行为与批量不同，不能用于 AVIF 对标）。
//!
//! 由 `--probe-avif-target <dir>` 分派。零外部数据集，仅扫指定目录。

use std::time::Instant;

use crate::crf::core::domain::{EncodeParams, ImageData, PredictionMode};
use crate::crf::core::metrics::ssim::ssim;
use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder::encode_sequence;
use crate::crf::performance::bench::load_frames;
use crate::crf::LossyOptionsV2Builder;

use super::probe_monotonicity::{frame_mse, psnr_from_mse};

/// AVIF CQ18 基准（PNG1000 14 帧，optimization-review §19，RGB 自包含还原口径）
const AVIF_BYTES: u64 = 3_370_055;
const AVIF_PSNR: f64 = 37.324;
const AVIF_WORST_PSNR: f64 = 37.116;
/// 阶段 A：AVIF +10%
const STAGE_A_BYTES: u64 = 3_707_061;
/// 阶段 B：不大于 AVIF
const STAGE_B_BYTES: u64 = 3_370_055;
/// 质量匹配窗口（±dB）
const PSNR_TOL: f64 = 0.5;
/// Q 档扫描阶梯（CRF_PROBE_STEP 设置时只用首个占位）
const LADDER: [u16; 1] = [75];

/// 运行探针。`dir` 为图像组目录（批量路径，≤50 帧）。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.len() < 2 {
        return Err(format!("{dir}: 至少需要 2 帧，实际 {}", frames.len()));
    }
    println!("=== AVIF 对标标定探针（批量路径口径）===");
    println!("目录: {dir}  ({} 帧)", frames.len());
    println!("AVIF CQ18 基准: {AVIF_BYTES} B @ {AVIF_PSNR:.3} dB, 最差帧 {AVIF_WORST_PSNR:.3} dB");
    println!("阶段 A ≤{STAGE_A_BYTES} B / 阶段 B ≤{STAGE_B_BYTES} B（匹配质量 ±{PSNR_TOL} dB）\n");
    println!(
        "{:<4} {:>12} {:>10} {:>10} {:>12} {:>10}",
        "Q", "字节", "PSNR(dB)", "SSIM", "最差帧(dB)", "耗时(ms)"
    );

    let mut rows: Vec<(u16, u64, f64, f64, f64)> = Vec::new();
    for &q in &LADDER {
        let params = make_params_avif(q)?;
        let t0 = Instant::now();
        let encoded = encode_sequence(&frames, &params).map_err(|e| e.to_string())?;
        let enc_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let decoded = decode_from_bytes(&encoded).map_err(|e| e.to_string())?;
        if std::env::var("CRF_PROBE_DEBUG").is_ok() {
            for fi in [0usize, 1] {
                let a = &frames[fi].pixels;
                let b = &decoded.frames[fi].pixels;
                let av = &a[..6.min(a.len())];
                let bv = &b[..6.min(b.len())];
                println!(
                    "  DEBUG f{fi} orig={av:?} dec={bv:?} len={}/{}",
                    a.len(),
                    b.len()
                );
            }
        }
        let (mse, pixels, worst, ssim_avg) = quality_batch(&frames, &decoded.frames);
        let psnr = psnr_from_mse(mse, pixels);
        println!(
            "{:<4} {:>12} {:>10.3} {:>10.6} {:>12.3} {:>10.0}",
            q,
            encoded.len(),
            psnr,
            ssim_avg,
            worst,
            enc_ms
        );
        rows.push((q, encoded.len() as u64, psnr, ssim_avg, worst));
    }

    // 判定：找 PSNR 最接近 AVIF 的档位
    println!("\n--- 阶段判定 ---");
    let best = rows.iter().min_by(|a, b| {
        (a.2 - AVIF_PSNR)
            .abs()
            .partial_cmp(&(b.2 - AVIF_PSNR).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(&(q, bytes, psnr, ssim_v, worst)) = best {
        let matched = (psnr - AVIF_PSNR).abs() <= PSNR_TOL;
        println!(
            "最接近 AVIF 质量档位: q{q} (PSNR {psnr:.3} dB, Δ={:+.3} dB)",
            psnr - AVIF_PSNR
        );
        println!(
            "  字节 {bytes} B  vs AVIF {AVIF_BYTES} B  ({:+.1}%)",
            (bytes as f64 - AVIF_BYTES as f64) / AVIF_BYTES as f64 * 100.0
        );
        println!("  SSIM {ssim_v:.6}");
        println!("  最差帧 {worst:.3} dB vs AVIF {AVIF_WORST_PSNR:.3} dB");
        if matched {
            println!("  质量匹配: ✓（在 ±{PSNR_TOL} dB 窗口内）");
            if bytes <= STAGE_B_BYTES {
                println!("  阶段 B: ✅ 达标（≤AVIF 字节）");
            } else if bytes <= STAGE_A_BYTES {
                println!("  阶段 A: ✅ 达标（≤AVIF+10%）");
                println!("  阶段 B: ❌ 未达（需 ≤{STAGE_B_BYTES} B）");
            } else {
                println!("  阶段 A/B: ❌ 均未达（需 ≤{STAGE_A_BYTES} / {STAGE_B_BYTES} B）");
            }
        } else {
            println!("  质量匹配: ❌ 最近档位仍在 ±{PSNR_TOL} dB 窗口外（需细化档位）");
        }
    }
    Ok(())
}

/// 构造批量闭环有损参数（`input_original_frames: false`，与 --test 对标口径一致）。
///
/// 环境变量 `CRF_PROBE_STEP` 设置时改用显式步长（隔离 quality→step 映射，用于
/// 定位 step 4/5 的 PSNR 倒挂）。
fn make_params_avif(quality: u16) -> Result<EncodeParams, String> {
    let lossy = if let Ok(step_s) = std::env::var("CRF_PROBE_STEP") {
        // 格式 "luma" 或 "luma:chroma"
        let (ls, cs) = match step_s.split_once(':') {
            Some((l, c)) => (l, c),
            None => (step_s.as_str(), step_s.as_str()),
        };
        let l: f64 = ls.parse().map_err(|_| "bad luma step".to_string())?;
        let c: f64 = cs.parse().map_err(|_| "bad chroma step".to_string())?;
        LossyOptionsV2Builder::explicit_steps(
            (l * 256.0).round() as u16,
            (c * 256.0).round() as u16,
        )
        .build()
        .map_err(|e| e.to_string())?
    } else {
        LossyOptionsV2Builder::preset(quality * 100)
            .build()
            .map_err(|e| e.to_string())?
    };
    Ok(EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Med,
        adaptive_prediction: true,
        lossy: Some(lossy),
        input_original_frames: false,
        user_metadata: None,
    })
}

/// 批量还原质量：返回 (总MSE, 总像素, 最差帧PSNR, 平均SSIM)。
fn quality_batch(orig: &[ImageData], restored: &[ImageData]) -> (f64, u64, f64, f64) {
    let verbose = std::env::var("CRF_PROBE_PERFRAME").is_ok();
    let mut total_mse = 0.0f64;
    let mut total_pixels = 0u64;
    let mut worst = f64::INFINITY;
    let mut ssim_sum = 0.0f64;
    let n = orig.len().min(restored.len());
    for i in 0..n {
        let (mse, px) = frame_mse(&orig[i], &restored[i]);
        total_mse += mse;
        total_pixels += px;
        let p = psnr_from_mse(mse, px);
        worst = worst.min(p);
        ssim_sum += ssim(&orig[i], &restored[i]);
        if verbose {
            println!(
                "    f{i}: PSNR {p:.3} dB, SSIM {:.6}",
                ssim(&orig[i], &restored[i])
            );
        }
    }
    (total_mse, total_pixels, worst, ssim_sum / n.max(1) as f64)
}
