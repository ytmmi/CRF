//! JPEG-XL 式抖动调色板收益探针（optimization-review §56 低优先新候选）
//!
//! 对低色数图做**有损**抖动调色板：均匀量化到 L³ 色 + Floyd-Steinberg 抖动，
//! 测 PSNR + 像素级 palette 编码字节，与现有最优（无损 `encode_frame_adaptive`）
//! 对比，判断「有损抖动调色板」是否有潜力（在可接受 PSNR 下显著更省字节）。
//!
//! 说明：抖动调色板属有损；仅统计像素级色数 ≤256 的帧（palette 可用场景），
//! 且像素数超限跳过（误差扩散 O(pixel) 对大图过慢）。
//!
//! 不接入生产路径，由 `--probe-palette-dither <root>` CLI 分派。

use std::collections::HashMap;

use crate::crf::core::domain::CompressionType;
use crate::crf::encoder::exp_golomb::ExpGolombEncoder;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_golomb::RleGolombEncoder;
use crate::crf::performance::bench::load_frames;

/// 超过此像素数跳过（误差扩散过慢）。
const PIXEL_LIMIT: usize = 4_000_000;

/// 均匀量化 + Floyd-Steinberg 抖动，返回 (量化像素, PSNR dB)。
fn quantize_dither(pixels: &[i32], w: usize, h: usize, levels: i32) -> (Vec<i32>, f64) {
    let step = 256.0 / levels as f64;
    let comp = 3usize;
    let stride = w * comp;
    let mut out = vec![0i32; pixels.len()];
    let mut err = vec![0.0f64; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            for c in 0..comp {
                let idx = y * stride + x * comp + c;
                let v = pixels[idx] as f64 + err[idx];
                let q = ((v / step).round() * step).clamp(0.0, 255.0);
                out[idx] = q as i32;
                let e = v - q;
                // FS 扩散：右 7/16，左下 3/16，下 5/16，右下 1/16
                if x + 1 < w {
                    err[idx + comp] += e * 7.0 / 16.0;
                }
                if y + 1 < h {
                    if x > 0 {
                        err[idx + stride - comp] += e * 3.0 / 16.0;
                    }
                    err[idx + stride] += e * 5.0 / 16.0;
                    if x + 1 < w {
                        err[idx + stride + comp] += e * 1.0 / 16.0;
                    }
                }
            }
        }
    }
    let mut mse = 0.0f64;
    for i in 0..pixels.len() {
        let d = (pixels[i] - out[i]) as f64;
        mse += d * d;
    }
    mse /= pixels.len().max(1) as f64;
    let psnr = if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    };
    (out, psnr)
}

/// 像素级 palette 编码（RGB 三元组），返回字节；色数 >256 返回 None。
fn encode_pixel_palette(pixels: &[i32]) -> Option<usize> {
    let mut map: HashMap<u64, u16> = HashMap::new();
    let mut order: Vec<(i32, i32, i32)> = Vec::new();
    let mut indices: Vec<i32> = Vec::with_capacity(pixels.len() / 3);
    for px in pixels.chunks_exact(3) {
        let key =
            ((px[0] as u32 as u64) << 32) | ((px[1] as u32 as u64) << 16) | (px[2] as u32 as u64);
        let next = map.len() as u16;
        match map.get(&key) {
            Some(&ix) => indices.push(ix as i32),
            None => {
                if next as usize >= 256 {
                    return None;
                }
                map.insert(key, next);
                order.push((px[0], px[1], px[2]));
                indices.push(next as i32);
            }
        }
    }
    let mut e = RleGolombEncoder::adaptive(&indices);
    e.encode_signed_array(&indices);
    let idx_bytes = e.finish().len();
    let mut pe = ExpGolombEncoder::new();
    for &(r, g, b) in &order {
        pe.encode_signed(r);
        pe.encode_signed(g);
        pe.encode_signed(b);
    }
    Some(idx_bytes + pe.finish().len())
}

/// 运行探针。`root` 为含图像子目录的根（默认 test/png-valid）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== JPEG-XL 抖动调色板探针（§56 低优先候选）===");
    println!("root: {root}\n");
    println!(
        "{:<20} {:>6} {:>10} {:>8} {:>10} {:>10}",
        "图", "L³色", "PSNR(dB)", "A2最优", "抖动字节", "抖动/A2"
    );
    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for (fi, frame) in frames.iter().enumerate() {
            if frame.color_format.component_count() != 3 || frame.pixels.len() > PIXEL_LIMIT {
                continue;
            }
            // A2：现有最优（无损）
            let a2 = encode_frame_adaptive(
                frame,
                CompressionType::GolombRice,
                8,
                true,
                FrameQuant::lossless(),
                None,
                None,
            )
            .map_err(|e| e.to_string())?
            .data
            .len();
            for levels in [4i32, 6, 8] {
                let (q, psnr) = quantize_dither(
                    &frame.pixels,
                    frame.width as usize,
                    frame.height as usize,
                    levels,
                );
                let Some(bytes) = encode_pixel_palette(&q) else {
                    continue; // 色数 >256，跳过
                };
                println!(
                    "{:<20} {:>6} {:>10.2} {:>10} {:>10} {:>9.1}%",
                    format!("{name}/{fi}"),
                    levels * levels * levels,
                    psnr,
                    a2,
                    bytes,
                    (bytes as f64 - a2 as f64) / a2 as f64 * 100.0
                );
            }
        }
    }
    println!("\n判定：抖动调色板在 PSNR ≥40dB 下字节显著小于 A2（无损）才有潜力（有损 vs 无损粗略信号）。");
    Ok(())
}
