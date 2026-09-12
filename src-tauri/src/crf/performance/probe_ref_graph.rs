//! 参考结构图（MST / 深度约束森林）收益探针
//!
//! 背景：CRF 差分序列当前的参考结构是「星型」（全部差分帧相对首帧 golden）
//! 或「时间链」（previous/prev2 贪心竞争）。图像集/相册压缩领域的经典结论
//! （Gergel 2006 *A Unified Framework for Image Set Compression*、Zou 2012
//! APSIPA、Ling 2014 ISCAS）指出：把每帧视为图节点、帧间预测代价为边权，
//! **无损压缩下最小生成树（MST）即最优预测结构**——它可发现"非相邻但更相似"
//! 的变体对，而非固定时间相邻。
//!
//! 本探针在真实二次元差分组上对比四种参考结构的差分编码字节：
//! - **A 星型 golden**：`ref(i) = frame0`（当前路径 G 默认语义）；
//! - **B 时间链 previous**：`ref(i) = frame(i-1)`（纯链式，误差累积对照）；
//! - **H Hybrid 贪心**：`ref(i) = argmin bytes{ golden, i-1, i-2 }`（CRF
//!   `reference_mode=Auto→Hybrid` 的实际机制，三候选取最小）；
//! - **C MST**：边权 = 采样 SAD，Prim 求最小生成树（根 = frame0），
//!   子节点参考父节点。
//!
//! **判定基线是 H**（而非 B）——H 才是 CRF 现状的真实对照。
//!
//! 输出 SAD 估计与**实际编码字节**双口径，并统计 MST 树深度（随机访问代价）。
//! 零码流改动、零外部数据集；仅扫 `test/png`（可用环境变量覆盖）。
//!
//! CLI：`--probe-ref-graph [root]`（默认 `E:\CRF\test\png`）。
//!
//! 判定门槛（项目统一 ≥3%）：C 相对 B 平均字节下降 ≥3% 才进入实施评估；
//! 否则记录为「已探针证伪」关闭。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

/// SAD 采样步长（每 N 个 i32 分量采样一次，降低大图 O(N²) 估计成本）。
/// `CRF_PROBE_REF_GRAPH_STRIDE` 可覆盖。
fn sad_stride() -> usize {
    std::env::var("CRF_PROBE_REF_GRAPH_STRIDE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(4)
}

/// 每组参与探针的最大帧数（防止超大组 O(N²) 过慢）。
/// `CRF_PROBE_REF_GRAPH_MAX` 可覆盖；0 或未设 = 不限。
fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_REF_GRAPH_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// 采样 SAD：`sum |a[k] - b[k]|`，按 `stride` 跳采样。
fn sampled_sad(a: &[i32], b: &[i32], stride: usize) -> u64 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    let mut i = 0usize;
    while i < a.len() {
        sum = sum.saturating_add((a[i] - b[i]).unsigned_abs() as u64);
        i += stride;
    }
    sum
}

