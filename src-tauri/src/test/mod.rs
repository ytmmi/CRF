//! CRF 编码和解码测试模块（支持大分辨率图像组）
//!
//! 内存布局针对超大图（如 4500×8000）优化：
//! - 流式构建差分序列：原始帧仅在瞬间存在，不整体持有
//! - 编码阶段结束后释放差分序列，验证阶段从磁盘重新解码比对
//! - 峰值内存 ≈ 差分序列(5.8GB@4500x8000) + 单次编码临时缓冲

use crate::crf;
use image::{ImageReader, RgbImage};
use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// 默认测试图片目录（快速验证图片组）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
const DEFAULT_TEST_PNG_DIR: &str = r"E:\CRF\test\png\1000";
/// CRF 输出目录
const TEST_CRF_OUTPUT_DIR: &str = r"E:\CRF\test\output\crf";
/// 解码图片输出目录
const TEST_IMG_OUTPUT_DIR: &str = r"E:\CRF\test\output\img";

/// 无损输出图像格式（CRF 解码还原帧的落盘格式）
///
/// - Png（默认）/ WebP：均为无损容器，保存结果重新解码后与帧像素逐位一致。
///
/// 可通过环境变量 CRF_OUTPUT_FORMAT=webp 切换默认输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Png,
    WebP,
}

impl OutputFormat {
    fn from_env() -> Self {
        match std::env::var("CRF_OUTPUT_FORMAT").as_deref() {
            Ok(s) if s.eq_ignore_ascii_case("webp") => OutputFormat::WebP,
            _ => OutputFormat::Png,
        }
    }

    fn ext(self) -> &'static str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::WebP => "webp",
        }
    }
}

/// 测试方案定义
struct Scheme {
    /// 输出 CRF 文件名
    file: &'static str,
    /// 固定预测模式（adaptive=true 时作为回退设置）
    mode: crf::PredictionMode,
    /// 是否启用逐帧+条带+三平面自适应
    adaptive: bool,
    /// 有损质量档位：None=无损；Some(q) 启用真有损量化（q∈1..=100）
    quality: Option<u8>,
}

/// 调试：打印两张图各自与差分的真实值域，定位越界来源
pub fn debug_pixels(path_a: &str, path_b: &str) {
    let a = load_single_png(path_a).expect("加载 A 失败");
    let b = load_single_png(path_b).expect("加载 B 失败");
    println!(
        "A: {}x{} len={} min={} max={}",
        a.width,
        a.height,
        a.pixels.len(),
        a.pixels.iter().min().unwrap(),
        a.pixels.iter().max().unwrap()
    );
    println!("A 前9分量: {:?}", &a.pixels[..9.min(a.pixels.len())]);
    println!(
        "B: {}x{} len={} min={} max={}",
        b.width,
        b.height,
        b.pixels.len(),
        b.pixels.iter().min().unwrap(),
        b.pixels.iter().max().unwrap()
    );
    println!("B 前9分量: {:?}", &b.pixels[..9.min(b.pixels.len())]);

    if a.pixels.len() == b.pixels.len() {
        let mut dmin = i32::MAX;
        let mut dmax = i32::MIN;
        for (x, y) in a.pixels.iter().zip(b.pixels.iter()) {
            let d = x - y;
            dmin = dmin.min(d);
            dmax = dmax.max(d);
        }
        println!("A-B 差分范围: [{}, {}]", dmin, dmax);
        let diff: Vec<i32> = a
            .pixels
            .iter()
            .zip(b.pixels.iter())
            .map(|(x, y)| x - y)
            .collect();
        println!("差分 前9分量: {:?}", &diff[..9]);
    } else {
        println!("!! 长度不一致: A={} B={}", a.pixels.len(), b.pixels.len());
    }
}

