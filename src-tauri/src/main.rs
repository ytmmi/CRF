mod cli_config;
mod crf;
mod test;

use image::{ImageBuffer, ImageReader, Rgb, RgbImage};
use std::fs;
use std::fs::File;
use std::io::BufReader;

/// 项目正式版本号(四位 `a.b.c.d`),规则见 docs/project-standards.md §13。
/// Cargo.toml 的三位 semver(`a.b.c`)与 `d` 段(bug 修复位)合并而来;
/// 升级时须与 Cargo.toml 及项目标准同步。
pub const APP_VERSION: &str = "0.3.4.2";

fn main() {
    // 检查命令行参数，决定运行模式
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--version" {
        println!("crf-viewer {APP_VERSION}");
        return;
    }
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
    if args.len() > 1 && args[1] == "--probe-planar-candidate" {
        // planar 子平面候选级 Fast-Fail 空间探针（§56 新候选）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_planar_candidate::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-planar-band-mode" {
        // planar 子平面条带级模式切换收益探针（§9 建议 9）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_planar_band_mode::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-planar-parallel" {
        // planar 三子平面「外层并行 + 内层禁用并行」收益探针（§P1c 遗留验证）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_planar_parallel::run(&dir) {
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
    if args.len() > 1 && args[1] == "--probe-group-bench" {
        // 指定图像组的 adaptive/q90 压缩时间与体积探针（streaming，支持 >50 帧）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\30-3-81")
        };
        if let Err(e) = crf::performance::probe_group_bench::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-gpu-kernel" {
        // GPU kernel 端到端加速比探针（性能规划 P3；忽略 dir 参数）
        if let Err(e) = crf::performance::probe_gpu_kernel::run("") {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-first-frame-bypass" {
        // 首帧 RCT 双路竞争：RGB 直通胜出率 + G 零值特征探针（默认扫全部组）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_first_frame_bypass::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-banded-alt" {
        // banded 条带高度自适应（32 vs 64 行）胜出率探针
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_banded_alt::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-lic" {
        // LIC 整帧乘加照明补偿收益探针(§9 建议 6;默认扫 test/png 全部组)
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_lic::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-lic-ab" {
        // LIC A/B 字节对比探针:on vs CRF_DISABLE_LIC=1 的实际编码字节差
        //（v1.16 正式实现有效性验证;默认扫 test/png 全部组）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_lic_ab::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-error-separation" {
        // 首帧误差分离探针:JPEG2000 嵌入式分层收益(默认扫 test/png 全部组)
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_error_separation::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-ma-tree" {
        // MA 树叶数分布探针:直方图共享(§8.2)可行性(默认扫 test/png 全部组)
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_ma_tree::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-monotonicity" {
        // DAT.1 跨内容单调性验收探针（默认扫 test/png 下全部 x-y-z 组）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_monotonicity::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-rest-frames" {
        // 差分帧候选 profile 探针:胜出率 + 阶段耗时分离(默认扫全部组)
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_rest_frames::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-ringing" {
        // P4.7 前置验证：ringing 信号与 edge 分类独立性探针（S0）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_ringing::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-lambda" {
        // P4.6 前置验证：RDOQ Trellis λ 敏感性扫描探针（S1）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_lambda::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-coeff-ctx" {
        // P3 前置验证：CoeffCABAC 方向扫描/邻块上下文合成内容能力探针（无参数）
        if let Err(e) = crf::performance::coeff_ctx_probe::run() {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-palette-dither" {
        // JPEG-XL 式抖动调色板收益探针（§56 低优先候选）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png-valid")
        };
        if let Err(e) = crf::performance::probe_palette_dither::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-palette-mtf" {
        // 调色板排序 + MTF 编码收益探针（§56 新候选）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png-valid")
        };
        if let Err(e) = crf::performance::probe_palette_mtf::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-palette" {
        // palette 色数分布探针：分量级/像素级唯一值 + RCT 残差域可行性
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_palette::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-dct-simd" {
        // DCT i16 打包 SIMD 可行性探针（§56 新候选）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_dct_simd::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-delta-palette" {
        // JPEG-XL 式 delta palette 收益探针（§8.4）：像素级色数 + delta 条目
        // 编码 + 索引流竞争，与当前最优候选逐帧对比（不写码流）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_delta_palette::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-activity" {
        // P4.2/P4.3/P4.4 activity masking 三旋钮标定探针（默认扫 test/png 下全部 x-y-z 组）
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png")
        };
        if let Err(e) = crf::performance::probe_activity::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-avif-target" {
        // AVIF CQ18 对标标定探针（§6.3）：Q 档扫描 + PSNR/SSIM/最差帧 + 字节
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png\1000")
        };
        if let Err(e) = crf::performance::probe_avif_target::run(&dir) {
            eprintln!("Probe error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--probe-valid-set" {
        // 扩展验证集质量趋势探针（§6.1）：30 张分层首帧跑质量映射
        let dir = if args.len() > 2 {
            args[2].clone()
        } else {
            String::from(r"E:\CRF\test\png-valid")
        };
        if let Err(e) = crf::performance::probe_valid_set::run(&dir) {
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
