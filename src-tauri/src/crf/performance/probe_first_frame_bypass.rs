//! 首帧 RCT 双路竞争探针：RGB 直通胜出率 + 内容特征 + 耗时占比
//!
//! v1.13 首帧 RCT 自适应对首帧跑「YCoCg-R 域 vs RGB 直通」两路完整管线竞争，
//! 字节最小者胜出（直通胜出置位 flags.bit3）。文档声称「自然相关内容 RCT 恒
//! 胜出，直通只在 G=0 高饱和内容胜出」——若成立，自然插画下 RGB 直通这路
//! 是纯浪费（完整编码 + 从不胜出），可安全跳过（字节不变）。
//!
//! 本探针验证该假设：对每组首帧跑两路编码，统计直通胜出率、胜出时 G 通道
//! 零值占比、以及两路耗时占比，为「内容门控跳过直通」提供数据驱动依据。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;

/// 运行探针。`root` 为 test/png 根目录（扫其下全部 x-y-z 组）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== first-frame RCT bypass probe ===");
    println!("root: {root}  组数: {}\n", groups.len());

    let mut total_frames = 0usize;
    let mut bypass_wins = 0usize;
    let mut bypass_saved_bytes = 0i64; // 直通胜出时省下的字节（rct - bypass）
    let mut bypass_win_g_zero_rates: Vec<f64> = Vec::new();

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if !f.is_empty() => f,
            _ => continue,
        };
        let frame = &frames[0];
        if frame.color_format != crate::crf::core::domain::ColorFormat::Rgb {
            continue;
        }
        total_frames += 1;

        // G 通道零值占比（RGB 交织，G 为 offset 1）
        let g_zero = frame
            .pixels
            .iter()
            .skip(1)
            .step_by(3)
            .filter(|&&v| v == 0)
            .count();
        let g_total = frame.pixels.len() / 3;
        let g_zero_rate = g_zero as f64 / g_total.max(1) as f64;

        let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
        let eff = ImageData {
            width: frame.width,
            height: frame.height,
            bit_depth: frame.bit_depth,
            color_format: frame.color_format,
            pixels: ycocg,
        };
        let rct_out = encode_frame_adaptive(
            &eff,
            CompressionType::GolombRice,
            8,
            false,
            FrameQuant::lossless(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;

        let bypass_out = encode_frame_adaptive(
            frame,
            CompressionType::GolombRice,
            8,
            false,
            FrameQuant::lossless(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;

        let rct_len = rct_out.data.len();
        let bypass_len = bypass_out.data.len();
        let won = bypass_len < rct_len;
        if won {
            bypass_wins += 1;
            bypass_saved_bytes += (rct_len as i64) - (bypass_len as i64);
            bypass_win_g_zero_rates.push(g_zero_rate);
        }
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        println!(
            "{name}: rct={rct_len}  bypass={bypass_len}  g_zero={:.3}  {}",
            g_zero_rate,
            if won { "BYPASS胜出" } else { "rct" }
        );
    }

    println!("\n--- 汇总 ---");
    println!(
        "总首帧数: {total_frames}  直通胜出: {bypass_wins}  ({:.1}%)",
        bypass_wins as f64 / total_frames.max(1) as f64 * 100.0
    );
    if !bypass_win_g_zero_rates.is_empty() {
        let min = bypass_win_g_zero_rates
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        println!(
            "直通胜出组的 G 零值占比: min={:.3}  胜出累计省字节={}",
            min, bypass_saved_bytes
        );
    }
    Ok(())
}
