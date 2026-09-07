//! 首帧误差分离探针：JPEG2000 式嵌入式分层收益验证（§9 建议 3/10）
//!
//! 建议 3/10（采纳探索，高风险高回报）：首帧有损 + 误差层单独编码，
//! 借鉴 JPEG2000 嵌入式设计——精化层引用有损重建 G_hat（而非原图），
//! 避免两层冗余。§16 的负结果（golden_lossless=false）前提是「误差注入
//! 差分帧」；本探针验证「误差 E 单独编码」的分离方案：
//!
//!   size(有损全序列 q90) + size(E 无损误差层)  vs  size(无损全序列)
//!
//! 若前者 < 后者，误差分离（嵌入/渐进式）有净收益，值得进一步实现；
//! 若 ≥，则两层冗余抵消收益，维持现状。零外部数据集，仅扫 test/png。

use crate::crf::core::color::rct;
use crate::crf::core::config::lossy_v2::LossyOptionsV2Builder;
use crate::crf::core::domain::{ColorFormat, CompressionType, EncodeParams, ImageData};
use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder::encode_sequence;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;

/// 运行探针。`root` 为 test/png 根目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== 首帧误差分离探针（JPEG2000 嵌入式）===");
    println!("root: {root}\n");

    let mut total_lossless = 0u64;
    let mut total_lossy_plus_e = 0u64;
    let mut groups_seen = 0usize;
    let mut groups_win = 0usize;

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        // 控制每组帧数(取前 3 帧)以控制耗时与内存——误差分离收益看首帧
        // 与差分帧的相对结构,前 3 帧足够采样(大图组全帧超 BATCH 限制)。
        let frames: Vec<ImageData> = frames.into_iter().take(3).collect();
        groups_seen += 1;

        // 无损全序列（路径 G，与 bench 一致）
        let params_lossless = EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crate::crf::core::domain::PredictionMode::Med,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: true,
            user_metadata: None,
        };
        let bytes_lossless = match encode_sequence(&frames, &params_lossless) {
            Ok(b) => b.len() as u64,
            Err(e) => {
                println!("{name:<14} 跳过(无损编码失败: {e})");
                continue;
            }
        };

        // 有损 q90 全序列
        let lossy = match LossyOptionsV2Builder::preset(9000).build() {
            Ok(l) => l,
            Err(e) => {
                println!("{name:<14} 跳过(有损配置失败: {e})");
                continue;
            }
        };
        let params_lossy = EncodeParams {
            lossy: Some(lossy),
            ..params_lossless.clone()
        };
        let encoded_lossy = match encode_sequence(&frames, &params_lossy) {
            Ok(b) => b,
            Err(e) => {
                println!("{name:<14} 跳过(有损编码失败: {e})");
                continue;
            }
        };
        let bytes_lossy = encoded_lossy.len() as u64;

        // 解码得 G_hat（有损重建首帧,RGB 域）
        let decoded = match decode_from_bytes(&encoded_lossy) {
            Ok(d) => d,
            Err(e) => {
                println!("{name:<14} 跳过(解码失败: {e})");
                continue;
            }
        };
        let g_hat = decoded.frames.first().ok_or("no decoded frame")?;
        if g_hat.pixels.len() != frames[0].pixels.len() {
            continue;
        }

        // 误差层 E = frames[0] − G_hat（RGB 域像素差）
        let e_pixels: Vec<i32> = frames[0]
            .pixels
            .iter()
            .zip(g_hat.pixels.iter())
            .map(|(a, b)| a.wrapping_sub(*b))
            .collect();

        // E 无损编码：RCT + encode_frame_adaptive（单帧，同首帧编码语义）
        let mut e_rgb = e_pixels.clone();
        crate::crf::core::color::rct::rct_forward_in_place(&mut e_rgb, components)
            .map_err(|e| e.to_string())?;
        let e_img = ImageData {
            width: frames[0].width,
            height: frames[0].height,
            bit_depth: frames[0].bit_depth,
            color_format: ColorFormat::Rgb,
            pixels: e_rgb,
        };
        let e_out = encode_frame_adaptive(
            &e_img,
            CompressionType::GolombRice,
            8,
            true,
            FrameQuant::lossless(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        let bytes_e = e_out.data.len() as u64;

        let lossy_plus_e = bytes_lossy + bytes_e;
        let win = lossy_plus_e < bytes_lossless;
        if win {
            groups_win += 1;
        }
        total_lossless += bytes_lossless;
        total_lossy_plus_e += lossy_plus_e;

        println!(
            "{name:<14} 无损 {bytes_lossless:>10}  有损+E {lossy_plus_e:>10}  (有损 {bytes_lossy:>9} + E {bytes_e:>7})  差 {:>+8.1}%  {}",
            (lossy_plus_e as f64 - bytes_lossless as f64) / bytes_lossless as f64 * 100.0,
            if win { "✓胜" } else { "✗" }
        );
    }

    println!("\n--- 汇总 ---");
    if groups_seen == 0 {
        println!("(无可用 RGB 组)");
        return Ok(());
    }
    println!("组数: {groups_seen}  误差分离更小组数: {groups_win}");
    let total_pct = (total_lossy_plus_e as f64 - total_lossless as f64) / total_lossless as f64 * 100.0;
    println!(
        "总无损 {total_lossless}  vs  总有损+E {total_lossy_plus_e}  总体差 {total_pct:+.1}%"
    );
    if total_pct < 0.0 {
        println!("判定: 误差分离总字节更小 → JPEG2000 式分层有价值，可深入实现");
    } else {
        println!("判定: 误差分离总字节更大 → 两层冗余抵消收益，维持现状（与 §16 方向一致但机制不同）");
    }
    Ok(())
}