/// 清空目录内容（保留目录本身），用于每次测试前重置输出
fn clean_dir(dir: &str) {
    let _ = fs::create_dir_all(dir);
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let _ = fs::remove_dir_all(&p);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
    }
    println!("已清空输出目录: {}", dir);
}

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
    let t_total = std::time::Instant::now();

    // 组名 = 图像目录末级名称（如 1000 / 2000 / c / d）
    let group_name = Path::new(png_dir)
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
fn run_streaming_suite(paths: &[String], crf_out_dir: &str, img_out_dir: &str) {
    println!("模式: 流式（CRF_STREAMING=1，内存 O(golden+单帧+码流)）\n");

    for (label, quality) in [
        ("streaming_adaptive", None),
        ("streaming_q90", Some(90u8)),
        ("streaming_q75", Some(75u8)),
    ] {
        let t0 = std::time::Instant::now();
        let params = crate::crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crate::crf::PredictionMode::Average,
            adaptive_prediction: true,
            lossy_quality: quality,
            lossy_tuning: None,
            input_original_frames: true,
            user_metadata: None,
        };
        let mut enc = match crate::crf::encoder::streaming::StreamingEncoder::new(&params) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("  {} 编码器创建失败: {}", label, err);
                continue;
            }
        };

        // 逐帧推送（首帧自动成为 golden 无损基准；原始帧离开作用域，
        // 内存峰值 O(golden+单帧)）
        let mut frame_count = 0usize;
        for path in paths {
            let img = match load_single_png(path) {
                Some(img) => img,
                None => panic!("无法加载 {}", path),
            };
            enc.push_frame(&img)
                .unwrap_or_else(|e| panic!("{} 推送失败: {}", label, e));
            frame_count += 1;
        }

        let encoded = match enc.finish() {
            Ok(b) => b,
            Err(err) => {
                eprintln!("  {} finish 失败: {}", label, err);
                continue;
            }
        };
        let out_path = format!("{}/{}.crf", crf_out_dir, label);
        std::fs::write(&out_path, &encoded).expect("写入 CRF 文件失败");
        println!(
            "  {}: {} 帧 → {:.2} MB（耗时 {:.1}s）",
            label,
            frame_count,
            encoded.len() as f64 / 1048576.0,
            t0.elapsed().as_secs_f64()
        );

        // 磁盘级校验（复用现有验证逻辑）
        let ok = verify_crf_against_pngs(
            &out_path,
            paths,
            OutputFormat::from_env(),
            quality,
            &format!("{}/{}", img_out_dir, label),
        );
        println!(
            "  {} -> {}",
            label,
            if ok {
                "✓ 校验通过"
            } else {
                "✗ 校验失败"
            }
        );
    }
}

