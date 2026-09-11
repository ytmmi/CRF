//! delta palette 收益探针（compression-algorithm-exploration §8.4 最高优先级探索项前置验证）
//!
//! 在不改动任何码流的前提下，估算 JPEG-XL 式 **delta palette** 在真实数据上的
//! 字节收益，并与当前最优候选（`encode_frame_adaptive`）逐帧对比：
//!
//! 1. **像素级(三元组)调色板收集**——现有 `frame_type=4` 按「分量级唯一值 ≤256」
//!    判定（尺度错配：真实差分帧分量级唯一 320~506 全部 >256，但像素级可能低）；
//! 2. **调色板条目 delta 编码**——按亮度排序后，逐条目在
//!    {zero, left, avg(left,left2)} 三个 predictor 中选 |delta| 最小者，
//!    delta 走 exp-Golomb(zigzag)（复用现有编码器，无新熵编码器）；
//! 3. **索引流竞争**——Zero predictor（索引直编）vs copy-above（上方同列相同→0）
//!    取字节小者，均走 RLE+Golomb 自适应 k。
//!
//! 采样覆盖**首帧 + 全部差分帧**（D4 教训：`take(2)` 结果不可外推为「从不胜出」）。
//! 差分帧采用路径 G 语义：RGB 域 `frame − golden` 后 RCT 正变换。
//!
//! 判定门槛：目标组（低色数差分图/立绘）净收益 ≥3% 才进入格式设计；否则记录否决。
//! 零外部数据集，仅扫 `test/png`。由 `--probe-delta-palette <root>` CLI 分派。

use std::collections::{HashMap, HashSet};

use crate::crf::backend::ops::sub_i32;
use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::exp_golomb::ExpGolombEncoder;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_golomb::RleGolombEncoder;
use crate::crf::performance::bench::load_frames;

/// delta palette 载荷头字节数：[count u32][flags u8][k u8][pal_len u32]
const DP_HEADER_LEN: usize = 10;

/// 探针色数上限：超过即视为索引位深与表体积爆炸、不可能胜出，放弃估算。
const MAX_COLORS: usize = 65536;

/// 收益门槛（%）：目标组净收益需 ≥ 此值才进入格式设计。
const GAIN_THRESHOLD_PCT: f64 = 3.0;

/// 统计像素级(三元组)唯一色数；超过 `MAX_COLORS` 提前返回（值略大于上限，够判定）。
fn count_colors(pixels: &[i32]) -> usize {
    let mut set: HashSet<[i32; 3]> = HashSet::new();
    for chunk in pixels.chunks_exact(3) {
        set.insert([chunk[0], chunk[1], chunk[2]]);
        if set.len() > MAX_COLORS {
            return set.len();
        }
    }
    set.len()
}

/// 收集像素级(三元组)调色板与索引流（首现顺序）。调用前须保证色数 ≤ `MAX_COLORS`。
fn collect_palette(pixels: &[i32]) -> (Vec<[i32; 3]>, Vec<i32>) {
    let mut map: HashMap<[i32; 3], u32> = HashMap::new();
    let mut entries: Vec<[i32; 3]> = Vec::new();
    let mut indices: Vec<i32> = Vec::with_capacity(pixels.len() / 3);
    for chunk in pixels.chunks_exact(3) {
        let key = [chunk[0], chunk[1], chunk[2]];
        match map.get(&key) {
            Some(&i) => indices.push(i as i32),
            None => {
                let i = entries.len() as u32;
                map.insert(key, i);
                entries.push(key);
                indices.push(i as i32);
            }
        }
    }
    (entries, indices)
}

