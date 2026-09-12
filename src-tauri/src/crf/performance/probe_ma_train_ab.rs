//! MA 树训练参数 A/B 真实编码字节探针（P0-A 第二杠杆）
//!
//! **背景**：§69 证明「加深 MA 树」只换来 −0.3~−1.2% 真实字节——瓶颈不在深度，
//! 而在**训练参数**：`MA_MIN_GAIN=0.04` 与仅 4 个候选阈值 `[2,6,14,30]`
//! （所有 >30 的残差被 lump 进同一侧，大幅值尾区完全无区分度）。本探针扫描
//! `min_gain × 阈值集 × 深度` 网格，先廉价代理筛选、再真实字节确认。
//!
//! **口径**（每差分帧，与生产路径-G 完全一致）：
//! `diff_rgb = frame − golden` → `rct_forward` → RCT 残差；k 与生产相同。
//! - **Stage 1（廉价筛选）**：对每个参数组合用生产训练函数 `build_ma_tree_with_params`
//!   建树 → 精确树头字节 `serialize().len()` + 训练样本幅值桶条件熵代理
//!   `header + H×总像素/8`（把 §69 的树头开销显式计入）；全局代理升序取 top K。
//! - **Stage 2（真实字节确认）**：对 top K + 默认锚点调 `encode_ma_variant_with_params`
//!   （含树头 + CABAC 全帧真实字节）+ Gradient/Uniform 基线，整帧竞争
//!   `min(MA, G, U)`；**最终判定只看真实字节**，Stage 1 仅用于排序控成本。
//!
//! 零码流改动、零外部数据集。CLI：`--probe-ma-train-ab [root]`。
//! 环境变量：`CRF_PROBE_MA_GAINS`（默认 `0.04,0.02,0.01,0.005,0.001`）、
//! `CRF_PROBE_MA_DEPTHS`（默认 `3,4,5,6`）、`CRF_PROBE_MA_THRESHOLD_SET`
//! （默认 `base,dense,log`）、`CRF_PROBE_MA_THRESHOLDS`（显式逗号列表，覆盖集名）、
//! `CRF_PROBE_MA_TOP`（Stage 2 保留组合数，默认 6）、`CRF_PROBE_MA_DEPTH_MAX`（帧数上限）。

use crate::crf::core::color::rct;
use crate::crf::core::entropy::context::{
    build_ma_tree_with_params, MaTrainParams, MaTree, MA_MIN_SAMPLES,
};
use crate::crf::encoder::rle_cabac::{
    encode_cabac_variant, encode_ma_variant_with_params, CabacEncoder,
};
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

const MAG_BUCKETS: usize = 5;

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

/// 训练采样（与生产 `build_ma_tree_with_params` 同口径：从 0 起 `step_by`，
/// attr 用 `unsigned_abs()` **不** clamp——§69 探针曾误加 `.min(255)` 造成保真度缺口）。
fn make_samples(pixels: &[i32], stride: usize) -> Vec<TrainSample> {
    let total = pixels.len();
    if total <= stride + 1 {
        return Vec::new();
    }
    let step = total.div_ceil(16384).max(1);
    let mut samples = Vec::with_capacity(total / step + 1);
    for i in (0..total).step_by(step) {
        let l = if i >= 1 {
            pixels[i - 1].unsigned_abs()
        } else {
            0
        };
        let t = if i >= stride {
            pixels[i - stride].unsigned_abs()
        } else {
            0
        };
        let tl = if i > stride {
            pixels[i - stride - 1].unsigned_abs()
        } else {
            0
        };
        let tr = if i + 1 >= stride {
            pixels[i + 1 - stride].unsigned_abs()
        } else {
            0
        };
        samples.push(TrainSample {
            attrs: [l, t, tl, tr],
            bucket: mag_bucket(pixels[i].unsigned_abs()),
        });
    }
    samples
}

/// 条件熵（bits/样本）：`Σ_leaf H(bucket | leaf) · n_leaf / n`。
fn conditional_entropy(tree: &MaTree, samples: &[TrainSample]) -> f64 {
    let leaf_count = tree.leaf_count();
    let mut hist = vec![[0u32; MAG_BUCKETS]; leaf_count];
    for s in samples {
        let leaf = tree.walk(s.attrs[0], s.attrs[1], s.attrs[2], s.attrs[3]);
        hist[leaf][s.bucket] += 1;
    }
    let n = samples.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut total = 0.0;
    for h in &hist {
        let ln: u32 = h.iter().sum();
        if ln == 0 {
            continue;
        }
        let lf = ln as f64;
        let ent: f64 = h
            .iter()
            .filter(|&&c| c > 0)
            .map(|&c| {
                let p = c as f64 / lf;
                -p * p.log2()
            })
            .sum();
        total += ent * lf;
    }
    total / n
}

