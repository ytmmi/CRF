//! planar 子平面次级候选胜出频率探针（profile 验证）
//!
//! planar（frame_type=3）的 2669ms 开销中，3 个子平面编码占 99%（每平面
//! 约 800~920ms）。每个子平面是单分量 Gray 平面，却递归跑完整自适应
//! 流水线：SATD + top-2 试编码 + banded(2) + palette(4) + intrabc(7) +
//! cabac(5) + dct(6)。
//!
//! 本探针回答一个剪枝决策问题：在单分量子平面上，这些**次级候选**
//! （banded/palette/intrabc/dct/cabac）实际胜出多少次？若某候选几乎
//! 从不胜出，即可字节透明剪枝（跳过该候选绝不改变最终字节）。
//!
//! 探针对每个 RGB 帧：
//! 1. rct_forward 得到 [Y, Co, Cg]；
//! 2. 拆三平面，CfL 搜索 + 扣除（与真实 planar 管线一致）；
//! 3. 对每个子平面跑 lossless `encode_frame_adaptive`；
//! 4. 读回 frame_type（data[8]），按子平面下标聚合胜出分布。
//!
//! 不接入生产路径，仅由 `--probe-planar-sub <dir>` CLI 分派。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;

/// frame_type → 名称（解码端 frame/dispatcher 一致）
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

/// CfL α 候选集（与 planar.rs 一致）
const CFL_CANDIDATES: [i32; 9] = [-4, -3, -2, -1, 0, 1, 2, 3, 4];

fn search_alpha(y: &[i32], chroma: &[i32]) -> i32 {
    let step = (chroma.len() / 16).max(1);
    let mut best_a = 0i32;
    let mut best_sad = u64::MAX;
    for &a in &CFL_CANDIDATES {
        let mut sad = 0u64;
        let mut i = 0;
        while i < chroma.len() {
            let pred = (a * (y[i] - 128)) >> 4;
            sad += (chroma[i] - pred).unsigned_abs() as u64;
            i += step;
        }
        if sad < best_sad {
            best_sad = sad;
            best_a = a;
        }
    }
    best_a
}

fn apply_cfl(chroma: &[i32], y: &[i32], alpha: i32) -> Vec<i32> {
    if alpha == 0 {
        return chroma.to_vec();
    }
    let mut out = vec![0i32; chroma.len()];
    crate::crf::backend::ops::cfl_luma_subtract(chroma, y, alpha, &mut out);
    out
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.is_empty() {
        return Err(format!("{dir}: no images"));
    }
    println!("=== planar sub-plane candidate probe ===");
    println!("group: {dir}  ({} frames)\n", frames.len());

    // 聚合：sub[子平面下标][frame_type] = 胜出次数
    const NSUB: usize = 3;
    let mut sub_wins: [[usize; 9]; NSUB] = [[0; 9]; NSUB];
    let mut total_sub = 0usize;

    for (fi, frame) in frames.iter().enumerate() {
        if frame.color_format != ColorFormat::Rgb {
            continue;
        }
        // 与真实管线一致：RCT 去相关到 Y/Co/Cg 域。
        let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
        let w = frame.width as usize;
        let h = frame.height as usize;
        let n = w * h;
        let mut planes: [Vec<i32>; 3] = std::array::from_fn(|_| Vec::with_capacity(n));
        for px in ycocg.chunks_exact(3) {
            planes[0].push(px[0]);
            planes[1].push(px[1]);
            planes[2].push(px[2]);
        }
        let alpha_c = search_alpha(&planes[0], &planes[1]);
        let alpha_g = search_alpha(&planes[0], &planes[2]);
        let co_adj = apply_cfl(&planes[1], &planes[0], alpha_c);
        let cg_adj = apply_cfl(&planes[2], &planes[0], alpha_g);

        let plane_refs: [(&[i32], usize, usize); 3] = [
            (&planes[0], w, h),
            (&co_adj, w, h),
            (&cg_adj, w, h),
        ];
        let mut preferred = None;
        for (pi, (plane, pw, ph)) in plane_refs.iter().enumerate() {
            let img = ImageData {
                width: *pw as u16,
                height: *ph as u16,
                bit_depth: frame.bit_depth,
                color_format: ColorFormat::Gray,
                pixels: plane.to_vec(),
            };
            let out = encode_frame_adaptive(
                &img,
                CompressionType::GolombRice,
                8,
                false,
                FrameQuant::lossless(),
                preferred,
                None,
            )
            .map_err(|e| e.to_string())?;
            preferred = out.pred_mode;
            let ft = out.data.get(8).copied().unwrap_or(0);
            if (ft as usize) < 9 {
                sub_wins[pi][ft as usize] += 1;
            }
            total_sub += 1;
        }
        let _ = fi;
    }

    let names = ["Y", "Co", "Cg"];
    let nframes = frames.len();
    println!("sub-plane 胜出 frame_type 分布（共 {total_sub} 子平面 = {nframes} 帧 × 3）:\n");
    for pi in 0..NSUB {
        println!("  {} 平面:", names[pi]);
        for t in 0..9 {
            let c = sub_wins[pi][t];
            if c > 0 {
                println!(
                    "    {:<14} {:>3}  ({:>5.1}%)",
                    frame_type_name(t as u8),
                    c,
                    c as f64 / nframes.max(1) as f64 * 100.0
                );
            }
        }
    }

    // 次级候选（banded/palette/intrabc/cabac/dct/intra_transform）合计胜出
    println!("\n次级候选胜出汇总（banded/palette/intrabc/cabac/dct/intra_transform）:");
    const SECONDARY: [usize; 6] = [2, 4, 5, 6, 7, 8];
    for pi in 0..NSUB {
        let sum: usize = SECONDARY.iter().map(|&t| sub_wins[pi][t]).sum();
        println!("  {} 平面: {sum} / {}", names[pi], frames.len());
    }
    Ok(())
}
