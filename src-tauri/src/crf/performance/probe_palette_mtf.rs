//! 调色板排序 + MTF 编码收益探针（optimization-review §56 新候选）
//!
//! 对比三种 palette 字节口径：
//! - A：现有生产语义（首现顺序调色板 + copy-above token + RLE+Golomb）；
//! - B：按值升序排序调色板 + Move-To-Front 索引流 + RLE+Golomb；
//! - A2：现有最优（`encode_frame_adaptive` 全候选竞争，含 palette 自身）。
//!
//! 原理：低色数渐变内容中相邻像素颜色相近，排序后索引相邻 → MTF rank 小值 →
//! RLE+Golomb 更省。仅统计分量级色数 ≤256 的低色数帧（palette 可用场景）。
//!
//! 口径：A/B 只比「palette 值流 + 索引流」字节（不含帧头）；A2 为完整帧字节
//! （含帧头），故 B vs A2 是「MTF palette 能否击败现有最优」的端到端判据。
//!
//! 不接入生产路径，由 `--probe-palette-mtf <root>` CLI 分派。

use std::collections::HashMap;

use crate::crf::core::domain::CompressionType;
use crate::crf::encoder::exp_golomb::ExpGolombEncoder;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_golomb::RleGolombEncoder;
use crate::crf::performance::bench::load_frames;

const PALETTE_MAX: usize = 256;
/// 超过此像素数跳过 A2（现有最优）计算——超大图自适应编码过慢。
const A2_PIXEL_LIMIT: usize = 8_000_000;

/// 首现顺序构建调色板与索引流；色数超限返回 None。
fn build_palette(pixels: &[i32]) -> Option<(Vec<i32>, Vec<i32>)> {
    let mut map: HashMap<i32, u16> = HashMap::with_capacity(512);
    let mut order = Vec::new();
    let mut indices = Vec::with_capacity(pixels.len());
    for &v in pixels {
        let next = map.len() as u16;
        match map.get(&v) {
            Some(&ix) => indices.push(ix as i32),
            None => {
                if next as usize >= PALETTE_MAX {
                    return None;
                }
                map.insert(v, next);
                order.push(v);
                indices.push(next as i32);
            }
        }
    }
    Some((order, indices))
}

/// palette 值流字节（exp-Golomb zigzag）。
fn pal_bytes(order: &[i32]) -> usize {
    let mut pe = ExpGolombEncoder::new();
    for &v in order {
        pe.encode_signed(v);
    }
    pe.finish().len()
}

/// A：首现顺序 + copy-above token + RLE+Golomb。
fn encode_a(indices: &[i32], width: usize) -> usize {
    let use_copy = width > 0 && indices.len() > width;
    let payload: Vec<i32> = if use_copy {
        indices
            .iter()
            .enumerate()
            .map(|(i, &ix)| {
                if i >= width && indices[i - width] == ix {
                    0
                } else {
                    ix + 1
                }
            })
            .collect()
    } else {
        indices.to_vec()
    };
    let mut e = RleGolombEncoder::adaptive(&payload);
    e.encode_signed_array(&payload);
    e.finish().len()
}

/// B：值升序排序调色板 + MTF 索引流 + RLE+Golomb。
fn encode_b(order: &[i32], indices: &[i32]) -> usize {
    let mut sorted: Vec<i32> = order.to_vec();
    sorted.sort_unstable();
    let mut rank_of: HashMap<i32, i32> = HashMap::with_capacity(sorted.len());
    for (i, &v) in sorted.iter().enumerate() {
        rank_of.insert(v, i as i32);
    }
    // MTF 列表 + 命中位置移动（低色数场景 ≤256 元素，成本可接受）
    let mut mtf: Vec<i32> = (0..sorted.len() as i32).collect();
    let mut ranks: Vec<i32> = Vec::with_capacity(indices.len());
    for &ix in indices {
        let target = rank_of[&order[ix as usize]];
        let pos = mtf.iter().position(|&x| x == target).unwrap_or(0);
        ranks.push(pos as i32);
        mtf.remove(pos);
        mtf.insert(0, target);
    }
    let mut e = RleGolombEncoder::adaptive(&ranks);
    e.encode_signed_array(&ranks);
    e.finish().len()
}

/// 运行探针。`root` 为含图像子目录的根（默认 test/png-valid）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== palette 排序+MTF 探针（§56 新候选）===");
    println!("root: {root}\n");
    println!(
        "{:<20} {:>6} {:>9} {:>10} {:>10} {:>10} {:>9}",
        "图", "色数", "像素", "A现有", "B排序MTF", "A2最优", "B/A2"
    );
    let mut usable = 0usize;
    let mut total = 0usize;
    let mut sum_a = 0usize;
    let mut sum_b = 0usize;
    let mut sum_a2 = 0usize;
    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for (fi, frame) in frames.iter().enumerate() {
            total += 1;
            let Some((order, indices)) = build_palette(&frame.pixels) else {
                continue;
            };
            usable += 1;
            let mut sorted = order.clone();
            sorted.sort_unstable();
            let a = pal_bytes(&order) + encode_a(&indices, frame.width as usize);
            let b = pal_bytes(&sorted) + encode_b(&order, &indices);
            sum_a += a;
            sum_b += b;
            // A2：现有最优（全候选竞争）；超大图跳过（自适应编码过慢）
            let (a2, b_vs_a2) = if indices.len() <= A2_PIXEL_LIMIT {
                let n = encode_frame_adaptive(
                    frame,
                    CompressionType::GolombRice,
                    8,
                    true,
                    FrameQuant::lossless(),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?
                .data
                .len();
                sum_a2 += n;
                (n, format!("{:+.1}%", (b as f64 - n as f64) / n as f64 * 100.0))
            } else {
                (0usize, "skip".to_string())
            };
            println!(
                "{:<20} {:>6} {:>9} {:>10} {:>10} {:>10} {:>9}",
                format!("{name}/{fi}"),
                order.len(),
                indices.len(),
                a,
                b,
                a2,
                b_vs_a2
            );
        }
    }
    println!(
        "\n可用帧(分量级≤{PALETTE_MAX}): {usable}/{total}  合计 A={sum_a} B={sum_b}  B/A={:+.1}%  A2={sum_a2} B/A2={:+.1}%",
        (sum_b as f64 - sum_a as f64) / sum_a.max(1) as f64 * 100.0,
        (sum_b as f64 - sum_a2 as f64) / sum_a2.max(1) as f64 * 100.0
    );
    println!("判定：B/A2 净收益 >3%（即 MTF palette 击败现有最优）才值得实施（§56 门槛）。");
    Ok(())
}