#[derive(Clone)]
struct Combo {
    gain: f64,
    thr_name: String,
    thresholds: Vec<u32>,
    depth: usize,
}

impl Combo {
    fn params(&self) -> MaTrainParams {
        MaTrainParams {
            min_gain: self.gain,
            thresholds: self.thresholds.clone(),
            min_samples: MA_MIN_SAMPLES,
        }
    }

    fn label(&self) -> String {
        format!("g{}·{}·d{}", self.gain, self.thr_name, self.depth)
    }
}

fn parse_f64_list(raw: &str, fallback: &[f64]) -> Vec<f64> {
    let v: Vec<f64> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .filter(|g| g.is_finite() && *g >= 0.0)
        .collect();
    if v.is_empty() {
        fallback.to_vec()
    } else {
        v
    }
}

fn parse_usize_list(raw: &str, fallback: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&d| (1..=6).contains(&d))
        .collect();
    v.sort_unstable();
    v.dedup();
    if v.is_empty() {
        fallback.to_vec()
    } else {
        v
    }
}

fn threshold_set(name: &str) -> Option<Vec<u32>> {
    match name {
        "base" => Some(vec![2, 6, 14, 30]),
        "dense" => Some(vec![1, 2, 3, 4, 6, 8, 11, 14, 20, 30, 45, 64, 96, 128]),
        "log" => Some(vec![
            1, 2, 3, 4, 6, 8, 11, 14, 18, 23, 30, 39, 51, 66, 85, 110, 142, 183, 236, 255,
        ]),
        _ => None,
    }
}

fn build_combos() -> Vec<Combo> {
    let gains = parse_f64_list(
        &std::env::var("CRF_PROBE_MA_GAINS").unwrap_or_default(),
        &[0.04, 0.02, 0.01, 0.005, 0.001],
    );
    let depths = parse_usize_list(
        &std::env::var("CRF_PROBE_MA_DEPTHS").unwrap_or_default(),
        &[3, 4, 5, 6],
    );
    // 阈值集：显式列表优先，否则按集名。
    let mut sets: Vec<(String, Vec<u32>)> = Vec::new();
    if let Ok(raw) = std::env::var("CRF_PROBE_MA_THRESHOLDS") {
        let mut t: Vec<u32> = raw
            .split(',')
            .filter_map(|s| s.trim().parse::<u32>().ok())
            .filter(|&x| (1..=255).contains(&x))
            .collect();
        t.sort_unstable();
        t.dedup();
        if !t.is_empty() {
            sets.push(("custom".to_string(), t));
        }
    }
    if sets.is_empty() {
        let names =
            std::env::var("CRF_PROBE_MA_THRESHOLD_SET").unwrap_or_else(|_| "base,dense,log".into());
        for n in names.split(',').map(str::trim) {
            if let Some(t) = threshold_set(n) {
                sets.push((n.to_string(), t));
            }
        }
    }
    if sets.is_empty() {
        sets.push(("base".to_string(), vec![2, 6, 14, 30]));
    }
    let mut combos = Vec::new();
    for &gain in &gains {
        for (thr_name, thresholds) in &sets {
            for &depth in &depths {
                combos.push(Combo {
                    gain,
                    thr_name: thr_name.clone(),
                    thresholds: thresholds.clone(),
                    depth,
                });
            }
        }
    }
    combos
}

fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_MA_DEPTH_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

fn stage2_top() -> usize {
    std::env::var("CRF_PROBE_MA_TOP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(6)
}

/// 收集所有可用组的目录（不预加载——逐组处理以控内存，避免全组帧同时驻留）。
fn collect_group_dirs(root: &str) -> Result<Vec<(String, std::path::PathBuf)>, String> {
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
    Ok(groups
        .into_iter()
        .map(|p| {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            (name, p)
        })
        .collect())
}

/// 加载单组帧并应用帧数上限；非「≥2 帧同尺寸 RGB」组返回 None。
fn load_group(dir: &std::path::Path) -> Option<Vec<crate::crf::ImageData>> {
    let mut frames = match load_frames(&dir.to_string_lossy()) {
        Ok(f) if f.len() >= 2 => f,
        _ => return None,
    };
    let limit = group_frame_limit();
    if limit > 0 && frames.len() > limit {
        frames.truncate(limit);
    }
    if frames[0].color_format.component_count() != 3 {
        return None;
    }
    let px = frames[0].pixels.len();
    if frames.iter().any(|f| f.pixels.len() != px) {
        return None;
    }
    Some(frames)
}

/// 单帧 RCT 残差（生产路径-G 语义）。
fn residuals_of(
    frame: &crate::crf::ImageData,
    golden: &[i32],
    components: usize,
) -> Result<Vec<i32>, String> {
    let mut diff_rgb = vec![0i32; golden.len()];
    crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())
}

