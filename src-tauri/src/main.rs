mod cli_config;
mod crf;
mod test;

use image::{ImageBuffer, ImageReader, Rgb, RgbImage};
use std::fs;
use std::fs::File;
use std::io::BufReader;

fn main() {
    // 检查命令行参数，决定运行模式
    let args: Vec<String> = std::env::args().collect();
    let cli_lossy = cli_config::parse(&args).unwrap_or_else(|e| {
        eprintln!("Configuration error: {e}");
        std::process::exit(2);
    });
    cli_config::write_requested_exports(&cli_lossy).unwrap_or_else(|e| {
        eprintln!("Configuration export error: {e}");
        std::process::exit(2);
    });
    if cli_lossy.dump_schema.is_some() || cli_lossy.dump_resolved.is_some() {
        return;
    }
    if args.len() > 1 && args[1] == "--debug-pixels" {
        test::debug_pixels(&args[2], &args[3]);
        return;
    }
    if args.len() > 1 && args[1] == "--test" {
        // 运行测试模式：可选第二个参数指定图像组目录
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        test::run_all_tests(&dir);
        return;
    }
    if args.len() > 1 && args[1] == "--probe-split" {
        // 超块先导探针：条带列方向二分收益测量（可选第二个参数指定图像组）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        test::run_probe_split_tests(&dir);
        return;
    }
    if args.len() > 1 && args[1] == "--bench" {
        // 端到端编解码基准（P0 可复现基线；可选第二个参数指定图像组目录）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::bench::run(&dir) {
            eprintln!("Benchmark error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-planar" {
        // planar 剪枝探针（profile 验证；可选第二个参数指定图像组目录）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_planar::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-planar-sub" {
        // planar 子平面次级候选胜出频率探针（profile 验证）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_planar_sub::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-first-frame" {
        // 首帧内部候选阶段 profile 探针
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_first_frame::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }

    let input_dir = r"E:\CRF\test\png";
    let output_dir = r"E:\CRF\test\output\crf";

    fs::create_dir_all(output_dir).expect("Failed to create output directory");

    let mut entries: Vec<_> = fs::read_dir(input_dir)
        .expect("Failed to read input directory")
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

    entries.sort_by_key(|a| a.file_name());
    println!("Found {} images in {:?}", entries.len(), input_dir);

    if entries.len() < 2 {
        eprintln!("Error: Need at least 2 images, found {}", entries.len());
        std::process::exit(1);
    }

    // 1. 加载原始图片（最多取前5帧测试）
    let mut frames = Vec::new();
    let mut first_width = 0u16;
    let mut first_height = 0u16;
    let max_frames = 5; // 限制帧数避免内存不足

    for (i, entry) in entries.iter().take(max_frames).enumerate() {
        let path = entry.path();
        println!(
            "Loading [{}/{}]: {:?}",
            i + 1,
            entries.len(),
            path.file_name().unwrap()
        );

        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Failed to open: {}", e);
                continue;
            }
        };
        let reader = BufReader::new(file);
        let img = match ImageReader::new(reader).with_guessed_format() {
            Ok(guessed) => match guessed.decode() {
                Ok(img) => img,
                Err(e) => {
                    eprintln!("Failed to decode: {}", e);
                    continue;
                }
            },
            Err(e) => {
                eprintln!("Failed to guess format: {}", e);
                continue;
            }
        };

        let rgb = img.to_rgb8();
        let (w, h) = rgb.dimensions();

        if i == 0 {
            first_width = w as u16;
            first_height = h as u16;
        } else if w != first_width as u32 || h != first_height as u32 {
            eprintln!("Warning: dimensions mismatch, skipping");
            continue;
        }

        let pixels: Vec<i32> = rgb
            .pixels()
            .flat_map(|p| vec![p[0] as i32, p[1] as i32, p[2] as i32])
            .collect();

        frames.push(crf::ImageData {
            width: first_width,
            height: first_height,
            bit_depth: 8,
            color_format: crf::ColorFormat::Rgb,
            pixels,
        });
    }

    println!(
        "\nLoaded {} frames ({}x{})",
        frames.len(),
        first_width,
        first_height
    );

    if frames.len() < 2 {
        eprintln!("Error: Need at least 2 valid frames");
        std::process::exit(1);
    }

    // 2. 计算帧间差分（残差帧）： residual[i] = frame[i] - frame[i-1]
    let mut residuals: Vec<crf::ImageData> = Vec::new();
    for i in 1..frames.len() {
        let residual_pixels: Vec<i32> = frames[i]
            .pixels
            .iter()
            .zip(frames[i - 1].pixels.iter())
            .map(|(a, b)| a - b)
            .collect();
        residuals.push(crf::ImageData {
            width: first_width,
            height: first_height,
            bit_depth: 8,
            color_format: crf::ColorFormat::Rgb,
            pixels: residual_pixels,
        });
    }

    // 3. 统计残差数据分布
    let all_vals: Vec<i32> = residuals
        .iter()
        .flat_map(|r| r.pixels.iter().cloned())
        .collect();
    let min_val = all_vals.iter().min().unwrap();
    let max_val = all_vals.iter().max().unwrap();
    let mean_val = all_vals.iter().sum::<i32>() as f64 / all_vals.len() as f64;
    let within_10 = all_vals
        .iter()
        .filter(|&&v| (-10..=10).contains(&v))
        .count();
    let total = all_vals.len();
    println!("\nResidual statistics ({} frames):", residuals.len());
    println!("  Range: [{}, {}]", min_val, max_val);
    println!("  Mean: {:.2}", mean_val);
    println!(
        "  Values within [-10, 10]: {:.2}%",
        within_10 as f64 / total as f64 * 100.0
    );

    // 4. 编码残差帧为 CRF 格式
    let original_png_size: usize = 178 * 1024 * 1024;
    println!(
        "\nOriginal PNG total: ~{:.1} MB",
        original_png_size as f64 / 1048576.0
    );

    println!("\n=== Golomb-Rice compression ===");

    let params = crf::EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: crf::PredictionMode::None,
        adaptive_prediction: false,
        lossy: cli_lossy.lossy.clone(),
        input_original_frames: false,
        user_metadata: Some(format!(
            "{} residual frames, {}x{}",
            residuals.len(),
            first_width,
            first_height
        )),
    };

    match crf::codec::encode(crf::codec::EncodeRequest {
        frames: residuals.clone(),
        params: params.clone(),
    }) {
        Ok(report) => {
            let encoded = &report.bytes;
            let output_path = format!("{}/test_residual.crf", output_dir);
            fs::write(&output_path, encoded).expect("Failed to write");

            let ratio = encoded.len() as f64 / original_png_size as f64 * 100.0;
            println!("  Output: {}", output_path);
            println!(
                "  CRF size: {} bytes ({:.1} MB)",
                encoded.len(),
                encoded.len() as f64 / 1048576.0
            );
            println!("  vs original PNG: {:.2}%", ratio);

            // 验证解码
            match crf::codec::decode_from_bytes(crf::codec::DecodeRequest {
                bytes: encoded.clone(),
            }) {
                Ok(result) => {
                    println!("  Decoded {} residual frames", result.frames.len());
                    let mut all_match = true;
                    for (i, (orig, dec)) in residuals.iter().zip(result.frames.iter()).enumerate() {
                        if orig.pixels != dec.pixels {
                            eprintln!(
                                "  Frame {} MISMATCH! first diff at pixel {}",
                                i,
                                orig.pixels
                                    .iter()
                                    .zip(dec.pixels.iter())
                                    .position(|(a, b)| a != b)
                                    .unwrap_or(0)
                            );
                            all_match = false;
                            break;
                        }
                    }
                    if all_match {
                        println!("  Verification: PASSED");
                    } else {
                        println!("  Verification: FAILED");
                    }
                }
                Err(e) => eprintln!("  Decode error: {}", e),
            }
        }
        Err(e) => eprintln!("  Encode error: {}", e),
    }

    // 5. 测试从文件读取解码并输出图片
    println!("\n=== File-based decode test ===");
    let crf_path = format!("{}/test_residual.crf", output_dir);
    let img_output_dir = r"E:\CRF\test\output\img";
    fs::create_dir_all(img_output_dir).expect("Failed to create img output directory");

    match fs::read(&crf_path) {
        Ok(data) => {
            println!("  Read {} bytes from {}", data.len(), crf_path);
            match crf::decode_from_bytes(&data) {
                Ok(result) => {
                    println!("  Decoded {} frames from file", result.frames.len());
                    println!(
                        "  Header: {}x{}, {} frames, depth={}",
                        result.header.width,
                        result.header.height,
                        result.header.frame_count,
                        result.header.bit_depth
                    );

                    let w = result.header.width as u32;
                    let h = result.header.height as u32;

                    // 保存每帧残差图为 PNG
                    for (i, frame) in result.frames.iter().enumerate() {
                        let mut img: RgbImage = ImageBuffer::new(w, h);
                        for y in 0..h {
                            for x in 0..w {
                                let base = ((y * w + x) * 3) as usize;
                                let r = frame.pixels[base].clamp(0, 255) as u8;
                                let g = frame.pixels[base + 1].clamp(0, 255) as u8;
                                let b = frame.pixels[base + 2].clamp(0, 255) as u8;
                                img.put_pixel(x, y, Rgb([r, g, b]));
                            }
                        }
                        let path = format!("{}/residual_{:02}.png", img_output_dir, i);
                        img.save(&path).expect("Failed to save image");
                        println!("  Saved: {}", path);
                    }

                    // 验证与原始残差帧一致
                    let mut all_match = true;
                    for (i, (orig, dec)) in residuals.iter().zip(result.frames.iter()).enumerate() {
                        if orig.pixels != dec.pixels {
                            eprintln!("  Frame {} MISMATCH!", i);
                            all_match = false;
                            break;
                        }
                    }
                    if all_match {
                        println!("  File decode verification: PASSED");
                    } else {
                        println!("  File decode verification: FAILED");
                    }
                }
                Err(e) => eprintln!("  Decode error: {}", e),
            }
        }
        Err(e) => eprintln!("  Failed to read CRF file: {}", e),
    }

    println!("\n=== Done ===");
}
