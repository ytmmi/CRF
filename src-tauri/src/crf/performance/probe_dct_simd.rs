//! DCT i16 打包 SIMD 可行性探针（optimization-review §56 新候选）
//!
//! 回答：i16 打包（vpmaddwd）替代 i32 AVX2 是否有端到端价值。
//! - 测 DCT 输入/输出值域（判断 i16 适用性：中间值是否超 16 位）；
//! - 测各 DCT 变体标量耗时（确认热点量级）；
//! - 结合 encode p50 估算 i16 SIMD 的端到端收益上限。
//!
//! 不接入生产路径，由 `--probe-dct-simd <dir>` CLI 分派。

use std::time::Instant;

use crate::crf::core::color::rct;
use crate::crf::encoder::dct_path::dct_quantize_interleaved_bs;
use crate::crf::performance::bench::load_frames;

/// 组 1000 无损 bench encode p50（§51 基准），用于端到端占比估算。
const ENCODE_P50_MS: f64 = 9327.0;

/// 运行探针。`dir` 为图像组目录（取首帧）。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    let frame = frames.first().ok_or_else(|| format!("{dir}: no images"))?;
    if frame.color_format.component_count() != 3 {
        return Err(format!("{dir}: 首帧非 RGB"));
    }
    let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
    let w = frame.width as usize;
    let h = frame.height as usize;

    let in_max = ycocg.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
    println!("=== DCT i16 打包 SIMD 可行性探针（§56）===");
    println!("group: {dir}  首帧 {}x{}", frame.width, frame.height);
    println!("DCT 输入 RCT 域 max|v| = {in_max}  (i16 上限 32767)\n");

    // 各 DCT 变体（无损 Q=1，flat 矩阵）
    let variants = [(4usize, 4usize), (8, 8), (8, 4), (4, 8)];
    println!("{:<10} {:>12} {:>14}", "变体", "耗时(ms)", "系数max|v|");
    let mut max_ms = 0.0f64;
    for (bw, bh) in variants {
        // 预热
        let _ = dct_quantize_interleaved_bs(&ycocg, w, h, 3, 1, bw, bh, false, false);
        let t0 = Instant::now();
        let coeffs = dct_quantize_interleaved_bs(&ycocg, w, h, 3, 1, bw, bh, false, false);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let cmax = coeffs.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
        max_ms = max_ms.max(ms);
        println!("{:<10} {:>12.1} {:>14}", format!("{bw}x{bh}"), ms, cmax);
    }

    println!(
        "\nDCT 阶段 wall ≈ max(变体) = {max_ms:.1}ms（生产 4 变体并行，P1c）"
    );
    println!(
        "encode p50(组1000 无损) = {ENCODE_P50_MS:.0}ms  → DCT 阶段占比 ≈ {:.1}%",
        max_ms / ENCODE_P50_MS * 100.0
    );
    println!("\n--- i16 SIMD 端到端收益上限估算 ---");
    for k in [1.34f64, 2.0, 4.0] {
        let saved = max_ms * (1.0 - 1.0 / k);
        println!(
            "  内核快 {k:.2}×：省 {saved:.0}ms → 端到端 {:+.2}%",
            saved / ENCODE_P50_MS * 100.0
        );
    }
    println!("\n判定：端到端收益 <3% 则证伪（§56 门槛）；另需确认 DCT 中间值不超 i16。");
    Ok(())
}
