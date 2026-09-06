//! P4.2/P4.3/P4.4 activity masking 三旋钮标定探针
//!
//! 在 DAT.1 扩展数据集（`test/png` 下 x-y-z 命名图像组）上对三旋钮做
//! **单旋钮边际扫描**（其余中性 100），输出每档体积 + 全局 PSNR + 最差帧
//! PSNR，供「质量护栏内体积最小」的默认值决策。
//!
//! 三旋钮语义（`estimate_band_activity_steps`，§28 reference 修复后正确激活）：
//! - `activity_masking_x100`（纹理，>100 增步长省码率）；
//! - `flat_area_protection_x100`（平坦，>100 减步长防 banding）；
//! - `edge_protection_x100`（边缘，>100 减步长防 ringing）。
//!
//! 判定：activity 单独能否在 q90 护栏内拿到 ≥3% 体积下降；flat/edge 若
//! 基线无 banding/ringing（保护对象不存在）则体积白增、证伪。

use std::path::{Path, PathBuf};

use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::decoder::session::DecodeSession;
use crate::crf::LossyOptionsV2Builder;

/// 标定档位固定 Q_target=q90（quality_x100 = 9000）
const QUALITY: u16 = 9000;

/// 单旋钮扫描矩阵（`validate_perceptual` 上限 200）
const ACTIVITY_SCAN: [u16; 4] = [125, 150, 175, 200];
const FLAT_SCAN: [u16; 3] = [125, 150, 200];
const EDGE_SCAN: [u16; 3] = [125, 150, 200];

/// 单次配置的聚合结果
#[derive(Debug, Clone)]
struct ScanRow {
    label: String,
    total_bytes: u64,
    global_psnr: f64,
    worst_psnr: f64,
}

pub fn run(root: &str) -> Result<(), String> {
    let all_groups = enumerate_groups(root)?;
    // 可选：CRF_ACTIVITY_GROUPS 逗号分隔组名，只跑指定组（各数量级抽样，加速标定）
    let groups: Vec<PathBuf> = match std::env::var("CRF_ACTIVITY_GROUPS") {
        Ok(spec) if !spec.trim().is_empty() => {
            let wanted: Vec<String> = spec.split(',').map(|s| s.trim().to_string()).collect();
            all_groups
                .into_iter()
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .map(|n| wanted.contains(&n))
                        .unwrap_or(false)
                })
                .collect()
        }
        _ => all_groups,
    };
    if groups.is_empty() {
        return Err(format!("{root}: 未发现匹配的 x-y-z 命名图像组"));
    }
    // 环境变量分阶段：activity(核心) / flat-edge / full(默认)
    let phase = std::env::var("CRF_ACTIVITY_PHASE").unwrap_or_else(|_| "full".to_string());
    // 环境变量限制组帧数（跳过超过 N 帧的大组，加速核心扫描）
    let max_frames: usize = std::env::var("CRF_ACTIVITY_MAX_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    println!("=== P4.2/P4.3/P4.4 activity masking 三旋钮标定 ===");
    println!(
        "图像组: {} 个 | 档位: q90 | phase: {phase} | max_frames: {max_frames}\n",
        groups.len()
    );
    flush();

    let run_activity = phase == "activity" || phase == "full";
    let run_flat_edge = phase == "flat-edge" || phase == "full";

    let mut rows: Vec<ScanRow> = Vec::new();
    rows.push(scan_config(
        &groups,
        "基线 100/100/100",
        100,
        100,
        100,
        max_frames,
    )?);
    if run_activity {
        for &a in &ACTIVITY_SCAN {
            rows.push(scan_config(
                &groups,
                &format!("activity={a}"),
                a,
                100,
                100,
                max_frames,
            )?);
        }
    }
    if run_flat_edge {
        for &f in &FLAT_SCAN {
            rows.push(scan_config(&groups, &format!("flat={f}"), 100, f, 100, max_frames)?);
        }
        for &e in &EDGE_SCAN {
            rows.push(scan_config(&groups, &format!("edge={e}"), 100, 100, e, max_frames)?);
        }
    }

    let base = rows[0].total_bytes;
    let base_psnr = rows[0].global_psnr;
    let base_worst = rows[0].worst_psnr;
    println!("\n--- 汇总（基线 = {base} B @ {base_psnr:.2} dB, worst {base_worst:.2} dB）---");
    for r in &rows {
        let dbytes = (r.total_bytes as f64 - base as f64) / base as f64 * 100.0;
        let dpsnr = r.global_psnr - base_psnr;
        println!(
            "{:<24} {:>11} B ({:+.2}%)  global={:.2}dB({:+.3})  worst={:.2}dB",
            r.label, r.total_bytes, dbytes, r.global_psnr, dpsnr, r.worst_psnr
        );
    }

    println!("\n=== 判定参考 ===");
    println!("  activity >100：体积下降且全局 PSNR 在 q90 护栏内（不跌近 q85 的 43.31dB）→ 采纳");
    println!("  flat/edge >100：若基线无 banding/ringing（保护对象不存在）→ 体积白增，证伪");
    println!("  加权码率下降 ≥3% 才值得改默认（compression-algorithm-exploration §6）");
    Ok(())
}

