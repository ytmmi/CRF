//! DAT.1 跨内容单调性验收探针
//!
//! 规划文档 §6.4：设 Qa > Qb，重标定后必须满足 quality(Qa) >= quality(Qb) 且
//! bytes(Qa) >= bytes(Qb)。本探针对 `test/png` 下 x-y-z 命名的 DAT.1 图像组，
//! 逐质量档位（q75/q80/q85/q90/q95）聚合全组体积与全局 PSNR / 最差帧 PSNR，
//! 并判定单调性。
//!
//! 内存安全：>50 帧或超大分辨率组走流式编解码（O(golden + 单帧 + 码流)），
//! 编码侧用 `StreamingEncoder`（与批量路径逐字节一致），解码侧用
//! `DecodeSession::decode_bytes_streaming` 逐帧回调，原始帧逐文件加载后即释放。

use std::path::{Path, PathBuf};

use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::decoder::session::DecodeSession;
use crate::crf::encoder::streaming::StreamingEncoder;
use crate::crf::LossyOptionsV2Builder;

/// 质量阶梯（围绕 Q_target=q90 的标定范围）
const LADDER: [u16; 5] = [75, 80, 85, 90, 95];

/// 聚合结果：质量档位 → (总字节, 全局PSNR, 最差帧PSNR)
#[derive(Debug, Clone, Copy)]
struct AggRow {
    quality: u16,
    total_bytes: u64,
    global_psnr: f64,
    worst_frame_psnr: f64,
}

pub fn run(root: &str) -> Result<(), String> {
    let groups = enumerate_groups(root)?;
    if groups.is_empty() {
        return Err(format!("{root}: 未发现 x-y-z 命名图像组"));
    }
    println!("=== DAT.1 跨内容单调性验收 ===");
    println!("图像组: {} 个 | 质量阶梯: {:?}\n", groups.len(), LADDER);
    flush();

    let mut rows: Vec<AggRow> = Vec::new();
    for &q in &LADDER {
        println!(">>> q{q} 开始");
        flush();
        let mut total_bytes = 0u64;
        let mut total_mse = 0.0f64;
        let mut total_pixels = 0u64;
        let mut worst_frame_psnr = f64::INFINITY;

        for dir in &groups {
            let name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let paths = collect_paths(dir)?;
            if paths.len() < 2 {
                continue;
            }
            let params = make_params(q)?;
            let encoded = encode_streaming(&paths, &params)?;
            total_bytes += encoded.len() as u64;

            let (mse, pixels, worst) = psnr_streaming(&encoded, &paths)?;
            total_mse += mse;
            total_pixels += pixels;
            worst_frame_psnr = worst_frame_psnr.min(worst);

            let g_psnr = psnr_from_mse(mse, pixels);
            println!(
                "  {name}: {bytes} B, global={g_psnr:.2}dB, worst={worst:.2}dB",
                name = name,
                bytes = encoded.len(),
                g_psnr = g_psnr,
                worst = worst,
            );
            flush();
        }

        let global_psnr = psnr_from_mse(total_mse, total_pixels);
        rows.push(AggRow {
            quality: q,
            total_bytes,
            global_psnr,
            worst_frame_psnr,
        });
        println!(
            "q{q:>3} 汇总: bytes={total_bytes:>11}  global_psnr={global_psnr:6.2}dB  worst_frame_psnr={worst_frame_psnr:6.2}dB\n",
        );
        flush();
    }

    println!("--- 单调性判定 ---");
    let mut all_ok = true;
    for pair in rows.windows(2) {
        let (lo, hi) = (&pair[0], &pair[1]);
        let bytes_ok = hi.total_bytes >= lo.total_bytes;
        let psnr_ok = hi.global_psnr >= lo.global_psnr;
        let bytes_flag = if bytes_ok { "✓" } else { "✗" };
        let psnr_flag = if psnr_ok { "✓" } else { "✗" };
        let verdict = if bytes_ok && psnr_ok {
            "通过"
        } else {
            "倒挂"
        };
        println!(
            "q{} → q{}: bytes {} -> {} [{}], global_psnr {:.2} -> {:.2} [{}]  => {verdict}",
            lo.quality,
            hi.quality,
            lo.total_bytes,
            hi.total_bytes,
            bytes_flag,
            lo.global_psnr,
            hi.global_psnr,
            psnr_flag,
        );
        if !(bytes_ok && psnr_ok) {
            all_ok = false;
        }
    }

    println!(
        "\n=== 结论: {} ===",
        if all_ok {
            "跨内容单调性通过 ✓"
        } else {
            "存在单调性倒挂 ✗"
        }
    );
    Ok(())
}

