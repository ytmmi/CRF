//! 指定图像组的 adaptive/q90 压缩时间与体积探针（streaming，支持 >50 帧）
//!
//! 用 `StreamingEncoder` 逐帧推送，内存 O(golden+单帧+码流)，适用于超大组
//! （如 30-3-81：81 帧 3826×5412，批量接口超 50 帧限制）。仅测编码时间与体积，
//! 不做解码校验（校验由 `--test` 负责）。
//!
//! 由 `--probe-group-bench <dir>` CLI 分派。

use std::time::Instant;

use image::{ImageReader, RgbImage};

use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::encoder::streaming::StreamingEncoder;
use crate::crf::LossyOptionsV2Builder;

fn collect_paths(dir: &str) -> Result<Vec<String>, String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let ext = e
                .path()
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "tif"
            )
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries
        .iter()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect())
}

fn load_frame(path: &str) -> Result<ImageData, String> {
    let img: RgbImage = ImageReader::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .decode()
        .map_err(|e| format!("{path}: {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        for x in 0..w {
            let px = img.get_pixel(x, y);
            pixels.push(px[0] as i32);
            pixels.push(px[1] as i32);
            pixels.push(px[2] as i32);
        }
    }
    Ok(ImageData {
        width: w as u16,
        height: h as u16,
        bit_depth: 8,
        color_format: ColorFormat::Rgb,
        pixels,
    })
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let paths = collect_paths(dir)?;
    if paths.len() < 2 {
        return Err(format!("{dir}: 至少需要 2 张图片，实际 {}", paths.len()));
    }
    println!("=== 组压缩探针（streaming，支持 >50 帧）===");
    println!("组: {dir}  ({} 帧)\n", paths.len());
    for (label, quality) in [("adaptive(无损)", None), ("q90", Some(90u8))] {
        let params = EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: PredictionMode::Med,
            adaptive_prediction: true,
            lossy: quality.map(|q| {
                LossyOptionsV2Builder::preset(q as u16 * 100)
                    .build()
                    .expect("preset 校验通过")
            }),
            input_original_frames: false,
            user_metadata: None,
        };
        let t0 = Instant::now();
        let mut enc = StreamingEncoder::new(&params).map_err(|e| e.to_string())?;
        let total = paths.len();
        for (i, p) in paths.iter().enumerate() {
            let frame = load_frame(p)?;
            enc.push_frame(&frame).map_err(|e| e.to_string())?;
            // 进度：每 5 帧或末帧打印（含 ETA）
            if (i + 1) % 5 == 0 || i + 1 == total {
                let elapsed = t0.elapsed().as_secs_f64();
                let eta = elapsed / (i + 1) as f64 * (total - i - 1) as f64;
                println!(
                    "  [{label}] 帧 {}/{total} ({:.0}%)  已用 {:.0}s  ETA {:.0}s",
                    i + 1,
                    (i + 1) as f64 / total as f64 * 100.0,
                    elapsed,
                    eta
                );
            }
        }
        let bytes = enc.finish().map_err(|e| e.to_string())?;
        let secs = t0.elapsed().as_secs_f64();
        println!(
            "{label:<16}: {} B ({:.2} MB)  编码 {:.1}s ({:.0} ms/帧)",
            bytes.len(),
            bytes.len() as f64 / 1048576.0,
            secs,
            secs * 1000.0 / paths.len() as f64
        );
    }
    Ok(())
}