/// 对全部图像组跑一次三旋钮配置，聚合体积与质量。
fn scan_config(
    groups: &[PathBuf],
    label: &str,
    activity: u16,
    flat: u16,
    edge: u16,
    max_frames: usize,
) -> Result<ScanRow, String> {
    println!(">>> {label} 开始");
    flush();
    let mut total_bytes = 0u64;
    let mut total_mse = 0.0f64;
    let mut total_pixels = 0u64;
    let mut worst = f64::INFINITY;

    // 组间串行（避免大图组 batch 中间缓冲同时驻留导致内存爆炸）；单组内
    // encode_group 按内存自动选 batch（帧级并行）或 streaming（串行）。
    for dir in groups {
        let paths = collect_paths(dir)?;
        if paths.len() < 2 || paths.len() > max_frames {
            continue;
        }
        let params = make_params(activity, flat, edge)?;
        let encoded = encode_group(&paths, &params)?;
        total_bytes += encoded.len() as u64;

        let (mse, pixels, w) = psnr_streaming(&encoded, &paths)?;
        total_mse += mse;
        total_pixels += pixels;
        worst = worst.min(w);
    }

    Ok(ScanRow {
        label: label.to_string(),
        total_bytes,
        global_psnr: psnr_from_mse(total_mse, total_pixels),
        worst_psnr: worst,
    })
}

/// 构造 q90 + 三旋钮配置（流式路径恒 golden 差分）。
fn make_params(activity: u16, flat: u16, edge: u16) -> Result<EncodeParams, String> {
    let lossy = LossyOptionsV2Builder::preset(QUALITY)
        .activity_masking(activity)
        .flat_area_protection(flat)
        .edge_protection(edge)
        // 显式 Golden 参考：batch 与 streaming 产物一致（decode_bytes_streaming
        // 正确解码），且 batch 残差帧可帧级并行（无 previous 链式依赖）。
        .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Golden)
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

/// 按内存预估选择编码路径：小图 batch（帧级并行、打满 CPU），大图 streaming
/// （串行、内存 O(golden+单帧)）。避免大图组在 batch 接口触发中间缓冲内存爆炸。
fn encode_group(paths: &[PathBuf], params: &EncodeParams) -> Result<Vec<u8>, String> {
    let first = load_one(&paths[0])?;
    let per_frame = first.width as usize * first.height as usize * 3 * 4;
    let estimated = per_frame.saturating_mul(paths.len());
    const BATCH_THRESHOLD: usize = 4_000_000_000; // 4 GB 帧数据阈值：batch 中间缓冲 ~3×帧数据
    if estimated <= BATCH_THRESHOLD {
        let mut frames = Vec::with_capacity(paths.len());
        frames.push(first);
        for p in &paths[1..] {
            frames.push(load_one(p)?);
        }
        crate::crf::encoder::encode_sequence(&frames, params).map_err(|e| e.to_string())
    } else {
        let mut enc = crate::crf::encoder::streaming::StreamingEncoder::new(params)
            .map_err(|e| e.to_string())?;
        for p in paths {
            let frame = load_one(p)?;
            enc.push_frame(&frame).map_err(|e| e.to_string())?;
        }
        enc.finish().map_err(|e| e.to_string())
    }
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
        worst = worst.min(psnr_from_mse(mse, pixels));
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    Ok((total_mse, total_pixels, worst))
}

/// 单帧 MSE（三通道合并）与像素总数。
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

fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// 加载单张图像为 RGB i32 像素（与 probe_monotonicity 一致）。
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

/// 收集目录下按文件名排序的图像路径（支持 PNG/JPEG/WebP/BMP）。
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

/// 枚举 x-y-z 命名的图像组目录（x/y/z 均为数字，至少 2 张图）。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_matrix_within_validate_range() {
        for &a in &ACTIVITY_SCAN {
            assert!(a <= 200 && a > 100);
        }
        for &f in &FLAT_SCAN {
            assert!(f <= 200 && f > 100);
        }
        for &e in &EDGE_SCAN {
            assert!(e <= 200 && e > 100);
        }
    }

    #[test]
    fn make_params_accepts_scan_values() {
        let p = make_params(200, 150, 125).expect("三旋钮在 validate 范围内");
        assert!(p.lossy.is_some());
    }

    #[test]
    fn x_y_z_naming() {
        assert!(is_x_y_z("2-1-2"));
        assert!(is_x_y_z("10-2-14"));
        assert!(!is_x_y_z("1000"));
        assert!(!is_x_y_z("a-b-c"));
        assert!(!is_x_y_z("2-1"));
    }
}