/// 批量编码套件（原逻辑）：全帧驻留 + 多方案竞争
fn run_batch_suite(paths: &[String], crf_out_dir: &str, img_out_dir: &str) {
    println!("共 {} 张图片，开始流式构建差分序列...", paths.len());

    // 有损源提示：JPEG/有损 WebP 的压缩噪声会抬高无损残差能量，
    // 真有损模式的死区量化恰可滤除该噪声（见规范 §5.2"量化即滤噪"）
    let src_ext = Path::new(&paths[0])
        .extension()
        .map_or_else(String::new, |e| e.to_string_lossy().to_lowercase());
    if matches!(src_ext.as_str(), "jpg" | "jpeg" | "webp") {
        println!(
            "提示: 检测到有损源容器(.{})——其解码像素含压缩噪声，\n\
             \x20      无损模式将忠实保存该噪声；建议同时测试真有损档位\n\
             \x20      （q75/q50 可滤除源噪声，实测体积可低于源文件 60%+）。",
            src_ext
        );
    }
    let frames = load_frame_sequence(paths);
    let frame_mb = frames[0].pixels.len() as f64 * 4.0 / 1048576.0;
    println!(
        "差分序列构建完成: {} 帧 ({}x{}, 每帧约 {:.0} MB 内存)\n",
        frames.len(),
        frames[0].width,
        frames[0].height,
        frame_mb
    );

    // 原始帧输入：差分由编码器内部闭环完成（首帧参考，无误差累积）

    // ===== 阶段二：逐方案编码并写盘 =====
    fs::create_dir_all(crf_out_dir).expect("无法创建 CRF 输出目录");

    // JPEG 源噪声感知开关（CRF_NOISE_ADAPTIVE=1 开启，A/B 对比用）
    let noise_adaptive = std::env::var("CRF_NOISE_ADAPTIVE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    // golden 首帧有损开关（CRF_GOLDEN_LOSSY=1：首帧按档位量化，
    // 超小序列体积收益大；默认保持无损）
    let golden_lossy = std::env::var("CRF_GOLDEN_LOSSY")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    let lossy_tuning = if noise_adaptive {
        Some(crf::LossyTuning {
            noise_adaptive: true,
            ..crf::LossyTuning::default()
        })
    } else {
        None
    };
    if noise_adaptive {
        println!("噪声感知: 已启用（差分残差条带级软阈值归一化）");
    }

    let schemes = [
        Scheme {
            file: "test_hybrid_none.crf",
            mode: crf::PredictionMode::None,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_hybrid_h.crf",
            mode: crf::PredictionMode::Horizontal,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_hybrid_v.crf",
            mode: crf::PredictionMode::Vertical,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_hybrid_a.crf",
            mode: crf::PredictionMode::Average,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_hybrid_dc.crf",
            mode: crf::PredictionMode::DC,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_hybrid_med.crf",
            mode: crf::PredictionMode::Med,
            adaptive: false,
            quality: None,
        },
        Scheme {
            file: "test_adaptive.crf",
            mode: crf::PredictionMode::Average,
            adaptive: true,
            quality: None,
        },
        // 视觉无损档（v1.12，对标 AVIF cq18 / HEIF crf30）：DCT 高频轻滤
        Scheme {
            file: "test_lossy_q95.crf",
            mode: crf::PredictionMode::Average,
            adaptive: true,
            quality: Some(95),
        },
        // 真有损对照档位（与无损并行可选）
        Scheme {
            file: "test_lossy_q90.crf",
            mode: crf::PredictionMode::Average,
            adaptive: true,
            quality: Some(90),
        },
        Scheme {
            file: "test_lossy_q75.crf",
            mode: crf::PredictionMode::Average,
            adaptive: true,
            quality: Some(75),
        },
        // 扩展档位（CRF_EXTRA_QUALITY=<q> 时在下方追加，用于低质源压测）
    ];
    let mut schemes: Vec<Scheme> = schemes.into_iter().collect();
    // 实验工具参数（显式 opt-in）：CRF_ONLY_SCHEME=<文件名子串> 只保留匹配
    // 的方案（如 q95），供标定扫描裁剪运行时间；不影响默认全量行为。
    if let Ok(only) = std::env::var("CRF_ONLY_SCHEME") {
        let needle = only.trim().to_string();
        if !needle.is_empty() {
            schemes.retain(|s| s.file.contains(&needle));
        }
    }
    if let Ok(q) = std::env::var("CRF_EXTRA_QUALITY") {
        if let Ok(qv) = q.trim().parse::<u8>() {
            schemes.push(Scheme {
                file: Box::leak(format!("test_lossy_q{}.crf", qv).into_boxed_str()),
                mode: crf::PredictionMode::Average,
                adaptive: true,
                quality: Some(qv),
            });
        }
    }

    let mut written: Vec<(String, usize)> = Vec::new(); // (路径, 方案索引)
    for (idx, s) in schemes.iter().enumerate() {
        println!(
            "--- 方案{}：{} ---",
            idx + 1,
            match (s.adaptive, s.quality) {
                (true, Some(q)) => format!("真有损 q={}（自适应管线）", q),
                (true, None) => "逐帧+条带自适应预测".to_string(),
                (false, _) => format!("{:?} 预测", s.mode),
            }
        );
        let mut tuning = lossy_tuning.clone();
        if golden_lossy {
            let mut t = tuning.take().unwrap_or_default();
            t.golden_lossless = false;
            tuning = Some(t);
        }
        // 实验工具参数（显式 opt-in，规范 §6）：标定扫描用环境变量注入。
        // CRF_DEADZONE=<-32..32>：亮度死区偏置（闭环路径：正=单侧加宽负残差死区）
        if let Ok(v) = std::env::var("CRF_DEADZONE") {
            if let Ok(bias) = v.trim().parse::<i8>() {
                if (-32..=32).contains(&bias) {
                    let mut t = tuning.take().unwrap_or_default();
                    t.deadzone_bias = bias;
                    tuning = Some(t);
                }
            }
        }
        // CRF_CHROMA_DEADZONE=<-32..32>：色度死区偏置独立通道
        if let Ok(v) = std::env::var("CRF_CHROMA_DEADZONE") {
            if let Ok(bias) = v.trim().parse::<i8>() {
                if (-32..=32).contains(&bias) {
                    let mut t = tuning.take().unwrap_or_default();
                    t.chroma_deadzone_bias = Some(bias);
                    tuning = Some(t);
                }
            }
        }
        // CRF_NOISE_ADAPTIVE=1：启用 JPEG 源噪声感知软阈值 + per-band 步长
        //（色度 band steps 随之启用——评审 §11.1 第二项验证开关）
        if let Ok(v) = std::env::var("CRF_NOISE_ADAPTIVE") {
            if v.trim() == "1" {
                let mut t = tuning.take().unwrap_or_default();
                t.noise_adaptive = true;
                tuning = Some(t);
            }
        }
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: s.mode,
            adaptive_prediction: s.adaptive,
            lossy_quality: s.quality,
            lossy_tuning: tuning,
            input_original_frames: true,
            user_metadata: Some(format!(
                "{} frames, {}x{}",
                frames.len(),
                frames[0].width,
                frames[0].height
            )),
        };

        let t0 = std::time::Instant::now();
        match crf::encode_sequence(&frames, &params) {
            Ok(encoded) => {
                let output_path = format!("{}/{}", crf_out_dir, s.file);
                fs::write(&output_path, &encoded).expect("写入 CRF 文件失败");
                println!(
                    "  编码成功: {:.2} MB (耗时 {:.1}s)",
                    encoded.len() as f64 / 1048576.0,
                    t0.elapsed().as_secs_f64()
                );
                written.push((s.file.to_string(), idx));
            }
            Err(e) => eprintln!("  编码失败: {}", e),
        }
    }

    // 释放差分序列（约 5.8GB），为解码验证腾出内存
    drop(frames);

    // ===== 阶段三：磁盘级无损验证 =====
    println!("\n--- 无损验证（解码 → 流式还原 → 与原始图像逐帧比对） ---");
    let out_fmt = OutputFormat::from_env();
    println!(
        "  还原输出格式: {}（CRF_OUTPUT_FORMAT=webp 可切换）",
        match out_fmt {
            OutputFormat::Png => "PNG",
            OutputFormat::WebP => "WebP",
        }
    );

    let mut results: Vec<(String, bool, Option<u8>)> = Vec::new();
    for (file, idx) in &written {
        let crf_path = format!("{}/{}", crf_out_dir, file);
        let ok = verify_crf_against_pngs(
            &crf_path,
            paths,
            out_fmt,
            schemes[*idx].quality,
            img_out_dir,
        );
        println!(
            "  {} -> {}",
            file,
            if ok {
                "✓ 校验通过"
            } else {
                "✗ 校验失败"
            }
        );
        results.push((file.to_string(), ok, schemes[*idx].quality));
    }

    // ===== 阶段四：汇总 =====
    println!("\n--- 结果汇总 ---");
    for (file, ok, q) in &results {
        let size = fs::metadata(format!("{}/{}", crf_out_dir, file))
            .map(|m| m.len())
            .unwrap_or(0);
        let mode_tag = match q {
            None => "LOSSLESS",
            Some(qv) if *qv >= 96 => "NEAR-LOSSLESS",
            Some(_) => "LOSSY",
        };
        println!(
            "  {:<26} {:>9.2} MB  [{:^14}] {}",
            file,
            size as f64 / 1048576.0,
            mode_tag,
            if *ok { "PASS" } else { "FAIL" }
        );
    }
    println!(
        "\n产物位置:\n  CRF: {}\n  还原帧: {}\\<方案名\\/",
        crf_out_dir, img_out_dir
    );
}

