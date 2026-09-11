//! 差分帧候选 profile 探针：候选胜出率 + 阶段耗时分离
//!
//! 现有 `--probe-first-frame` 只覆盖首帧。而 bench telemetry 显示
//! `encode.rest_frames`（差分帧）是 encode 最大块（1000 组约 4.3s/轮 vs
//! 首帧 2.7s）。本探针在真实差分帧上（路径 G 语义：RGB 域 `frame−golden`
//! 后 RCT）逐候选统计：
//! 1. 胜出 frame_type 分布（剪切决策：从不胜出的高成本候选可字节透明剪枝）；
//! 2. 各候选独占耗时（确定下一性能轮目标）。
//!
//! ⚠ **采样完整性限制（D4 教训）**：本探针每组仅取前 2 差分帧（`take(2)`）。
//! 「某候选 0 胜出」只在该采样内成立，**不能**外推为全组/全数据集的
//! 「从不胜出」——dct 在 2-12-4 后续帧真实胜出，按探针剪枝致字节 +2.2%
//! 已回退（见 optimization-review §42）。任何基于本探针的剪枝必须：
//! ① 覆盖组内全部差分帧；② 剪枝后与 HEAD 同工具链逐字节对拍全部组。
//!
//! 零外部数据集，仅扫 test/png。由 `--probe-rest-frames <root>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use crate::crf::performance::telemetry;

/// frame_type → 名称
fn frame_type_name(t: u8) -> &'static str {
    match t {
        0 => "golomb-block",
        1 => "rle",
        2 => "banded",
        3 => "planar",
        4 => "palette",
        5 => "cabac",
        6 => "dct",
        7 => "intrabc",
        8 => "intra_transform",
        _ => "unknown",
    }
}

/// 运行探针。`root` 为 test/png 根目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== 差分帧候选 profile 探针 ===");
    println!("root: {root}\n");

    const MAX_TYPES: usize = 9;
    let mut wins = [0usize; MAX_TYPES];
    let mut total_frames = 0usize;

    telemetry::enable();
    telemetry::clear();

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let golden = &frames[0].pixels;
        let components = frames[0].color_format.component_count();
        let width = frames[0].width as usize;
        let height = frames[0].height as usize;

        // 默认每组前 2 差分帧（快速）；CRF_PROBE_ALL_FRAMES=1 覆盖全部差分帧。
        // D4 教训：剪枝决策必须基于全帧采样，`take(2)` 结果不可外推。
        let all_frames = std::env::var("CRF_PROBE_ALL_FRAMES")
            .map(|v| v == "1")
            .unwrap_or(false);
        let diff_take = if all_frames { usize::MAX } else { 2 };
        for frame in frames.iter().skip(1).take(diff_take) {
            if frame.pixels.len() != golden.len() || components != 3 {
                continue;
            }
            // 路径 G 差分帧语义：RGB 域 diff(frame − golden) 后 RCT。
            let mut diff_rgb = vec![0i32; golden.len()];
            crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
            let diff = rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;
            let eff = ImageData {
                width: width as u16,
                height: height as u16,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels: diff,
            };
            let out = encode_frame_adaptive(
                &eff,
                CompressionType::GolombRice,
                8,
                false,
                FrameQuant::lossless(),
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
            let ft = out.data.get(8).copied().unwrap_or(0) as usize;
            if ft < MAX_TYPES {
                wins[ft] += 1;
            }
            total_frames += 1;
        }
    }

    println!("--- 差分帧胜出 frame_type 分布（共 {total_frames} 帧）---");
    for (t, &w) in wins.iter().enumerate().take(MAX_TYPES) {
        if w > 0 {
            println!(
                "  {:<15} {:>3}  ({:>5.1}%)",
                frame_type_name(t as u8),
                w,
                w as f64 / total_frames.max(1) as f64 * 100.0
            );
        }
    }

    let report = telemetry::report();
    println!("\n--- 差分帧候选阶段耗时 ---");
    if report.is_empty() {
        println!("(无采样)");
    } else {
        println!("{report}");
    }
    Ok(())
}
