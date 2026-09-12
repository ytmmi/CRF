//! MA 树深度/上下文密度收益探针（P0-A）
//!
//! **背景**：全局范式调研（optimization-review §68 前置）显示——动漫插画的最优
//! 顶层范式就是「预测 + 上下文建模」，但同范式内**上下文建模密度**决定压缩率：
//! CM（EMMA）比 JPEG-XL 最强无损配置再小 11~13%，差距来自上下文模型规模。
//! CRF 的 MA 树当前仅 `MA_MAX_DEPTH=3`（8 叶子）、4 个幅值属性，处于密度光谱
//! 最浅端。本探针量化「加深/加宽 MA 树」的条件熵收益上限。
//!
//! **口径**（每差分帧，RCT 残差 = `rct_forward(frame − golden)`，交织采样）：
//! 对每个深度 d ∈ {3,4,5,6,7,8} 用与生产相同的贪心生长训练 MA 树，
//! 统计残差幅值桶在叶子上下文下的**条件熵**（bits/样本），对比 d=3 基线。
//! 条件熵是实际编码字节的下界代理——熵不降则字节不会降。
//!
//! 零码流改动、零外部数据集。CLI：`--probe-ma-depth [root]`。

use crate::crf::core::color::rct;
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

const MAG_BUCKETS: usize = 5;
const CANDIDATE_THRESHOLDS: [u32; 4] = [2, 6, 14, 30];
const MA_MIN_SAMPLES: usize = 192;
const MA_MIN_GAIN: f64 = 0.04;
const MAX_SAMPLES: usize = 16384;

fn depth_list() -> Vec<usize> {
    let raw = std::env::var("CRF_PROBE_MA_DEPTHS").unwrap_or_else(|_| "3,4,5,6,7,8".to_string());
    let mut v: Vec<usize> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&d| (1..=12).contains(&d))
        .collect();
    v.sort_unstable();
    v.dedup();
    if v.is_empty() {
        v.push(3);
    }
    v
}

fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_MA_DEPTH_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

#[inline]
fn mag_bucket(abs_v: u32) -> usize {
    match abs_v {
        0 => 0,
        1..=4 => 1,
        5..=16 => 2,
        17..=64 => 3,
        _ => 4,
    }
}

#[derive(Clone, Copy)]
struct TrainSample {
    attrs: [u32; 4],
    bucket: usize,
}

struct Node {
    is_leaf: bool,
    attr: usize,
    threshold: u32,
    children: [usize; 2],
}