/// 收集目录下的图像路径（按文件名排序）
///
/// 支持 PNG / JPEG / WebP / BMP / TIFF 输入。
/// 运行超块分区先导探针：条带列方向二分收益测量（第四批 #1 先导验证）
///
/// 对图像组每帧构造与路径 G 相同的 RCT 域编码输入（首帧 = rct_forward(原图)，
/// 差分帧 = rct_forward(frame − frame0)），在 {32, 64} 两档条带高度下测量
/// 「整条带 vs 列方向二分」的熵编码字节差。纯编码端本地测量，不产出码流。
pub fn run_probe_split_tests(png_dir: &str) {
    use crate::crf::encoder::banded::probe_band_split_savings;

    println!("=== 超块先导探针：条带列方向二分收益测量 ===");
    println!("图像组: {}\n", png_dir);
    let paths = collect_png_paths(png_dir);
    if paths.len() < 2 {
        eprintln!("需要至少 2 张图片，实际 {}", paths.len());
        return;
    }
    let frames = load_frame_sequence(&paths);
    let components = frames[0].color_format.component_count();

    for band_height in [32usize, 64usize] {
        let mut total_full = 0usize;
        let mut total_split = 0usize;
        let mut total_bands = 0usize;
        let mut won_bands = 0usize;
        println!("--- 条带高度 {} 行 ---", band_height);
        for (i, frame) in frames.iter().enumerate() {
            // 与路径 G 一致的 RCT 域编码输入
            let diff_rgb: Vec<i32> = if i == 0 {
                frame.pixels.clone()
            } else {
                frame
                    .pixels
                    .iter()
                    .zip(frames[0].pixels.iter())
                    .map(|(a, b)| a - b)
                    .collect()
            };
            let eff = crate::crf::format::rct_forward(&diff_rgb, components).expect("RCT 失败");
            let img = crf::ImageData {
                width: frame.width,
                height: frame.height,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels: eff,
            };
            let probe = probe_band_split_savings(&img, band_height)
                .unwrap_or_else(|e| panic!("探针失败 帧{}: {}", i, e));
            let saved = probe.full_bytes.saturating_sub(probe.split_bytes);
            let pct = if probe.full_bytes > 0 {
                saved as f64 / probe.full_bytes as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  帧{:2}: 整带 {:9} B | 二分 {:9} B | 节省 {:8} B ({:5.2}%) | 二分胜出 {}/{} 条带",
                i,
                probe.full_bytes,
                probe.split_bytes,
                saved,
                pct,
                probe.bands_won,
                probe.band_count
            );
            total_full += probe.full_bytes;
            total_split += probe.split_bytes;
            won_bands += probe.bands_won;
            total_bands += probe.band_count;
        }
        let saved_total = total_full.saturating_sub(total_split);
        let pct_total = if total_full > 0 {
            saved_total as f64 / total_full as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  汇总: banded 载荷 {:9} → {:9} B，节省 {:.2}%（二分胜出 {}/{} 条带）",
            total_full, total_split, pct_total, won_bands, total_bands
        );
    }
    println!("\n=== 探针完成 ===");
}

