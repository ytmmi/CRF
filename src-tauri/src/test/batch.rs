//! 批量与流式编码测试套件

use crate::crf;
use std::time::Instant;

use super::{clean_dir, collect_png_paths, load_frame_sequence, save_frame_lossless, verify_crf_against_pngs, OutputFormat, Scheme, TEST_CRF_OUTPUT_DIR, TEST_IMG_OUTPUT_DIR};

/// 运行所有测试（png_dir 为图像组目录）
///
/// 产物按图像组隔离落盘：
/// - CRF：`output/crf/<组名>/<方案>.crf`
/// - 还原帧：`output/img/<组名>/<方案名>/frame_NNN.<ext>`（全部方案）
///
/// 每次运行仅清空本组的子目录，其他图像组的历史产物保留。
pub fn run_all_tests(png_dir: &str) {
    println!("=== CRF 编码解码测试开始 ===");
    println!("图像组: {}\n", png_dir);
    let t_total = Instant::now();

    // 组名 = 图像目录末级名称（如 1000 / 2000 / c / d）
    let group_name = std::path::Path::new(png_dir)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let crf_out_dir = format!("{}/{}", TEST_CRF_OUTPUT_DIR, group_name);
    let img_out_dir = format!("{}/{}", TEST_IMG_OUTPUT_DIR, group_name);

    // 重置本组输出子目录（其他图像组的历史产物保留）
    clean_dir(&crf_out_dir);
    clean_dir(&img_out_dir);

    let paths = collect_png_paths(png_dir);
    if paths.len() < 2 {
        eprintln!("需要至少 2 张图片，实际 {}", paths.len());
        return;
    }

    // 流式模式（CRF_STREAMING=1）：逐帧推送，内存 O(golden+单帧+码流)，
    // 适用于超大分辨率组（如 4000：批量接口需 5.8GB 差分序列驻留）
    if std::env::var("CRF_STREAMING")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
    {
        run_streaming_suite(&paths, &crf_out_dir, &img_out_dir);
        println!(
            "\n=== 流式测试完成（总耗时 {:.1}s）===",
            t_total.elapsed().as_secs_f64()
        );
        return;
    }
    run_batch_suite(&paths, &crf_out_dir, &img_out_dir);
    println!(
        "\n=== 所有测试完成（总耗时 {:.1}s）===",
        t_total.elapsed().as_secs_f64()
    );
}

/// 流式编码套件：自适应无损 / q90 / q75 三方案，逐帧推送
pub fn run_streaming_suite(paths: &[String], crf_out_dir: &str, img_out_dir: &str) {
    println!("模式: 流式（CRF_STREAMING=1，内存 O(golden+单帧+码流)）\n");

    for (label, quality) in [
        ("streaming_adaptive", None),
        ("streaming_q90", Some(90u8)),
        ("streaming_q75", Some(75u8)),
    ] {
        let crf_path = format!("{}/{}.crf", crf_out_dir, label);
        let out_dir = format!("{}/{}", img_out_dir, label);

        println!("--- 方案: {} (q={:?}) ---", label, quality);
        let t = Instant::now();

        let frames = load_frame_sequence(paths);
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crf::PredictionMode::Med,
            adaptive_prediction: true,
            lossy_quality: quality,
            lossy_tuning: None,
            input_original_frames: false,
            user_metadata: None,
        };

        let encoded = crf::encode_sequence(&frames, &params).expect("流式编码失败");
        std::fs::write(&crf_path, &encoded).expect("写入 CRF 失败");
        println!(
            "  编码: {} B ({:.1}ms)",
            encoded.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );

        // 解码还原
        let data = std::fs::read(&crf_path).expect("读取 CRF 失败");
        let result = crf::decode(&data).expect("解码失败");

        let _ = std::fs::create_dir_all(&out_dir);
        for (i, frame) in result.frames.iter().enumerate() {
            let frame_path = format!("{}/frame_{:03}.png", out_dir, i);
            save_frame_lossless(frame, &frame_path, OutputFormat::Png);
        }
        println!("  解码: {} 帧", result.frames.len());

        // 逐帧校验
        let mut ok = true;
        for (i, (decoded, original)) in result.frames.iter().zip(frames.iter()).enumerate() {
            if decoded.pixels != original.pixels {
                eprintln!("  帧{} 不一致！", i);
                ok = false;
            }
        }
        if ok {
            println!("  校验: 全部通过 ✓");
        }
        println!();
    }
}

/// 批量编码套件：固定模式 + 自适应模式，全方案对比
pub fn run_batch_suite(paths: &[String], crf_out_dir: &str, img_out_dir: &str) {
    println!("模式: 批量\n");

    let schemes = [
        Scheme {
            file: "fixed_med",
            mode: crf::PredictionMode::Med,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "fixed_paeth",
            mode: crf::PredictionMode::Paeth,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "fixed_average",
            mode: crf::PredictionMode::Average,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "fixed_vertical",
            mode: crf::PredictionMode::Vertical,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "fixed_dc",
            mode: crf::PredictionMode::DC,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "adaptive",
            mode: crf::PredictionMode::Med,
            adaptive: true,
            quality: None,
        },
        Scheme {
            file: "adaptive_q90",
            mode: crf::PredictionMode::Med,
            adaptive: true,
            quality: Some(90),
        },
        Scheme {
            file: "adaptive_q75",
            mode: crf::PredictionMode::Med,
            adaptive: true,
            quality: Some(75),
        },
    ];

    let frames = load_frame_sequence(paths);

    for scheme in &schemes {
        let crf_path = format!("{}/{}.crf", crf_out_dir, scheme.file);
        let out_dir = format!("{}/{}", img_out_dir, scheme.file);

        println!(
            "--- {} (mode={:?}, adaptive={}, q={:?}) ---",
            scheme.file, scheme.mode, scheme.adaptive, scheme.quality
        );
        let t = Instant::now();

        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: scheme.mode,
            adaptive_prediction: scheme.adaptive,
            lossy_quality: scheme.quality,
            lossy_tuning: None,
            input_original_frames: false,
            user_metadata: None,
        };

        let encoded = crf::encode_sequence(&frames, &params).expect("编码失败");
        std::fs::write(&crf_path, &encoded).expect("写入 CRF 失败");
        println!(
            "  编码: {} B ({:.1}ms)",
            encoded.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );

        // 解码还原
        let data = std::fs::read(&crf_path).expect("读取 CRF 失败");
        let result = crf::decode(&data).expect("解码失败");

        let _ = std::fs::create_dir_all(&out_dir);
        for (i, frame) in result.frames.iter().enumerate() {
            let frame_path = format!("{}/frame_{:03}.png", out_dir, i);
            save_frame_lossless(frame, &frame_path, OutputFormat::Png);
        }

        // 校验
        if verify_crf_against_pngs(&crf_path, paths, scheme.file) {
            println!("  ✓ 全部通过");
        } else {
            eprintln!("  ✗ 存在不一致");
        }
        println!();
    }
}
