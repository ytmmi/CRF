//! planar 候选剪枝探针（profile 验证）
//!
//! planar（frame_type=3）在 3 分量帧上是自适应仲裁里最贵的候选（单次投入
//! 约为 DCT 的 6 倍）。本探针验证一个可廉价计算的剪枝信号——色度平坦度——
//! 是否为 planar 胜出的**必要条件**：
//!
//! planar 的收益来源是把 RCT 后 [Y, Co, Cg] 拆成三个平面独立编码，使 Co/Cg
//! 平面的大面积恒定区被 RLE 大幅压缩（二次元平涂特化）。若 Co/Cg 高度非平坦
//! （纹理密集、噪声），三平面拆分只增加平面头开销而无 RLE 收益，planar 必败。
//!
//! 因此「色度平坦度低于阈值 ⟹ planar 必败」若成立，即可安全剪枝。
//!
//! 探针对每个 RGB 帧：
//! 1. 先做 rct_forward（与真实管线一致）得到 [Y, Co, Cg]；
//! 2. 统计 Co/Cg 的**长零行程占比**（run≥8 的像素比例）作为平坦度；
//! 3. 跑一次 lossless `encode_frame_adaptive`（喂 RCT 域数据，与管线一致），
//!    读回胜出 frame_type 判断 planar 是否胜出；
//! 4. 输出（平坦度, 是否胜出）对照表。
//!
//! 不接入生产路径，仅由 `--probe-planar <dir>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;

/// 色度平面平坦度：Co/Cg 各通道中「处于 ≥8 等值水平行程内」的像素占比，∈ [0,1]。
/// 1.0 = 每行 Co/Cg 均为 ≥8 长恒定区（planar 最有利）；0.0 = 无任何长行程。
fn chroma_flatness(ycocg: &[i32]) -> f64 {
    let n = ycocg.len() / 3;
    if n <= 1 {
        return 1.0;
    }
    let mut in_long_run = 0usize;
    let mut total = 0usize;
    // 通道 1=Co, 2=Cg；逐通道独立统计水平长行程。
    for ch in [1usize, 2usize] {
        let mut run_start = 0usize;
        for i in 1..n {
            if ycocg[i * 3 + ch] != ycocg[(i - 1) * 3 + ch] {
                let run_len = i - run_start;
                if run_len >= 8 {
                    in_long_run += run_len;
                }
                run_start = i;
            }
        }
        let run_len = n - run_start;
        if run_len >= 8 {
            in_long_run += run_len;
        }
        total += n;
    }
    if total == 0 {
        1.0
    } else {
        in_long_run as f64 / total as f64
    }
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.is_empty() {
        return Err(format!("{dir}: no images"));
    }
    println!("=== planar prune probe ===");
    println!("group: {dir}  ({} frames)\n", frames.len());
    println!("frame  flatness  planar_won  size");
    let mut planar_wins = 0usize;
    let mut total = 0usize;
    for (i, frame) in frames.iter().enumerate() {
        if frame.color_format != ColorFormat::Rgb {
            continue;
        }
        total += 1;
        // 与真实管线一致：RCT 去相关到 Y/Co/Cg 域。
        let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
        let flatness = chroma_flatness(&ycocg);
        let eff = ImageData {
            width: frame.width,
            height: frame.height,
            bit_depth: frame.bit_depth,
            color_format: frame.color_format,
            pixels: ycocg,
        };
        let out = encode_frame_adaptive(
            &eff,
            CompressionType::GolombRice,
            8,
            true,
            FrameQuant::lossless(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        let frame_type = out.data.get(8).copied().unwrap_or(0);
        let planar_won = frame_type == 3;
        if planar_won {
            planar_wins += 1;
        }
        println!(
            "  {:>3}  {:>8.4}  {:>10}  {}x{}",
            i,
            flatness,
            if planar_won { "WIN" } else { "lose" },
            frame.width,
            frame.height,
        );
    }
    println!("\nplanar wins: {planar_wins}/{total}\n");
    Ok(())
}