pub(crate) fn collect_png_paths(dir: &str) -> Vec<String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("无法读取图像目录")
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
    entries
        .iter()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect()
}

/// 加载单张 PNG 为 RGB ImageData
pub(crate) fn load_single_png(path: &str) -> Option<crf::ImageData> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("打开失败 {}: {}", path, e);
            return None;
        }
    };
    let reader = BufReader::new(file);
    let img = match ImageReader::new(reader).with_guessed_format() {
        Ok(guessed) => match guessed.decode() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("解码失败 {}: {}", path, e);
                return None;
            }
        },
        Err(e) => {
            eprintln!("格式猜测失败 {}: {}", path, e);
            return None;
        }
    };

    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let pixels: Vec<i32> = rgb
        .pixels()
        .flat_map(|p| [p[0] as i32, p[1] as i32, p[2] as i32])
        .collect();

    Some(crf::ImageData {
        width: w as u16,
        height: h as u16,
        bit_depth: 8,
        color_format: crf::ColorFormat::Rgb,
        pixels,
    })
}

/// 加载原始帧序列（不做预差分）
///
/// 差分由编码器内部完成（input_original_frames=true 时采用首帧参考闭环，
/// 量化误差不沿链累积）。原始帧仅在加载瞬间存在，序列整体一次性持有。
pub(crate) fn load_frame_sequence(paths: &[String]) -> Vec<crf::ImageData> {
    let mut frames: Vec<crf::ImageData> = Vec::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        print!("加载 [{}/{}] ... \r", i + 1, paths.len());
        let img = load_single_png(p).unwrap_or_else(|| panic!("无法加载 {}", p));
        if i > 0 {
            assert_eq!(frames[0].pixels.len(), img.pixels.len(), "帧尺寸不一致");
        }
        frames.push(img);
    }
    println!();
    frames
}

