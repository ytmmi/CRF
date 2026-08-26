//! CRF 编码和解码测试模块（支持大分辨率图像组）
//!
//! 内存布局针对超大图（如 4500×8000）优化：
//! - 流式构建差分序列：原始帧仅在瞬间存在，不整体持有
//! - 编码阶段结束后释放差分序列，验证阶段从磁盘重新解码比对
//! - 峰值内存 ≈ 差分序列(5.8GB@4500x8000) + 单次编码临时缓冲

mod batch;
mod probe;

pub use batch::{run_all_tests, run_batch_suite, run_streaming_suite};
pub use probe::run_probe_split_tests;

use crate::crf;
use image::{ImageReader, RgbImage};
use std::fs;
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
pub(crate) fn clean_dir(dir: &str) {
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

/// 收集目录下的图像路径（按文件名排序）
///
/// 支持 PNG / JPEG / WebP / BMP / TIFF 输入。
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
    entries.sort_by_key(|e| e.file_name());
    entries.iter().map(|e| e.path().to_string_lossy().to_string()).collect()
}

/// 加载单张 PNG 为 ImageData
pub(crate) fn load_single_png(path: &str) -> Option<crf::ImageData> {
    let img: RgbImage = ImageReader::open(path).ok()?.decode().ok()?.to_rgb8();
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
    Some(crf::ImageData {
        width: w as u16,
        height: h as u16,
        bit_depth: 8,
        color_format: crf::ColorFormat::Rgb,
        pixels,
    })
}

/// 加载图像路径序列为帧序列
pub(crate) fn load_frame_sequence(paths: &[String]) -> Vec<crf::ImageData> {
    paths
        .iter()
        .filter_map(|p| load_single_png(p))
        .collect()
}

/// 将解码结果与原始 PNG 逐帧比对
pub(crate) fn verify_crf_against_pngs(
    crf_path: &str,
    png_paths: &[String],
    label: &str,
) -> bool {
    let data = std::fs::read(crf_path).expect("无法读取 CRF 文件");
    let result = crf::decode(&data).expect("CRF 解码失败");

    if result.frames.len() != png_paths.len() {
        eprintln!(
            "[{}] 帧数不匹配: CRF={} vs PNG={}",
            label,
            result.frames.len(),
            png_paths.len()
        );
        return false;
    }

    let mut all_ok = true;
    for (i, (frame, png_path)) in result.frames.iter().zip(png_paths.iter()).enumerate() {
        let orig = load_single_png(png_path).expect("无法加载原始 PNG");
        if frame.pixels != orig.pixels {
            let pos = frame
                .pixels
                .iter()
                .zip(orig.pixels.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(usize::MAX);
            eprintln!(
                "[{}] 帧{} 不一致！首个差异索引 {} (CRF={} vs PNG={})",
                label,
                i,
                pos,
                frame.pixels[pos],
                orig.pixels[pos]
            );
            all_ok = false;
        }
    }
    if all_ok {
        println!("[{}] 全部 {} 帧校验通过 ✓", label, result.frames.len());
    }
    all_ok
}

/// 保存单帧为无损图像
pub(crate) fn save_frame_lossless(
    frame: &crf::ImageData,
    path: &str,
    fmt: OutputFormat,
) {
    match fmt {
        OutputFormat::Png => {
            let mut img: RgbImage = ImageReader::open(path)
                .ok()
                .and_then(|r| r.decode().ok())
                .map(|d: image::DynamicImage| d.to_rgb8())
                .unwrap_or_else(|| {
                    // 创建新图
                    RgbImage::new(frame.width as u32, frame.height as u32)
                });
            for y in 0..frame.height as u32 {
                for x in 0..frame.width as u32 {
                    let idx = (y as usize * frame.width as usize + x as usize) * 3;
                    let r = frame.pixels[idx].clamp(0, 255) as u8;
                    let g = frame.pixels[idx + 1].clamp(0, 255) as u8;
                    let b = frame.pixels[idx + 2].clamp(0, 255) as u8;
                    img.put_pixel(x, y, image::Rgb([r, g, b]));
                }
            }
            img.save(path).expect("保存 PNG 失败");
        }
        OutputFormat::WebP => {
            // 复用 PNG 路径，实际用 WebP 编码
            let mut img: RgbImage = RgbImage::new(frame.width as u32, frame.height as u32);
            for y in 0..frame.height as u32 {
                for x in 0..frame.width as u32 {
                    let idx = (y as usize * frame.width as usize + x as usize) * 3;
                    let r = frame.pixels[idx].clamp(0, 255) as u8;
                    let g = frame.pixels[idx + 1].clamp(0, 255) as u8;
                    let b = frame.pixels[idx + 2].clamp(0, 255) as u8;
                    img.put_pixel(x, y, image::Rgb([r, g, b]));
                }
            }
            // WebP 无损编码
            let webp_path = path.replace(".png", ".webp");
            img.save(&webp_path).expect("保存 WebP 失败");
        }
    }
}