fn impurity(samples: &[TrainSample], idx: &[usize]) -> f64 {
    if idx.is_empty() {
        return 0.0;
    }
    let mut hist = [0u32; MAG_BUCKETS];
    for &i in idx {
        hist[samples[i].bucket] += 1;
    }
    let n = idx.len() as f64;
    hist.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

#[allow(clippy::too_many_arguments)]
fn grow(
    samples: &[TrainSample],
    subset: &[usize],
    depth: usize,
    max_depth: usize,
    max_nodes: usize,
    next_node: &mut usize,
    nodes: &mut Vec<Node>,
) -> usize {
    let can_split =
        depth < max_depth && subset.len() >= MA_MIN_SAMPLES && *next_node + 2 <= max_nodes;

    let mut best_gain = MA_MIN_GAIN;
    let mut best: Option<(usize, u32, Vec<usize>, Vec<usize>)> = None;

    if can_split {
        for attr in 0..4 {
            let lo = subset
                .iter()
                .map(|&i| samples[i].attrs[attr])
                .min()
                .unwrap_or(0);
            let hi = subset
                .iter()
                .map(|&i| samples[i].attrs[attr])
                .max()
                .unwrap_or(0);
            if lo == hi {
                continue;
            }
            for &t in CANDIDATE_THRESHOLDS.iter() {
                if t <= lo || t > hi {
                    continue;
                }
                let mut left_set = Vec::new();
                let mut right_set = Vec::new();
                for &i in subset {
                    if samples[i].attrs[attr] < t {
                        left_set.push(i);
                    } else {
                        right_set.push(i);
                    }
                }
                if left_set.len() < MA_MIN_SAMPLES / 4 || right_set.len() < MA_MIN_SAMPLES / 4 {
                    continue;
                }
                let parent_imp = impurity(samples, subset);
                let gain = parent_imp
                    - (impurity(samples, &left_set) * left_set.len() as f64
                        + impurity(samples, &right_set) * right_set.len() as f64)
                        / subset.len() as f64;
                if gain > best_gain {
                    best_gain = gain;
                    best = Some((attr, t, left_set, right_set));
                }
            }
        }
    }

    let my_idx = *next_node;
    *next_node += 1;
    match best {
        Some((attr, t, left_set, right_set)) => {
            nodes.push(Node {
                is_leaf: false,
                attr,
                threshold: t,
                children: [0, 0],
            });
            let li = grow(
                samples,
                &left_set,
                depth + 1,
                max_depth,
                max_nodes,
                next_node,
                nodes,
            );
            let ri = grow(
                samples,
                &right_set,
                depth + 1,
                max_depth,
                max_nodes,
                next_node,
                nodes,
            );
            nodes[my_idx].children = [li, ri];
            my_idx
        }
        None => {
            nodes.push(Node {
                is_leaf: true,
                attr: 0,
                threshold: 0,
                children: [0, 0],
            });
            my_idx
        }
    }
}

fn walk(nodes: &[Node], attrs: [u32; 4]) -> usize {
    let mut n = 0usize;
    while !nodes[n].is_leaf {
        let node = &nodes[n];
        n = if attrs[node.attr] < node.threshold {
            node.children[0]
        } else {
            node.children[1]
        };
    }
    n
}

/// 条件熵（bits/样本）：`Σ_leaf H(bucket | leaf) · n_leaf / n`。
fn conditional_entropy(nodes: &[Node], samples: &[TrainSample]) -> (f64, usize) {
    let mut leaf_hist: Vec<[u32; MAG_BUCKETS]> = Vec::new();
    let mut leaf_of = vec![usize::MAX; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        if node.is_leaf {
            leaf_of[i] = leaf_hist.len();
            leaf_hist.push([0u32; MAG_BUCKETS]);
        }
    }
    for s in samples {
        let leaf = walk(nodes, s.attrs);
        leaf_hist[leaf_of[leaf]][s.bucket] += 1;
    }
    let n = samples.len() as f64;
    let mut total = 0.0;
    for hist in &leaf_hist {
        let ln: u32 = hist.iter().sum();
        if ln == 0 {
            continue;
        }
        let lf = ln as f64;
        let h: f64 = hist
            .iter()
            .filter(|&&c| c > 0)
            .map(|&c| {
                let p = c as f64 / lf;
                -p * p.log2()
            })
            .sum();
        total += h * lf;
    }
    (total / n, leaf_hist.len())
}

/// 从 RCT 残差构造训练样本（与生产 `build_ma_tree` 同口径，交织 stride）。
fn make_samples(pixels: &[i32], stride: usize) -> Vec<TrainSample> {
    let st = stride;
    if st == 0 || pixels.len() <= st + 1 {
        return Vec::new();
    }
    let step = (pixels.len() / MAX_SAMPLES).max(1);
    let mut samples = Vec::with_capacity(MAX_SAMPLES.min(pixels.len()));
    let mut i = st + 1;
    while i < pixels.len() {
        let l = pixels[i - 1].unsigned_abs();
        let t = pixels[i - st].unsigned_abs();
        let tl = pixels[i - st - 1].unsigned_abs();
        let tr = if i + 1 >= st {
            pixels[i + 1 - st].unsigned_abs()
        } else {
            0
        };
        samples.push(TrainSample {
            attrs: [l.min(255), t.min(255), tl.min(255), tr.min(255)],
            bucket: mag_bucket(pixels[i].unsigned_abs()),
        });
        i += step;
    }
    samples
}

struct DepthStat {
    entropy: f64,
    leaves: usize,
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

    let depths = depth_list();
    let limit = group_frame_limit();
    println!("=== MA 树深度/上下文密度收益探针（P0-A）===");
    println!("root: {root}");
    println!(
        "扫描深度: {depths:?}  |  每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: RCT 残差 → 幅值桶条件熵（bits/样本，越低越好）\n");

    // 全局按深度聚合熵与样本量
    let mut global: Vec<(f64, usize)> = vec![(0.0, 0); depths.len()];

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
        let stride = frames[0].width as usize * components;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        let stats: Vec<Vec<DepthStat>> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<Vec<DepthStat>, String> {
                let frame = &frames[i];
                let mut diff_rgb = vec![0i32; golden.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let residuals =
                    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;
                let samples = make_samples(&residuals, stride);
                if samples.is_empty() {
                    return Ok(Vec::new());
                }
                let all: Vec<usize> = (0..samples.len()).collect();
                let mut out = Vec::with_capacity(depths.len());
                for &d in &depths {
                    let max_nodes = (1usize << (d + 1)) - 1;
                    let mut nodes: Vec<Node> = Vec::with_capacity(max_nodes);
                    let mut next_node = 0usize;
                    grow(&samples, &all, 0, d, max_nodes, &mut next_node, &mut nodes);
                    let (entropy, leaves) = conditional_entropy(&nodes, &samples);
                    out.push(DepthStat { entropy, leaves });
                }
                Ok(out)
            })
            .collect::<Result<Vec<Vec<DepthStat>>, String>>()?;

        // 帧级加权聚合（按样本量近似用等权——各帧样本量相近）
        let mut sums: Vec<(f64, usize)> = vec![(0.0, 0); depths.len()];
        let mut frames_used = 0usize;
        for frame_stats in &stats {
            if frame_stats.is_empty() {
                continue;
            }
            frames_used += 1;
            for (k, ds) in frame_stats.iter().enumerate() {
                sums[k].0 += ds.entropy;
                sums[k].1 += ds.leaves;
            }
        }
        if frames_used == 0 {
            continue;
        }
        let base = sums[0].0 / frames_used as f64;
        print!("组 {:<12} ({} 帧): ", name, frames_used);
        for (k, &d) in depths.iter().enumerate() {
            let e = sums[k].0 / frames_used as f64;
            let pct = if base == 0.0 {
                0.0
            } else {
                (e - base) / base * 100.0
            };
            print!("d{d}={e:.4}({pct:+.2}%) ");
            global[k].0 += sums[k].0;
            global[k].1 += frames_used;
        }
        println!();
    }

    println!("\n=== 汇总（全局平均条件熵）===");
    let base = if global[0].1 > 0 {
        global[0].0 / global[0].1 as f64
    } else {
        0.0
    };
    for (k, &d) in depths.iter().enumerate() {
        let e = if global[k].1 > 0 {
            global[k].0 / global[k].1 as f64
        } else {
            0.0
        };
        let pct = if base == 0.0 {
            0.0
        } else {
            (e - base) / base * 100.0
        };
        println!("深度 {d}: 条件熵 {e:.5} bits/样本  ({pct:+.2}% vs d3)");
    }
    let best_k = depths
        .iter()
        .enumerate()
        .min_by(|a, b| {
            global[a.0]
                .0
                .partial_cmp(&global[b.0].0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(k, _)| k)
        .unwrap_or(0);
    let best_e = if global[best_k].1 > 0 {
        global[best_k].0 / global[best_k].1 as f64
    } else {
        0.0
    };
    let best_pct = if base == 0.0 {
        0.0
    } else {
        (best_e - base) / base * 100.0
    };
    println!(
        "\n判定：最深最优深度 d{} 相对 d3 条件熵 {:+.2}% —— {}",
        depths[best_k],
        best_pct,
        if best_pct <= -3.0 {
            "加深 MA 树有显著收益（≥3%），值得实施"
        } else {
            "加深 MA 树收益有限（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeper_tree_never_increases_impurity() {
        // 合成样本：attrs[0] 与 bucket 强相关 → 加深树应降低条件熵
        let mut samples = Vec::new();
        for i in 0..4096u32 {
            let a = i % 64;
            samples.push(TrainSample {
                attrs: [a, 0, 0, 0],
                bucket: if a < 8 { 4 } else { 0 },
            });
        }
        let all: Vec<usize> = (0..samples.len()).collect();
        let mut entropies = Vec::new();
        for d in [1usize, 2, 3, 4] {
            let max_nodes = (1usize << (d + 1)) - 1;
            let mut nodes = Vec::new();
            let mut next = 0;
            grow(&samples, &all, 0, d, max_nodes, &mut next, &mut nodes);
            let (e, _) = conditional_entropy(&nodes, &samples);
            entropies.push(e);
        }
        // 更深或相等（贪心只会下降不纯度）
        for w in entropies.windows(2) {
            assert!(w[1] <= w[0] + 1e-9, "更深树熵上升: {:?}", entropies);
        }
    }

    #[test]
    fn mag_bucket_boundaries() {
        assert_eq!(mag_bucket(0), 0);
        assert_eq!(mag_bucket(1), 1);
        assert_eq!(mag_bucket(4), 1);
        assert_eq!(mag_bucket(5), 2);
        assert_eq!(mag_bucket(16), 2);
        assert_eq!(mag_bucket(17), 3);
        assert_eq!(mag_bucket(65), 4);
    }
}
