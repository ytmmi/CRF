//! 首帧内部候选阶段 profile 探针
//!
//! 首帧（golden 原图，非差分）是 encode 的串行主导（45%），但现有
//! `encode.adaptive.*` telemetry span 混合了首帧与差分帧、以及 planar 子平面
//! 递归调用，无法分离「首帧内部各候选阶段的独占耗时」。本探针对单个首帧：
//!
//! 1. rct_forward（与路径 G 首帧一致）；
//! 2. 开启 telemetry；
//! 3. 单次 `encode_frame_adaptive`；
//! 4. 输出 `encode.adaptive.*` 阶段独占耗时。
//!
//! 差分帧与首帧的关键差异：差分帧残差稀疏（SATD 排序后 top-2 试编码即收敛，
//! planar/banded/intrabc 等候选被 Fast-Fail 或字节竞争淘汰），首帧原图纹理
//! 密集、各候选都完整投入。故首帧是「候选流水线串行」的最坏情形。
//!
//! 不接入生产路径，仅由 `--probe-first-frame <dir>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::adaptive::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use crate::crf::performance::telemetry;

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.is_empty() {
        return Err(format!("{dir}: no images"));
    }
    let frame = &frames[0];
    if frame.color_format != crate::crf::core::domain::ColorFormat::Rgb {
        return Err(format!("{dir}: 首帧非 RGB"));
    }
    println!("=== first-frame candidate probe ===");
    println!(
        "group: {dir}  首帧 {}x{}\n",
        frame.width, frame.height
    );

    // 与路径 G 首帧一致：RCT 去相关到 Y/Co/Cg 域。
    let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
    let eff = ImageData {
        width: frame.width,
        height: frame.height,
        bit_depth: frame.bit_depth,
        color_format: frame.color_format,
        pixels: ycocg,
    };

    // 预热一次（填充分配器/线程池），再清空采样取单次干净测量。
    telemetry::enable();
    let _ = encode_frame_adaptive(
        &eff,
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
        &eff,
        CompressionType::GolombRice,
        8,
        false,
        FrameQuant::lossless(),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "首帧胜出 frame_type={}  帧字节={}\n",
        out.data.get(8).copied().unwrap_or(0),
        out.data.len()
    );

    let report = telemetry::report();
    println!("--- 首帧内部阶段耗时（单次，未嵌套差分帧）---");
    if report.is_empty() {
        println!("(无采样)");
    } else {
        println!("{report}");
    }
    Ok(())
}