/// 运行探针。
pub fn run(root: &str) -> Result<(), String> {
    let combos = build_combos();
    let top_k = stage2_top();
    let dirs = collect_group_dirs(root)?;
    if dirs.is_empty() {
        println!("未找到可用图像组（需要 ≥2 帧的 RGB 组）");
        return Ok(());
    }

    println!("=== MA 树训练参数 A/B 真实编码字节探针（P0-A 第二杠杆）===");
    println!("root: {root}");
    println!(
        "网格: {} 组合（gain {} × 阈值集 {} × 深度 {}）",
        combos.len(),
        combos.iter().map(|c| c.gain).collect::<Vec<_>>().len(),
        combos
            .iter()
            .map(|c| c.thr_name.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        combos.iter().map(|c| c.depth).max().unwrap_or(0),
    );
    println!("Stage 2 保留 top {top_k} + 默认锚点\n");

    // ===== Stage 1：廉价代理筛选（全局累加）=====
    let mut proxy_sum = vec![0f64; combos.len()];
    let mut frames_total = 0usize;
    for (_name, dir) in &dirs {
        let Some(frames) = load_group(dir) else {
            continue;
        };
        let golden = &frames[0].pixels;
        let stride = frames[0].width as usize * 3;
        let per_frame: Vec<Vec<f64>> = (1..frames.len())
            .into_par_iter()
            .map(|i| -> Result<Vec<f64>, String> {
                let residuals = residuals_of(&frames[i], golden, 3)?;
                let samples = make_samples(&residuals, stride);
                let mut out = Vec::with_capacity(combos.len());
                for c in &combos {
                    let tree =
                        build_ma_tree_with_params(&residuals, Some(stride), c.depth, &c.params())
                            .map_err(|e| e.to_string())?;
                    let header = tree.serialize().len() as f64;
                    let h = conditional_entropy(&tree, &samples);
                    out.push(header + h * residuals.len() as f64 / 8.0);
                }
                Ok(out)
            })
            .collect::<Result<Vec<_>, String>>()?;
        for row in per_frame {
            for (ci, v) in row.into_iter().enumerate() {
                proxy_sum[ci] += v;
            }
            frames_total += 1;
        }
    }

    // 全局代理排序 → top K
    let mut order: Vec<usize> = (0..combos.len()).collect();
    order.sort_by(|&a, &b| {
        proxy_sum[a]
            .partial_cmp(&proxy_sum[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let default_idx = combos
        .iter()
        .position(|c| (c.gain - 0.04).abs() < 1e-12 && c.thr_name == "base" && c.depth == 3)
        .unwrap_or(0);
    let mut selected: Vec<usize> = order.iter().copied().take(top_k).collect();
    if !selected.contains(&default_idx) {
        selected.push(default_idx);
    }

    println!(
        "--- Stage 1 代理（header + 条件熵×像素/8，升序 top {}）---",
        top_k
    );
    for &ci in order.iter().take(top_k.max(10)) {
        println!(
            "  {:<20} 代理 {:>14.0} B  (header 均值 {:.1} B/帧)",
            combos[ci].label(),
            proxy_sum[ci],
            proxy_sum[ci] / frames_total.max(1) as f64
        );
    }
    let default_proxy = proxy_sum[default_idx];
    println!(
        "  默认锚点 {} 代理 {:.0} B\n",
        combos[default_idx].label(),
        default_proxy
    );

    // ===== Stage 2：真实字节确认（top K + 默认锚点）=====
    let sel_set: std::collections::BTreeSet<usize> = selected.iter().copied().collect();
    let mut real_sum = vec![0u64; combos.len()];
    let mut base_sum = 0u64;
    let mut frames_used = 0usize;
    for (name, dir) in &dirs {
        let Some(frames) = load_group(dir) else {
            continue;
        };
        let golden = &frames[0].pixels;
        let stride = frames[0].width as usize * 3;
        let per_frame: Vec<(u64, Vec<(usize, u64)>)> = (1..frames.len())
            .into_par_iter()
            .map(|i| -> Result<(u64, Vec<(usize, u64)>), String> {
                let residuals = residuals_of(&frames[i], golden, 3)?;
                let k = CabacEncoder::adaptive(&residuals).k();
                let g = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 1)
                    .map(|o| o.map(|(_, t)| t).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX) as u64;
                let u = encode_cabac_variant(&residuals, k, Some(stride), usize::MAX, 2)
                    .map(|o| o.map(|(_, t)| t).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX) as u64;
                let d = encode_ma_variant_with_params(
                    &residuals,
                    k,
                    Some(stride),
                    usize::MAX,
                    3,
                    &MaTrainParams::default(),
                )
                .map(|o| o.map(|(_, t)| t).unwrap_or(usize::MAX))
                .unwrap_or(usize::MAX) as u64;
                let base = d.min(g).min(u);
                let mut per_combo = Vec::new();
                for &ci in &sel_set {
                    let c = &combos[ci];
                    let b = encode_ma_variant_with_params(
                        &residuals,
                        k,
                        Some(stride),
                        usize::MAX,
                        c.depth,
                        &c.params(),
                    )
                    .map(|o| o.map(|(_, t)| t).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX) as u64;
                    per_combo.push((ci, base.min(b)));
                }
                Ok((base, per_combo))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut g_base = 0u64;
        let mut g_combo = vec![0u64; combos.len()];
        let mut g_used = 0usize;
        for (base, per_combo) in per_frame {
            g_base += base;
            for (ci, v) in per_combo {
                g_combo[ci] += v;
            }
            g_used += 1;
        }
        if g_used == 0 {
            continue;
        }
        if let Some((bci, bv)) = sel_set
            .iter()
            .map(|&ci| (ci, g_combo[ci]))
            .min_by_key(|&(_, v)| v)
        {
            let gpct = if g_base == 0 {
                0.0
            } else {
                (bv as f64 - g_base as f64) / g_base as f64 * 100.0
            };
            println!(
                "组 {name:<12} ({} 帧): base={g_base} best={bv} ({gpct:+.2}%) [{}]",
                g_used,
                combos[bci].label()
            );
        }
        base_sum += g_base;
        for (ci, v) in g_combo.iter().enumerate() {
            real_sum[ci] += v;
        }
        frames_used += g_used;
    }

    println!(
        "--- Stage 2 真实字节（整帧竞争 min(MA,G,U)，{} 帧）---",
        frames_used
    );
    println!("  默认锚点 base = {base_sum} B");
    let mut ranked: Vec<(usize, u64)> = sel_set.iter().map(|&ci| (ci, real_sum[ci])).collect();
    ranked.sort_by_key(|&(_, v)| v);
    for (ci, v) in &ranked {
        let pct = if base_sum == 0 {
            0.0
        } else {
            (*v as f64 - base_sum as f64) / base_sum as f64 * 100.0
        };
        println!(
            "  {:<20} {:>14} B  ({pct:+.2}% vs 默认)",
            combos[*ci].label(),
            v
        );
    }
    let (best_ci, best_v) = ranked.first().copied().unwrap_or((default_idx, base_sum));
    let best_pct = if base_sum == 0 {
        0.0
    } else {
        (best_v as f64 - base_sum as f64) / base_sum as f64 * 100.0
    };
    println!(
        "\n判定：最优组合 {} 整帧竞争相对默认 {:+.2}% —— {}",
        combos[best_ci].label(),
        best_pct,
        if best_pct <= -3.0 {
            "放宽训练参数有显著收益（≥3%），值得实施"
        } else {
            "放宽训练参数收益有限（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mag_bucket_boundaries() {
        assert_eq!(mag_bucket(0), 0);
        assert_eq!(mag_bucket(4), 1);
        assert_eq!(mag_bucket(5), 2);
        assert_eq!(mag_bucket(16), 2);
        assert_eq!(mag_bucket(17), 3);
        assert_eq!(mag_bucket(65), 4);
    }

    #[test]
    fn threshold_sets_nonempty_and_in_range() {
        for name in ["base", "dense", "log"] {
            let t = threshold_set(name).unwrap();
            assert!(!t.is_empty());
            assert!(t.iter().all(|&x| (1..=255).contains(&x)), "{name} 越界");
        }
    }
}
