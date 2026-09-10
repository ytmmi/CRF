//! planar 条带级模式切换收益探针（compression-algorithm-exploration §9 建议 9）
//!
//! planar（frame_type=3）的子平面当前在整帧维度选一个预测模式（帧级 SATD 最优），
//! 而 banded（frame_type=2）已有条带级切换。本探针统计 planar 子平面（Y/Co/Cg）
//! 帧内不同条带的最优预测模式差异程度：
//!
//! - 整帧最优模式（8 候选 SATD 最小）的 SATD；
//! - 每 `BAND` 行条带各自最优模式的 SATD 之和；
//! 若条带级 SATD 显著低于整帧（>3%），则条带级模式切换有理论收益——但仍需扣除
//! 条带头开销（§13 R13② 警示条带头翻倍侵蚀收益）。
//!
//! 口径：SATD 仅作候选排序/理论信号，不等于最终字节收益。
//!
//! 不接入生产路径，仅由 `--probe-planar-band-mode <dir>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ColorFormat, PredictionMode};
use crate::crf::core::prediction::intra::predict_at;
use crate::crf::encoder::frame::candidate::ADAPTIVE_CANDIDATES;
use crate::crf::performance::bench::load_frames;

const BAND: usize = 32;
const BLOCK_STRIDE: usize = 8;

/// 4×4 Hadamard SATD（复制 cost.rs 私有实现，探针独立维护）。
fn satd4x4(block: &[i32; 16]) -> u64 {
    let mut rows = [[0i64; 4]; 4];
    for r in 0..4 {
        let x0 = block[r * 4] as i64;
        let x1 = block[r * 4 + 1] as i64;
        let x2 = block[r * 4 + 2] as i64;
        let x3 = block[r * 4 + 3] as i64;
        let a0 = x0 + x1;
        let a1 = x0 - x1;
        let a2 = x2 + x3;
        let a3 = x2 - x3;
        rows[r] = [a0 + a2, a1 + a3, a0 - a2, a1 - a3];
    }
    let mut total = 0u64;
    for c in 0..4 {
        let a0 = rows[0][c] + rows[1][c];
        let a1 = rows[0][c] - rows[1][c];
        let a2 = rows[2][c] + rows[3][c];
        let a3 = rows[2][c] - rows[3][c];
        for value in [a0 + a2, a1 + a3, a0 - a2, a1 - a3] {
            total = total.saturating_add(value.unsigned_abs());
        }
    }
    total
}

/// 单分量平面 [y0, y1) 行范围的采样 SATD（每 8×8 区域取一个 4×4 块）。
fn band_satd(
    pixels: &[i32],
    width: usize,
    height: usize,
    mode: PredictionMode,
    y0: usize,
    y1: usize,
) -> u64 {
    let mut satd = 0u64;
    let mut block = [0i32; 16];
    let mut by = y0;
    while by < y1 {
        let mut bx = 0;
        while bx < width {
            block.fill(0);
            for dy in 0..4 {
                let y = by + dy;
                if y >= y1 || y >= height {
                    break;
                }
                for dx in 0..4 {
                    let x = bx + dx;
                    if x >= width {
                        break;
                    }
                    let index = y * width + x;
                    let predicted = predict_at(pixels, index, x, y, width, 1, width, mode);
                    block[dy * 4 + dx] = pixels[index].wrapping_sub(predicted);
                }
            }
            satd = satd.saturating_add(satd4x4(&block));
            bx += BLOCK_STRIDE;
        }
        by += BLOCK_STRIDE;
    }
    satd
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    let frame = frames.first().ok_or_else(|| format!("{dir}: no images"))?;
    if frame.color_format != ColorFormat::Rgb {
        return Err(format!("{dir}: 首帧非 RGB"));
    }
    let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut planes: [Vec<i32>; 3] = std::array::from_fn(|_| vec![0i32; w * h]);
    for (i, px) in ycocg.chunks_exact(3).enumerate() {
        planes[0][i] = px[0];
        planes[1][i] = px[1];
        planes[2][i] = px[2];
    }

    println!("=== planar band-mode probe (compression-algorithm-exploration §9 建议 9) ===");
    println!(
        "group: {dir}  首帧 {}x{}  band={BAND} 行\n",
        frame.width, frame.height
    );
    let names = ["Y", "Co", "Cg"];
    for (pi, plane) in planes.iter().enumerate() {
        // 整帧最优模式
        let mut whole_best = (u64::MAX, PredictionMode::DC);
        for &m in &ADAPTIVE_CANDIDATES {
            let s = band_satd(plane, w, h, m, 0, h);
            if s < whole_best.0 {
                whole_best = (s, m);
            }
        }
        // 每条带最优模式
        let mut band_sum = 0u64;
        let mut band_modes: Vec<PredictionMode> = Vec::new();
        let mut y = 0;
        while y < h {
            let y1 = (y + BAND).min(h);
            let mut best = (u64::MAX, PredictionMode::DC);
            for &m in &ADAPTIVE_CANDIDATES {
                let s = band_satd(plane, w, h, m, y, y1);
                if s < best.0 {
                    best = (s, m);
                }
            }
            band_sum += best.0;
            band_modes.push(best.1);
            y = y1;
        }
        let gain = 1.0 - band_sum as f64 / whole_best.0.max(1) as f64;
        // 差异模式数（手动去重，避免依赖 PredictionMode: Hash）
        let mut distinct: Vec<PredictionMode> = Vec::new();
        for &m in &band_modes {
            if !distinct.contains(&m) {
                distinct.push(m);
            }
        }
        let mode_str: Vec<String> = band_modes.iter().map(|m| format!("{m:?}")).collect();
        println!(
            "{:<4} 整帧最优={:?}  SATD={}  条带数={}  条带最优=[{}]  差异模式数={}  条带级SATD收益={:+.1}%",
            names[pi],
            whole_best.1,
            whole_best.0,
            band_modes.len(),
            mode_str.join(","),
            distinct.len(),
            gain * 100.0
        );
    }
    println!("\n判定参考：条带级 SATD 收益 >3% 且扣除条带头开销后仍有净收益，才值得实施（§9 建议 9）。");
    Ok(())
}