/// 解码 CRF 并与磁盘上的原始图像逐帧流式比对/评估
///
/// lossy_quality=None：无损方案，逐位比对，任何不一致即 FAIL；
/// Some(q)：有损方案，计算最大误差与 PSNR 报告（不参与 PASS/FAIL 判定）。
/// 全部方案的还原帧落盘至 `img_out_dir/<crf 名>/`。
fn verify_crf_against_pngs(
    crf_path: &str,
    png_paths: &[String],
    out_fmt: OutputFormat,
    lossy_quality: Option<u8>,
    img_out_dir: &str,
) -> bool {
    println!("验证: {}", crf_path);

    let data = match fs::read(crf_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  读取失败: {}", e);
            return false;
        }
    };
    let result = match crf::decode_from_bytes(&data) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  解码失败: {}", e);
            return false;
        }
    };

    // 工具胜出率统计（规范 §6.2 必报指标）：帧头偏移 +8 处为 frame_type。
    // frame_type=0/1 熵编码直连、2 条带、3 planar、4 palette、5 CABAC、6 DCT、7 ITBC
    if result.frames.len() == result.frame_index.len() {
        let mut type_counts: std::collections::BTreeMap<u8, usize> = Default::default();
        for entry in &result.frame_index {
            let off = entry.offset as usize;
            if off + 8 < data.len() {
                *type_counts.entry(data[off + 8]).or_insert(0) += 1;
            }
        }
        let dist = type_counts
            .iter()
            .map(|(t, c)| format!("type{}×{}", t, c))
            .collect::<Vec<_>>()
            .join(" ");
        println!("  帧类型分布: {}", dist);
    }
    drop(data);

    if result.frames.len() != png_paths.len() {
        eprintln!("  帧数不符: {} vs {}", result.frames.len(), png_paths.len());
        return false;
    }

    let crf_filename = Path::new(crf_path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let dump_dir = format!("{}/{}", img_out_dir, crf_filename);
    let dump_frames = true;

    if dump_frames {
        // 清理历史帧，避免不同图像组/帧数变化时残留混淆
        let _ = fs::create_dir_all(&dump_dir);
        if let Ok(entries) = fs::read_dir(&dump_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let is_frame_img = p.extension().is_some_and(|e| {
                    let e = e.to_string_lossy().to_ascii_lowercase();
                    matches!(e.as_str(), "png" | "webp" | "jpg" | "jpeg")
                });
                if is_frame_img {
                    let _ = fs::remove_file(&p);
                }
            }
        }
    }

    let mut all_ok = true;
    // golden[i]=true：该帧以"首帧 + 残差累加"为还原式；链式帧 = 前一帧原帧 + 残差
    let golden_refs = result.frame_golden_refs.clone();
    // P0 闭环修复（规范 §5.3 / §8.2）：golden 差分的还原基准一律使用 CRF
    // 文件自身解码出的 frame0，禁止回读源 PNG——有损 golden 时源图参考
    // 会掩盖真实的文件自包含解码质量。无损 golden 时两者逐位相同。
    let decoded_golden = result.frames[0].clone();
    let mut current: Option<crf::ImageData> = None;

    for (i, dec) in result.frames.into_iter().enumerate() {
        let is_golden = golden_refs.get(i).copied().unwrap_or(false);

        // 还原式：chain 帧 = prev + residual；golden 帧 = 解码首帧 + residual
        let restored = match (&current, is_golden) {
            (None, _) => dec,
            (Some(_), true) => {
                // golden 差分帧：以文件自身解码出的 frame0（G_hat）为基准。
                // 有损 golden 时 G_hat 含量化误差——这正是端到端真实质量，
                // 不得用源图替换（历史缺陷曾在此重新加载源首帧 PNG）。
                let pixels: Vec<i32> = decoded_golden
                    .pixels
                    .iter()
                    .zip(dec.pixels.iter())
                    .map(|(a, b)| a + b)
                    .collect();
                crf::ImageData {
                    width: dec.width,
                    height: dec.height,
                    bit_depth: dec.bit_depth,
                    color_format: dec.color_format,
                    pixels,
                }
            }
            (Some(prev), false) => {
                let pixels: Vec<i32> = prev
                    .pixels
                    .iter()
                    .zip(dec.pixels.iter())
                    .map(|(a, b)| a + b)
                    .collect();
                crf::ImageData {
                    width: dec.width,
                    height: dec.height,
                    bit_depth: dec.bit_depth,
                    color_format: dec.color_format,
                    pixels,
                }
            }
        };

        // 与磁盘原始图像比对（每次仅载入一张）
        match load_single_png(&png_paths[i]) {
            Some(orig) => {
                if orig.pixels != restored.pixels {
                    if lossy_quality.is_none() {
                        let diff_pos = orig
                            .pixels
                            .iter()
                            .zip(restored.pixels.iter())
                            .position(|(a, b)| a != b)
                            .unwrap_or(usize::MAX);
                        eprintln!("  帧 {} 不一致！首个差异位于像素索引 {}", i, diff_pos);
                        all_ok = false;
                    } else {
                        // 有损方案：误差统计 + PSNR
                        let mut max_err: u32 = 0;
                        let mut sq_sum: f64 = 0.0;
                        for (a, b) in orig.pixels.iter().zip(restored.pixels.iter()) {
                            let d = (a - b).unsigned_abs();
                            max_err = max_err.max(d);
                            sq_sum += (d as f64) * (d as f64);
                        }
                        let mse = sq_sum / orig.pixels.len() as f64;
                        let psnr = if mse > 0.0 {
                            10.0 * (255.0_f64 * 255.0 / mse).log10()
                        } else {
                            f64::INFINITY
                        };
                        println!(
                            "  帧 {} 有损重建: max_err={} PSNR={:.2}dB",
                            i, max_err, psnr
                        );
                    }
                }
            }
            None => all_ok = false,
        }

        if dump_frames {
            fs::create_dir_all(&dump_dir).ok();
            let out = format!("{}/frame_{:03}.{}", dump_dir, i, out_fmt.ext());
            save_frame_lossless(&restored, &out, out_fmt);
        }

        current = Some(restored);
    }

    if dump_frames && all_ok {
        println!(
            "  还原帧已落盘（{} 格式，无损）: {}",
            match out_fmt {
                OutputFormat::Png => "PNG",
                OutputFormat::WebP => "WebP",
            },
            dump_dir
        );
    }
    all_ok
}

