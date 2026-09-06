//! 端到端编解码基准（P0 可复现基线）
//!
//! 由 `--bench <dir>` CLI 分派。流程：加载图像组 → 预热 1 轮 → 测量
//! `encode_sequence` / `decode_from_bytes` 各 N 轮 → 报告字节数与
//! p50/p95 延迟、吞吐（MPix/s），并在阶段计时开启时输出 telemetry 报告。
//!
//! 不改码流、不改变默认后端；仅调用公共 `encode_sequence` /
//! `decode_from_bytes` 入口，与生产路径完全一致。

use std::path::Path;
use std::time::Instant;

use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder::encode_sequence;
use crate::crf::performance::telemetry;

/// 测量轮数（不含预热）。
const MEASURED_ROUNDS: usize = 5;

/// 运行基准。`dir` 为图像组目录；失败时返回结构化错误。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.len() < 2 {
        return Err(format!("{dir}: 至少需要 2 帧，实际 {}", frames.len()));
    }
    let width = frames[0].width as usize;
    let height = frames[0].height as usize;
    let frame_count = frames.len();
    let megapixels_per_seq = (width as f64 * height as f64 * frame_count as f64) / 1_000_000.0;

    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Med,
        adaptive_prediction: true,
        lossy: None,
        input_original_frames: true,
        user_metadata: None,
    };

    telemetry::enable();
    println!("=== CRF 端到端基准 ===");
    println!(
        "组: {dir}  ({width}x{height}, {frame_count} 帧, {:.2} MPix/序列)",
        megapixels_per_seq
    );
    println!("模式: golomb-rice / adaptive / 路径 G (input_original_frames)");
    println!("轮数: 预热 1 + 测量 {MEASURED_ROUNDS}\n");

    // 预热：触发 lazy init（如 CUDA DLL 探测）、填充分配器与线程池。
    let warm = encode_sequence(&frames, &params).map_err(|e| e.to_string())?;
    let _ = decode_from_bytes(&warm).map_err(|e| e.to_string())?;

    // 清空预热期采样，阶段计时跨全部测量轮聚合（与端到端 p50/p95 同口径）。
    telemetry::clear();

    let mut enc_times = Vec::with_capacity(MEASURED_ROUNDS);
    let mut dec_times = Vec::with_capacity(MEASURED_ROUNDS);
    let mut bytes = 0usize;

    for round in 0..MEASURED_ROUNDS {
        let enc_start = Instant::now();
        let encoded = encode_sequence(&frames, &params).map_err(|e| e.to_string())?;
        let enc_elapsed = enc_start.elapsed();
        let dec_start = Instant::now();
        let _decoded = decode_from_bytes(&encoded).map_err(|e| e.to_string())?;
        let dec_elapsed = dec_start.elapsed();
        enc_times.push(enc_elapsed);
        dec_times.push(dec_elapsed);
        bytes = encoded.len();
        println!(
            "round {round}: encode {:.1}ms  decode {:.1}ms  bytes {}",
            enc_elapsed.as_secs_f64() * 1000.0,
            dec_elapsed.as_secs_f64() * 1000.0,
            bytes,
        );
    }

    enc_times.sort_unstable();
    dec_times.sort_unstable();
    let enc_p50 = telemetry::percentile(&enc_times, 0.50);
    let enc_p95 = telemetry::percentile(&enc_times, 0.95);
    let dec_p50 = telemetry::percentile(&dec_times, 0.50);
    let dec_p95 = telemetry::percentile(&dec_times, 0.95);

    println!("\n--- 汇总 ---");
    println!(
        "encode: p50 {:.1}ms  p95 {:.1}ms  吞吐 {:.2} MPix/s",
        enc_p50.as_secs_f64() * 1000.0,
        enc_p95.as_secs_f64() * 1000.0,
        megapixels_per_seq / (enc_p50.as_secs_f64().max(1e-9)),
    );
    println!(
        "decode: p50 {:.1}ms  p95 {:.1}ms  吞吐 {:.2} MPix/s",
        dec_p50.as_secs_f64() * 1000.0,
        dec_p95.as_secs_f64() * 1000.0,
        megapixels_per_seq / (dec_p50.as_secs_f64().max(1e-9)),
    );
    println!(
        "bytes: {bytes}  ({:.3} B/像素)",
        bytes as f64 / (width * height * frame_count) as f64
    );

    let stage_report = telemetry::report();
    if !stage_report.is_empty() {
        println!("\n--- 阶段耗时（telemetry）---\n{stage_report}");
    }
    Ok(())
}

/// 加载目录内全部 PNG/JPG 图像为 RGB 帧（按文件名排序，与测试路径一致）。
pub(crate) fn load_frames(dir: &str) -> Result<Vec<ImageData>, String> {
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
            matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut frames = Vec::new();
    for entry in entries {
        let path = entry.path();
        let img = image::ImageReader::open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .decode()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let rgb = img.to_rgb8();
        let (w, h) = rgb.dimensions();
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
        for p in rgb.pixels() {
            pixels.push(p[0] as i32);
            pixels.push(p[1] as i32);
            pixels.push(p[2] as i32);
        }
        frames.push(ImageData {
            width: w as u16,
            height: h as u16,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels,
        });
    }
    Ok(frames)
}

/// 供外部探测基准组目录是否可用。
pub fn group_exists(dir: &str) -> bool {
    Path::new(dir).is_dir()
}
