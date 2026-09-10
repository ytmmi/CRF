//! 扩展验证集质量趋势探针（first-frame-optimization-plan §6.1）
//!
//! 对 `test/png-valid` 下各分层（illustration/lineart/manga/pixel_sprite/wallpaper）
//! 的每张图，构造 `[图, 图]` 序列（CRF 需 ≥2 帧），编码后测**首帧**的 PSNR/SSIM，
//! 按分层汇总质量趋势，确认质量档位映射不是 PNG1000 特化。
//!
//! 由 `--probe-valid-set <root>` 分派。零外部数据集，仅扫指定目录。

use std::path::{Path, PathBuf};

use crate::crf::core::domain::{EncodeParams, PredictionMode};
use crate::crf::core::metrics::ssim::ssim;
use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder::encode_sequence;
use crate::crf::LossyOptionsV2Builder;

use super::probe_monotonicity::{frame_mse, load_one, psnr_from_mse};

/// 质量档位阶梯
const LADDER: [u16; 5] = [75, 80, 85, 90, 95];

/// 批量闭环有损参数（`input_original_frames: false`）
///
/// `first_frame_offset(0)`：首帧改用 QualityOffset 模式（= 序列质量），
/// 否则 golden 首帧默认强制无损，单图无法测有损质量。
fn make_params(q: u16) -> Result<EncodeParams, String> {
    let lossy = LossyOptionsV2Builder::preset(q * 100)
        .first_frame_offset(0)
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

/// 运行探针。`root` 为分层数据集根目录。
pub fn run(root: &str) -> Result<(), String> {
    let root_path = Path::new(root);
    let mut layers: Vec<PathBuf> = std::fs::read_dir(root_path)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    layers.sort();

    println!("=== 扩展验证集质量趋势探针 ===");
    println!("root: {root}  ({} 个分层)\n", layers.len());

    for layer in &layers {
        let name = layer
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut imgs: Vec<PathBuf> = std::fs::read_dir(layer)
            .map_err(|e| format!("{}: {e}", layer.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        imgs.sort();

        println!("--- {name} ({} 张) ---", imgs.len());
        for img_path in &imgs {
            let img = match load_one(img_path) {
                Ok(i) => i,
                Err(e) => {
                    println!("  {}: load err {e}", img_path.display());
                    continue;
                }
            };
            // CRF 需 ≥2 帧：构造 [图, 图]，关注首帧质量
            let frames = vec![img.clone(), img.clone()];
            let fname = img_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            print!("  {fname} ({}x{}):", img.width, img.height);
            for &q in &LADDER {
                let params = make_params(q)?;
                let enc = match encode_sequence(&frames, &params) {
                    Ok(e) => e,
                    Err(e) => {
                        print!(" q{q}:ERR({e})");
                        continue;
                    }
                };
                let dec = match decode_from_bytes(&enc) {
                    Ok(d) => d,
                    Err(e) => {
                        print!(" q{q}:DECERR({e})");
                        continue;
                    }
                };
                let (mse, px) = frame_mse(&frames[0], &dec.frames[0]);
                let psnr = psnr_from_mse(mse, px);
                let s = ssim(&frames[0], &dec.frames[0]);
                print!(" q{q}:{psnr:.2}dB/{s:.4}");
            }
            println!();
        }
        println!();
    }
    Ok(())
}