/// 构造质量档位对应的编码参数（流式路径忽略 input_original_frames 语义，恒 golden 差分）
fn make_params(quality: u16) -> Result<EncodeParams, String> {
    let lossy = LossyOptionsV2Builder::preset(quality * 100)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Med,
        adaptive_prediction: true,
        lossy: Some(lossy),
        input_original_frames: true,
        user_metadata: None,
    })
}

/// 流式编码：逐文件加载原帧 → push_frame → finish，返回完整码流字节。
fn encode_streaming(paths: &[PathBuf], params: &EncodeParams) -> Result<Vec<u8>, String> {
    let mut enc = StreamingEncoder::new(params).map_err(|e| e.to_string())?;
    for p in paths {
        let frame = load_one(p)?;
        enc.push_frame(&frame).map_err(|e| e.to_string())?;
    }
    enc.finish().map_err(|e| e.to_string())
}

/// 流式解码 + 逐帧 PSNR：返回 (总MSE, 总像素数, 最差帧PSNR)。
fn psnr_streaming(encoded: &[u8], paths: &[PathBuf]) -> Result<(f64, u64, f64), String> {
    let mut total_mse = 0.0f64;
    let mut total_pixels = 0u64;
    let mut worst = f64::INFINITY;

    DecodeSession::decode_bytes_streaming(encoded, |i, restored| {
        let orig = load_one(&paths[i]).map_err(crate::crf::error::CrfError::InvalidCodingParams)?;
        let (mse, pixels) = frame_mse(&orig, restored);
        total_mse += mse;
        total_pixels += pixels;
        let p = psnr_from_mse(mse, pixels);
        worst = worst.min(p);
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    Ok((total_mse, total_pixels, worst))
}

/// 单帧 MSE（三通道合并）与像素总数
fn frame_mse(a: &ImageData, b: &ImageData) -> (f64, u64) {
    let mut mse = 0.0f64;
    let mut n = 0u64;
    for (x, y) in a.pixels.iter().zip(b.pixels.iter()) {
        let d = (*x - *y) as f64;
        mse += d * d;
        n += 1;
    }
    (mse, n)
}

fn psnr_from_mse(mse: f64, pixels: u64) -> f64 {
    if pixels == 0 || mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0_f64 * 255.0 / (mse / pixels as f64)).log10()
    }
}

/// 刷新 stdout，保证逐组进度实时可见（后台运行时便于 tail 日志）。
fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// 加载单张图像为 RGB i32 像素（与 bench::load_frames 一致）
fn load_one(path: &Path) -> Result<ImageData, String> {
    let img = image::ImageReader::open(path)
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
    Ok(ImageData {
        width: w as u16,
        height: h as u16,
        bit_depth: 8,
        color_format: ColorFormat::Rgb,
        pixels,
    })
}

/// 收集目录下按文件名排序的图像路径（支持 PNG/JPEG/WebP/BMP）
fn collect_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp")
        })
        .collect();
    entries.sort();
    Ok(entries)
}

/// 枚举 x-y-z 命名的图像组目录（x/y/z 均为数字，至少 2 张图）
fn enumerate_groups(root: &str) -> Result<Vec<PathBuf>, String> {
    let mut groups: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .map(|n| is_x_y_z(&n))
                .unwrap_or(false)
        })
        .collect();
    groups.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    Ok(groups)
}

/// x-y-z 命名判定：三段均非空且全为 ASCII 数字。
fn is_x_y_z(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