/// 调色板条目 delta 编码字节数。输入 `sorted` 必须已按亮度排序（编码端与解码端一致）。
fn estimate_palette_bytes(sorted: &[[i32; 3]]) -> usize {
    let mut enc = ExpGolombEncoder::new();
    for sp in 0..sorted.len() {
        let cur = sorted[sp];
        let left = if sp >= 1 { Some(sorted[sp - 1]) } else { None };
        let left2 = if sp >= 2 { Some(sorted[sp - 2]) } else { None };
        // predictor 候选：0=zero, 1=left, 2=avg(left,left2)
        let preds: [[i32; 3]; 3] = [
            [0, 0, 0],
            left.unwrap_or([0, 0, 0]),
            match (left, left2) {
                (Some(a), Some(b)) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2],
                (Some(a), None) => a,
                _ => [0, 0, 0],
            },
        ];
        let mut best_p = 0usize;
        let mut best_cost = i64::MAX;
        for (pi, p) in preds.iter().enumerate() {
            let cost: i64 = (0..3).map(|c| (cur[c] - p[c]).abs() as i64).sum();
            if cost < best_cost {
                best_cost = cost;
                best_p = pi;
            }
        }
        enc.encode_value(best_p as u32);
        let p = &preds[best_p];
        for c in 0..3 {
            enc.encode_signed(cur[c] - p[c]);
        }
    }
    enc.finish().len()
}

/// 索引流字节数：多种预测器竞争取小；返回 (字节, k)。
///
/// RLE+Golomb 的行程压缩**只对值 0 生效**，因此必须让"不变区域"经预测后归零：
/// - Zero：索引直编（主色恰为索引 0 时最优）；
/// - copy-above：上方同列相同 → 0（AV1 palette 思路）；
/// - 上差分（垂直预测）：差分后整列不变区域归零；
/// - 左差分（水平预测）：差分后整行不变区域归零。
///
/// 差分帧残差大片为 (0,0,0)，其索引在不变区域恒定——上/左差分后产生长零行程，
/// 由 RLE 高效压缩。取四者最小者作为该帧索引流体积下界估计。
fn estimate_index_bytes(indices: &[i32], width: usize) -> (usize, u8) {
    let n = indices.len();
    let encode = |vals: &[i32]| -> (usize, u8) {
        let mut e = RleGolombEncoder::adaptive(vals);
        let k = e.k;
        e.encode_signed_array(vals);
        (e.finish().len(), k)
    };

    // Zero predictor：索引直编
    let mut best = encode(indices);

    // copy-above：与上方同列索引相同 → token 0，否则 ix+1
    let ca: Vec<i32> = (0..n)
        .map(|i| {
            if i >= width && indices[i - width] == indices[i] {
                0
            } else {
                indices[i] + 1
            }
        })
        .collect();
    let r = encode(&ca);
    if r.0 < best.0 {
        best = r;
    }

    // 上差分（垂直预测）：不变列归零
    let up: Vec<i32> = (0..n)
        .map(|i| {
            if i >= width {
                indices[i] - indices[i - width]
            } else {
                indices[i]
            }
        })
        .collect();
    let r = encode(&up);
    if r.0 < best.0 {
        best = r;
    }

    // 左差分（水平预测）：不变行归零
    let left: Vec<i32> = (0..n)
        .map(|i| {
            if i % width == 0 {
                indices[i]
            } else {
                indices[i] - indices[i - 1]
            }
        })
        .collect();
    let r = encode(&left);
    if r.0 < best.0 {
        best = r;
    }

    best
}

/// 单帧 delta palette 估算帧大小（含帧头）。仅 components==3、色数 ≤ MAX_COLORS 时可行。
fn estimate_delta_palette(image: &ImageData) -> Option<usize> {
    if image.color_format.component_count() != 3 {
        return None;
    }
    let width = image.width as usize;
    let (entries, indices) = collect_palette(&image.pixels);
    if entries.is_empty() {
        return None;
    }
    let n = entries.len();
    // 亮度排序（Y ≈ R + 2G + B）并重映射索引：调色板顺序必须与索引一致
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| entries[i][0] + 2 * entries[i][1] + entries[i][2]);
    let mut inv = vec![0i32; n];
    for (sp, &oi) in order.iter().enumerate() {
        inv[oi] = sp as i32;
    }
    let sorted_entries: Vec<[i32; 3]> = order.iter().map(|&i| entries[i]).collect();
    let sorted_indices: Vec<i32> = indices.iter().map(|&ix| inv[ix as usize]).collect();

    let pal_bytes = estimate_palette_bytes(&sorted_entries);
    let (idx_bytes, _k) = estimate_index_bytes(&sorted_indices, width);
    Some(FRAME_HEADER_SIZE + DP_HEADER_LEN + pal_bytes + idx_bytes)
}

