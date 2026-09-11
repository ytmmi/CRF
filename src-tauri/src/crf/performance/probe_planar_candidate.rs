//! planar 子平面候选级 Fast-Fail 空间探针（optimization-review §56 新候选）
//!
//! 对 planar 的 Y/Co/Cg 子平面单独跑 `encode_frame_adaptive`（telemetry），报告各候选
//! 阶段耗时与胜出 frame_type，判断是否还有「高耗时 · 0 胜出」可剪项。
//!
//! 背景：§P1b 已对单分量子平面剪掉 intrabc（components==1 门控）与 lossless dct
//! （`skip_dct_subplane`）；§44 决定 palette 保留（低色数能力，156 子平面 0 胜出但
//! 不剪）。本探针量化剩余候选级 Fast-Fail 空间。
//!
//! 不接入生产路径，由 `--probe-planar-candidate <dir>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use crate::crf::performance::telemetry;

/// 运行探针。`dir` 为图像组目录（取首帧）。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    let frame = frames.first().ok_or_else(|| format!("{dir}: no images"))?;
    if frame.color_format.component_count() != 3 {
        return Err(format!("{dir}: 首帧非 RGB"));
    }
    let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut planes: [Vec<i32>; 3] = std::array::from_fn(|_| vec![0i32; w * h]);
    for (i, px) in ycocg.chunks_exact(3).enumerate() {
        planes[0][i] = px[0];
        planes[1][i] = px[1];
        planes[2][i] = px[2];
    }

    println!("=== planar 子平面候选级 Fast-Fail 空间探针（§56）===");
    println!("group: {dir}  首帧 {}x{}\n", frame.width, frame.height);
    let names = ["Y", "Co", "Cg"];
    telemetry::enable();
    for (pi, plane) in planes.into_iter().enumerate() {
        let img = ImageData {
            width: frame.width,
            height: frame.height,
            bit_depth: frame.bit_depth,
            color_format: ColorFormat::Gray,
            pixels: plane,
        };
        // 预热（填充分配器）
        let _ = encode_frame_adaptive(
            &img,
            CompressionType::GolombRice,
            8,
            false,
            FrameQuant::lossless(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        telemetry::clear();
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
        let ft = out.data.get(8).copied().unwrap_or(0);
        println!(
            "--- 子平面 {}  胜出 frame_type={ft}  帧字节={} ---",
            names[pi],
            out.data.len()
        );
        let report = telemetry::report();
        if report.is_empty() {
            println!("(无采样)");
        } else {
            println!("{report}");
        }
    }
    println!("说明：候选耗时用于判断「高耗时 · 0 胜出」可剪项。§P1b 已剪 intrabc/dct，");
    println!("§44 保留 palette；若剩余候选均「低耗时或真实胜出」，则候选级 Fast-Fail 空间已尽。");
    Ok(())
}
