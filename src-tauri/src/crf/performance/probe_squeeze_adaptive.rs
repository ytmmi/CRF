//! Squeeze + CRF adaptive 组合探针：验证小波子带接强编码器的净价值
//!
//! **背景**：§66 探针显示 JXL Squeeze 类 Haar 小波在 CRF 差分帧残差上显著降低
//! 熵（RLE+Golomb 口径 −12%~−54.7%），但 squeeze + RLE 仍比 CRF 现有
//! `encode_frame_adaptive` 差 17~24%。本探针回答关键问题：**squeeze 子带接 CRF
//! 强编码器能否产生净收益**。
//!
//! **探针口径**（每差分帧，RCT 残差 = `rct_forward(frame − golden)`）：
//! - **A 交织**：`encode_frame_adaptive(交织残差)`（当前生产路径基线）；
//! - **B 分离分量**：3 分量各自 `encode_frame_adaptive` 累加（隔离 squeeze 贡献）；
//! - **C squeeze**：3 分量各自多级 squeeze → 各子带 `encode_frame_adaptive` 累加。
//!
//! **判定**：若 C < A（≥3%），则 squeeze 有净实施价值；若 C ≥ A，则 CRF 现有
//! 自适应编码已优于子带分解，squeeze 不实施。零码流改动、零外部数据集。
//!
//! CLI：`--probe-squeeze-adaptive [root]`（默认 `E:\CRF\test\png`）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use crate::crf::performance::probe_squeeze::{split_components, squeeze_pyramid};
use rayon::prelude::*;

fn squeeze_levels() -> usize {
    std::env::var("CRF_PROBE_SQUEEZE_ADAPTIVE_LEVELS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(2)
}

fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_SQUEEZE_ADAPTIVE_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// CRF `encode_frame_adaptive` 编码单张图像，返回字节数。
fn adaptive_bytes(
    pixels: &[i32],
    width: u16,
    height: u16,
    bit_depth: u8,
    color_format: ColorFormat,
) -> Result<usize, String> {
    let img = ImageData {
        width,
        height,
        bit_depth,
        color_format,
        pixels: pixels.to_vec(),
    };
    let out = encode_frame_adaptive(
        &img,
        CompressionType::GolombRice,
        8,
        false,
        FrameQuant::lossless(),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    Ok(out.data.len())
}

/// 单帧探针结果。
struct FrameStat {
    interleaved: usize,
    per_plane: usize,
    squeeze: usize,
}

/// 运行探针。`root` 为 test/png 根目录或单个组目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();
    if load_frames(root).map(|f| f.len() >= 2).unwrap_or(false) {
        groups = vec![std::path::PathBuf::from(root)];
    }

    let levels = squeeze_levels();
    let limit = group_frame_limit();
    println!("=== Squeeze + CRF adaptive 组合探针 ===");
    println!("root: {root}");
    println!(
        "squeeze 级数: {levels}  |  每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: A=交织 adaptive | B=分离分量 adaptive | C=squeeze 子带 adaptive\n");

    let mut all: Vec<FrameStat> = Vec::new();

    for dir in &groups {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        if limit > 0 && frames.len() > limit {
            frames.truncate(limit);
        }
        let n = frames.len();
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let golden = &frames[0].pixels;
        let width = frames[0].width;
        let height = frames[0].height;
        let bit_depth = frames[0].bit_depth;
        let color_format = frames[0].color_format;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        let stats: Vec<FrameStat> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<FrameStat, String> {
                let frame = &frames[i];
                let mut diff_rgb = vec![0i32; golden.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let residuals =
                    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;

                // A：交织残差 adaptive（当前生产路径）
                let interleaved =
                    adaptive_bytes(&residuals, width, height, bit_depth, color_format)?;

                // B：分离 3 分量各自 adaptive
                let planes = split_components(&residuals, components);
                let mut per_plane = 0usize;
                for plane in &planes {
                    per_plane +=
                        adaptive_bytes(plane, width, height, bit_depth, ColorFormat::Gray)?;
                }

                // C：squeeze 子带各自 adaptive
                let mut squeeze = 0usize;
                for plane in &planes {
                    for (band, bw, bh) in
                        squeeze_pyramid(plane, width as usize, height as usize, levels)
                    {
                        if band.is_empty() || bw == 0 || bh == 0 {
                            continue;
                        }
                        squeeze += adaptive_bytes(
                            &band,
                            bw as u16,
                            bh as u16,
                            bit_depth,
                            ColorFormat::Gray,
                        )?;
                    }
                }

                Ok(FrameStat {
                    interleaved,
                    per_plane,
                    squeeze,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let s = |f: fn(&FrameStat) -> usize| stats.iter().map(f).sum::<usize>();
        let (a, b, c) = (s(|x| x.interleaved), s(|x| x.per_plane), s(|x| x.squeeze));
        let pct = |x: usize, y: usize| -> f64 {
            if y == 0 {
                0.0
            } else {
                (x as f64 - y as f64) / y as f64 * 100.0
            }
        };
        println!(
            "组 {:<12} ({} 帧): A 交织 {} | B 分离 {} ({:+.1}% vs A) | C squeeze {} ({:+.1}% vs A, {:+.1}% vs B)",
            name,
            n - 1,
            a,
            b,
            pct(b, a),
            c,
            pct(c, a),
            pct(c, b),
        );
        all.extend(stats);
    }

    println!("\n=== 汇总 ===");
    if all.is_empty() {
        println!("(无有效组：需 ≥2 帧且 RGB)");
        return Ok(());
    }
    let sum = |f: fn(&FrameStat) -> usize| all.iter().map(f).sum::<usize>();
    let (a, b, c) = (
        sum(|x| x.interleaved),
        sum(|x| x.per_plane),
        sum(|x| x.squeeze),
    );
    let pct = |x: usize, y: usize| -> f64 {
        if y == 0 {
            0.0
        } else {
            (x as f64 - y as f64) / y as f64 * 100.0
        }
    };
    println!("帧数 {}", all.len());
    println!("A 交织 adaptive:   {a}");
    println!("B 分离 adaptive:   {b}  ({:+.1}% vs A)", pct(b, a));
    println!(
        "C squeeze adaptive: {c}  ({:+.1}% vs A, {:+.1}% vs B)",
        pct(c, a),
        pct(c, b)
    );

    let gain = pct(c, a);
    println!(
        "\n判定：C 相对 A（当前生产路径）{:+.2}% —— {}",
        gain,
        if gain <= -3.0 {
            "squeeze 有净实施价值（≥3%）"
        } else {
            "squeeze 无净收益（CRF 现有自适应已优）"
        }
    );
    Ok(())
}