/// 无损保存 RGB 帧（PNG / WebP-VP8L）
///
/// 两种格式均为无损容器：保存结果重新解码后与帧像素逐位一致。
/// WebP 使用 image crate 的 VP8L 无损编码器。
fn save_frame_lossless(frame: &crf::ImageData, path: &str, fmt: OutputFormat) {
    let w = frame.width as u32;
    let h = frame.height as u32;
    let mut bytes = Vec::with_capacity(frame.pixels.len());
    for &v in &frame.pixels {
        bytes.push(v.clamp(0, 255) as u8);
    }
    let img = match RgbImage::from_raw(w, h, bytes) {
        Some(img) => img,
        None => {
            eprintln!("保存失败 {}: 尺寸与数据长度不匹配", path);
            return;
        }
    };

    match fmt {
        OutputFormat::Png => {
            if let Err(e) = img.save(path) {
                eprintln!("保存失败 {}: {}", path, e);
            }
        }
        OutputFormat::WebP => {
            use image::codecs::webp::WebPEncoder;
            use image::ExtendedColorType;
            let file = match File::create(path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("创建文件失败 {}: {}", path, e);
                    return;
                }
            };
            let mut writer = std::io::BufWriter::new(file);
            let enc = WebPEncoder::new_lossless(&mut writer);
            if let Err(e) = enc.encode(img.as_raw(), w, h, ExtendedColorType::Rgb8) {
                eprintln!("WebP 编码失败 {}: {}", path, e);
            }
        }
    }
}