/// 运行探针。`root` 为 test/png 根目录。
///
/// 环境变量 `CRF_PROBE_TAKE=N` 限制每组采样帧数（默认全部；仅调试加速用，
/// 判定结论必须基于全量采样）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    let take: usize = std::env::var("CRF_PROBE_TAKE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    println!("=== delta palette 收益探针 ===");
    println!("root: {root}");
    if take != usize::MAX {
        println!("⚠ CRF_PROBE_TAKE={take}：采样受限，结论不可外推");
    }
    println!();

    let mut total_frames = 0usize;
    let mut feasible_frames = 0usize; // 色数 ≤ 上限，成功估算
    let mut win_frames = 0usize; // delta 估算 < 最优候选
    let mut ge3_frames = 0usize; // 收益 ≥ 3%
    let mut sum_best = 0u64;
    let mut sum_min = 0u64; // 候选竞争兜底后（min(best, dp)）的总字节

    println!(
        "{:<14} {:>4} {:>8} {:>12} {:>12} {:>8}",
        "组", "帧", "色数", "最优候选", "delta估算", "收益%"
    );

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
        let golden = &frames[0].pixels;

        for (fi, frame) in frames.iter().enumerate().take(take) {
            if frame.pixels.len() != golden.len() {
                continue;
            }
            let is_first = fi == 0;
            // 路径 G 语义：首帧原样；差分帧 RGB diff 后 RCT 正变换
            let eff = if is_first {
                ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: frame.pixels.clone(),
                }
            } else {
                let mut diff_rgb = vec![0i32; golden.len()];
                sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let diff = match rct::rct_forward(&diff_rgb, components) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: diff,
                }
            };

            // 当前最优候选（含帧头）
            let best = match encode_frame_adaptive(
                &eff,
                CompressionType::GolombRice,
                8,
                is_first,
                FrameQuant::lossless(),
                None,
                None,
            ) {
                Ok(o) => o.data.len(),
                Err(_) => continue,
            };

            let color_count = count_colors(&eff.pixels);
            let dp = if color_count <= MAX_COLORS {
                estimate_delta_palette(&eff)
            } else {
                None
            };

            total_frames += 1;
            sum_best += best as u64;

            let (dp_show, gain_pct) = match dp {
                Some(d) => {
                    feasible_frames += 1;
                    let gain = (best as f64 - d as f64) / best as f64 * 100.0;
                    if d < best {
                        win_frames += 1;
                    }
                    if gain >= GAIN_THRESHOLD_PCT {
                        ge3_frames += 1;
                    }
                    (format!("{d}"), gain)
                }
                None => ("—".to_string(), f64::NAN),
            };
            sum_min += dp.map_or(best as u64, |d| (best as u64).min(d as u64));

            let cc_show = if color_count > MAX_COLORS {
                format!(">{MAX_COLORS}")
            } else {
                format!("{color_count}")
            };
            println!(
                "{:<14} {:>4} {:>8} {:>12} {:>12} {:>8.1}",
                name, fi, cc_show, best, dp_show, gain_pct
            );
        }
    }

    let overall_gain = if sum_best > 0 {
        (sum_best as f64 - sum_min as f64) / sum_best as f64 * 100.0
    } else {
        0.0
    };

    println!("\n--- 汇总 ---");
    println!("总帧数: {total_frames}");
    println!("色数 ≤{MAX_COLORS} 可行帧: {feasible_frames}/{total_frames}");
    println!("delta 估算 < 最优候选: {win_frames} 帧");
    println!("收益 ≥{GAIN_THRESHOLD_PCT}%: {ge3_frames} 帧");
    println!("整体字节: 最优候选 {sum_best} → 候选竞争兜底 {sum_min}（{overall_gain:+.2}%）");
    println!(
        "判定: {}",
        if overall_gain >= GAIN_THRESHOLD_PCT && ge3_frames > 0 {
            "✅ 达标，进入格式设计（M1）"
        } else {
            "❌ 未达 ≥3% 门槛，建议记录否决、不进入格式设计"
        }
    );
    Ok(())
}