/// 单条差分边的实际编码字节：`diff = frame − ref` → RCT → 自适应编码。
///
/// 与路径 G 差分帧语义一致（RGB 域差分后 RCT），无损量化。
fn encode_diff_bytes(
    frame: &ImageData,
    reference: &[i32],
    components: usize,
) -> Result<usize, String> {
    let mut diff_rgb = vec![0i32; frame.pixels.len()];
    crate::crf::backend::ops::sub_i32(&frame.pixels, reference, &mut diff_rgb);
    let diff = rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;
    let eff = ImageData {
        width: frame.width,
        height: frame.height,
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
    Ok(out.data.len())
}

/// Prim 最小生成树（以 `root` 为起点）。
///
/// 返回 `(parent, order, total)`：`parent[v]` 为 v 在树中的父节点（`root` 的
/// parent = usize::MAX），`order` 为入树顺序（用于 O(n) 计算深度），`total`
/// 为 MST 总边权。确定性：最小边权平局时取最小下标。
fn prim_mst(cost: &[Vec<u64>], root: usize) -> (Vec<usize>, Vec<usize>, u64) {
    let n = cost.len();
    let mut in_tree = vec![false; n];
    let mut min_edge = vec![u64::MAX; n];
    let mut parent = vec![usize::MAX; n];
    let mut order = Vec::with_capacity(n);
    let mut total = 0u64;
    min_edge[root] = 0;

    for _ in 0..n {
        // 选取未入树节点中 min_edge 最小者（平局取最小下标，保证确定性）。
        let mut u = usize::MAX;
        let mut best = u64::MAX;
        for v in 0..n {
            if !in_tree[v] && min_edge[v] < best {
                best = min_edge[v];
                u = v;
            }
        }
        if u == usize::MAX {
            break;
        }
        in_tree[u] = true;
        order.push(u);
        total = total.saturating_add(min_edge[u]);
        for v in 0..n {
            if !in_tree[v] && cost[u][v] < min_edge[v] {
                min_edge[v] = cost[u][v];
                parent[v] = u;
            }
        }
    }
    (parent, order, total)
}

/// 由 Prim 的 `parent` + 入树 `order` 计算每个节点的树深度。
fn tree_depths(parent: &[usize], order: &[usize], root: usize) -> Vec<usize> {
    let mut depth = vec![0usize; parent.len()];
    for &v in order {
        if v == root {
            continue;
        }
        let p = parent[v];
        if p != usize::MAX {
            depth[v] = depth[p] + 1;
        }
    }
    depth
}

/// 单组探针统计。
struct GroupStat {
    name: String,
    frames: usize,
    sad_a: u64,
    sad_b: u64,
    sad_c: u64,
    bytes_a: u64,
    bytes_b: u64,
    bytes_h: u64,
    bytes_c: u64,
    max_depth: usize,
}

/// 运行探针。`root` 为 test/png 根目录（含多个图像组子目录）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();
    // 支持 `root` 直接指向单个图像组（本身含 ≥2 张图）——便于单组快速验证。
    if load_frames(root).map(|f| f.len() >= 2).unwrap_or(false) {
        groups = vec![std::path::PathBuf::from(root)];
    }

    let stride = sad_stride();
    let limit = group_frame_limit();

    println!("=== 参考结构图（MST）收益探针 ===");
    println!("root: {root}");
    println!(
        "SAD 采样步长: {stride}  |  每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("方案 A=星型 golden / B=时间链 previous / C=MST（根=frame0）\n");

    let mut stats: Vec<GroupStat> = Vec::new();

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
        // 尺寸一致性校验（load_frames 已保证同组同尺寸，防御性检查）。
        let px_len = frames[0].pixels.len();
        if frames.iter().any(|f| f.pixels.len() != px_len) {
            continue;
        }

        // ===== 1. 边权矩阵（采样 SAD，对称）=====
        let mut cost = vec![vec![0u64; n]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let w = sampled_sad(&frames[i].pixels, &frames[j].pixels, stride);
                cost[i][j] = w;
                cost[j][i] = w;
            }
        }

        // ===== 2. 三结构 SAD 估计 =====
        // A 星型：全部差分帧参考 frame0
        let sad_a: u64 = (1..n).map(|i| cost[i][0]).sum();
        // B 时间链：frame(i) 参考 frame(i-1)
        let sad_b: u64 = (1..n).map(|i| cost[i][i - 1]).sum();
        // C MST（根 = frame0）
        let (parent, order, sad_c) = prim_mst(&cost, 0);
        let depth = tree_depths(&parent, &order, 0);
        let max_depth = depth.iter().copied().max().unwrap_or(0);

        // ===== 3. 实际编码字节（无损差分，逐边编码；帧级并行）=====
        // 每帧编码 4 条候选边：a=golden、p1=previous(i-1)、p2=prev2(i-2)、
        // c=MST 父节点。Hybrid（CRF 实际机制，reference_mode=Auto→Hybrid）
        // 为逐帧贪心：h = min(a, p1, p2)，这才是与 MST 对照的真实基线。
        let per_frame: Vec<(u64, u64, u64, u64, u64)> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<(u64, u64, u64, u64, u64), String> {
                let a = encode_diff_bytes(&frames[i], &frames[0].pixels, components)? as u64;
                let p1 = encode_diff_bytes(&frames[i], &frames[i - 1].pixels, components)? as u64;
                let p2 = if i >= 2 {
                    encode_diff_bytes(&frames[i], &frames[i - 2].pixels, components)? as u64
                } else {
                    u64::MAX
                };
                let h = a.min(p1).min(p2);
                let p = parent[i];
                let ref_pixels = if p == usize::MAX || p == i {
                    &frames[0].pixels
                } else {
                    &frames[p].pixels
                };
                let c = encode_diff_bytes(&frames[i], ref_pixels, components)? as u64;
                Ok((a, p1, p2, h, c))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let bytes_a: u64 = per_frame.iter().map(|x| x.0).sum();
        let bytes_b: u64 = per_frame.iter().map(|x| x.1).sum();
        let bytes_h: u64 = per_frame.iter().map(|x| x.3).sum();
        let bytes_c: u64 = per_frame.iter().map(|x| x.4).sum();

        let stat = GroupStat {
            name: name.clone(),
            frames: n,
            sad_a,
            sad_b,
            sad_c,
            bytes_a,
            bytes_b,
            bytes_h,
            bytes_c,
            max_depth,
        };

        // 逐组明细
        let pct = |c: u64, b: u64| -> f64 {
            if b == 0 {
                0.0
            } else {
                (c as f64 - b as f64) / b as f64 * 100.0
            }
        };
        println!(
            "组 {:<12} ({} 帧): SAD A/B/C={}/{}/{} | 字节 A/B/H/C={}/{}/{}/{} | C vs H {:+.2}% | 深度 {}",
            name,
            n,
            sad_a,
            sad_b,
            sad_c,
            bytes_a,
            bytes_b,
            bytes_h,
            bytes_c,
            pct(bytes_c, bytes_h),
            max_depth,
        );
        stats.push(stat);
    }

    // ===== 4. 汇总 =====
    println!("\n=== 汇总 ===");
    if stats.is_empty() {
        println!("(无有效组：需 ≥2 帧且 RGB)");
        return Ok(());
    }
    let sum = |f: fn(&GroupStat) -> u64| stats.iter().map(f).sum::<u64>();
    let (ta, tb, tc) = (sum(|s| s.sad_a), sum(|s| s.sad_b), sum(|s| s.sad_c));
    let (ba, bb, bh, bc) = (
        sum(|s| s.bytes_a),
        sum(|s| s.bytes_b),
        sum(|s| s.bytes_h),
        sum(|s| s.bytes_c),
    );
    let pct = |c: u64, b: u64| -> f64 {
        if b == 0 {
            0.0
        } else {
            (c as f64 - b as f64) / b as f64 * 100.0
        }
    };
    println!(
        "组数 {}，帧数合计 {}",
        stats.len(),
        stats.iter().map(|s| s.frames).sum::<usize>()
    );
    println!(
        "SAD 合计: A={ta} B={tb} C={tc}  (C vs A {:+.2}%, C vs B {:+.2}%)",
        pct(tc, ta),
        pct(tc, tb)
    );
    println!(
        "字节合计: A={ba} B={bb} H={bh} C={bc}  (C vs A {:+.2}%, C vs H {:+.2}%)",
        pct(bc, ba),
        pct(bc, bh)
    );
    let max_depth = stats.iter().map(|s| s.max_depth).max().unwrap_or(0);
    println!(
        "MST 最大深度: {max_depth}（随机访问需解码 {} 帧链）",
        max_depth
    );

    // 逐组收益排序（展示哪些组受益/退化）
    let mut ranked: Vec<(&str, f64)> = stats
        .iter()
        .map(|s| (s.name.as_str(), pct(s.bytes_c, s.bytes_h)))
        .collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n逐组 C vs H 字节收益（负 = MST 更优）：");
    for (name, p) in &ranked {
        println!("  {:<12} {:+.2}%", name, p);
    }

    // 判定门槛（项目统一 ≥3%）
    let gain = pct(bc, bh);
    println!(
        "\n判定：C 相对 H（Hybrid 三候选取最小）字节变化 {:+.2}% —— {}",
        gain,
        if gain <= -3.0 {
            "达到 ≥3% 门槛，可进入实施评估"
        } else {
            "未达 ≥3% 门槛（记录为已探针证伪/关闭）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prim_mst_builds_tree_and_depth() {
        // 4 节点，节点 0 为根；边权设计为 0-1, 1-2, 2-3 链最优。
        let cost = vec![
            vec![0, 1, 9, 9],
            vec![1, 0, 1, 9],
            vec![9, 1, 0, 1],
            vec![9, 9, 1, 0],
        ];
        let (parent, order, total) = prim_mst(&cost, 0);
        assert_eq!(total, 3); // 1 + 1 + 1
        assert_eq!(parent[0], usize::MAX);
        assert_eq!(parent[1], 0);
        assert_eq!(parent[2], 1);
        assert_eq!(parent[3], 2);
        let depth = tree_depths(&parent, &order, 0);
        assert_eq!(depth[0], 0);
        assert_eq!(depth[3], 3);
    }

    #[test]
    fn sampled_sad_matches_full_when_stride_one() {
        let a = vec![1, 2, 3, 4, 5];
        let b = vec![1, 1, 1, 1, 1];
        assert_eq!(sampled_sad(&a, &b, 1), 0 + 1 + 2 + 3 + 4);
        assert_eq!(sampled_sad(&a, &b, 2), 0 + 2 + 4);
    }
}