/// 分析差分数据分布（流式统计）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
fn analyze_residual_distribution(residuals: &[crf::ImageData]) {
    println!("--- 差分数据分布分析 ---");

    let mut total: u64 = 0;
    let mut zero_count: u64 = 0;
    let mut small_count: u64 = 0;
    let mut medium_count: u64 = 0;
    let mut large_count: u64 = 0;
    let mut min_val = i32::MAX;
    let mut max_val = i32::MIN;
    let mut sum: i64 = 0;
    let mut sum_sq: i64 = 0;

    for frame in residuals {
        for &v in &frame.pixels {
            total += 1;
            sum += v as i64;
            sum_sq += (v as i64) * (v as i64);

            if v < min_val {
                min_val = v;
            }
            if v > max_val {
                max_val = v;
            }

            if v == 0 {
                zero_count += 1;
            }
            if v.abs() <= 1 {
                small_count += 1;
            }
            if v.abs() <= 10 {
                medium_count += 1;
            }
            if v.abs() > 100 {
                large_count += 1;
            }
        }
    }

    let mean_val = sum as f64 / total as f64;
    let variance = sum_sq as f64 / total as f64 - mean_val * mean_val;
    let std_dev = variance.sqrt();

    println!("总像素值数量: {}", total);
    println!("差分值范围: [{}, {}]", min_val, max_val);
    println!("平均值: {:.2}, 标准差: {:.2}", mean_val, std_dev);
    println!("分布统计:");
    println!(
        "  零值占比: {:.2}%",
        zero_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值≤1占比: {:.2}%",
        small_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值≤10占比: {:.2}%",
        medium_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值>100占比: {:.4}%",
        large_count as f64 / total as f64 * 100.0
    );
    println!();
}

/// 实验探针：无损路径放开 DCT(Q=1) 候选的价值验证
///
/// 比较「空间域 Med 预测 + RLE」vs「DCT(Q=1 恒等) + CABAC」的载荷大小，
/// 覆盖三类合成内容（平滑渐变 / 斜线图案 / 随机纹理）。
#[test]
fn probe_lossless_dct_candidate_value() {
    use crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive;
    use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;

    let cases: [(&str, Vec<i32>, usize, usize); 3] = [
        (
            "平滑渐变",
            {
                let mut v = Vec::with_capacity(128 * 128);
                for y in 0..128usize {
                    for x in 0..128usize {
                        v.push((50 + x / 2 + y / 4) as i32);
                    }
                }
                v
            },
            128,
            128,
        ),
        (
            "斜线图案",
            {
                let mut v = vec![0i32; 128 * 128];
                for y in 0..128usize {
                    for x in 0..128usize {
                        v[y * 128 + x] = (((x + y) % 32) * 7) as i32;
                    }
                }
                v
            },
            128,
            128,
        ),
        (
            "随机纹理",
            {
                let mut state = 0x12345678u64;
                (0..128 * 128)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        ((state >> 33) % 251) as i32 - 125
                    })
                    .collect()
            },
            128,
            128,
        ),
    ];

    for (name, px, w, h) in cases {
        let med_res = crf::apply_prediction(&px, w, h, 1, crf::PredictionMode::Med);
        let (rle_buf, _) = encode_frame_rle_golomb_adaptive(&med_res).unwrap();
        let rle_bytes = rle_buf.len();

        let coeffs4 = crate::crf::encoder::dct_path::dct_quantize_interleaved_bs(
            &px, w, h, 1, 1, 4, 4, false, false,
        );
        let (p4, _) = encode_frame_rle_cabac_adaptive(&coeffs4, None).unwrap();
        let cabac_dct4 = p4.len();

        let coeffs8 = crate::crf::encoder::dct_path::dct_quantize_interleaved_bs(
            &px, w, h, 1, 1, 8, 8, false, false,
        );
        let (p8, _) = encode_frame_rle_cabac_adaptive(&coeffs8, None).unwrap();
        let cabac_dct8 = p8.len();

        println!(
            "{}: RLE(Med)={} | DCT4={} (Δ{}) | DCT8={} (Δ{})",
            name,
            rle_bytes,
            cabac_dct4,
            rle_bytes as i64 - cabac_dct4 as i64,
            cabac_dct8,
            rle_bytes as i64 - cabac_dct8 as i64,
        );
    }
}
