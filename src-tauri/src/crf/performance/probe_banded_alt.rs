//! banded 条带高度自适应（32 vs 64 行）胜出率探针
//!
//! frame_type=2 在无损模式对每帧跑 32 行档 + 64 行档（height>=128）竞争，
//! 各档都跑 8 候选精确 SAD + top-2 试编码。若 64 行档极少胜出，剪枝它可
//! 省约一半 banded 时间（banded 为 encode 第二大热点）。本探针测量 64 行档
//! 的胜出率与胜出字节差，为「是否剪枝 64 行档」提供数据驱动依据。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ImageData, PredictionMode};
use crate::crf::encoder::banded::encode_banded_payload;
use crate::crf::performance::bench::load_frames;

/// 运行探针。`root` 为 test/png 根目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== banded 32 vs 64 行档胜出率探针 ===");
    println!("root: {root}\n");

    const BAND_32: usize = 32;
    const BAND_64: usize = 64;

    let mut frames_seen = 0usize;
    let mut skip_small = 0usize;
    let mut wins_64 = 0usize;
    let mut wins_32 = 0usize;
    let mut total_saved_64 = 0i64;

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        // 仅统计差分帧（首帧后），与 banded 在 adaptive 里的调用一致；均用
        // RCT 域（与路径 G 差分帧一致）。为控制耗时只取首帧 + 前 2 差分帧。
        let max_frames = frames.len().min(3);
        for frame in frames.iter().skip(1).take(max_frames - 1) {
            let h = frame.height as usize;
            if h < 128 {
                skip_small += 1;
                continue; // 64 行档不参与
            }
            let diff = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
            let eff = ImageData {
                width: frame.width,
                height: frame.height,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels: diff,
            };
            frames_seen += 1;
            let p32 = encode_banded_payload(&eff, BAND_32, PredictionMode::Med)
                .map_err(|e| e.to_string())?;
            let p64 = encode_banded_payload(&eff, BAND_64, PredictionMode::Med)
                .map_err(|e| e.to_string())?;
            if p64.len() < p32.len() {
                wins_64 += 1;
                total_saved_64 += (p32.len() as i64) - (p64.len() as i64);
            } else {
                wins_32 += 1;
            }
        }
    }

    println!("统计帧数: {frames_seen}（height<128 跳过: {skip_small}）");
    println!(
        "64 行档胜出: {wins_64} / {frames_seen} ({:.1}%)  累计省 {total_saved_64} 字节",
        wins_64 as f64 / frames_seen.max(1) as f64 * 100.0
    );
    println!(
        "32 行档胜出(或持平): {wins_32} / {frames_seen} ({:.1}%)",
        wins_32 as f64 / frames_seen.max(1) as f64 * 100.0
    );
    Ok(())
}
