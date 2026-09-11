//! MA 树叶数分布探针：直方图共享(§8.2)可行性前置验证
//!
//! JPEG-XL 的 MA 树「叶节点直方图共享」——邻近叶复用概率分布，减少过拟合。
//! CRF 当前 MA 树（深度 3、最多 8 叶）每叶独立概率槽位。若真实数据上树
//! 常退化到少量叶（1~2），则共享收益 ≈0、无实施价值；若叶数常接近 8 且
//! 各叶样本分布重叠，共享才有压缩率收益窗口。本探针统计各帧（首帧+差分帧）
//! MA 树的叶数、各叶样本占比，为「是否实施直方图共享」提供数据依据。

use std::collections::HashMap;

use crate::crf::core::color::rct;
use crate::crf::core::entropy::context::{build_ma_tree, MaTree};
use crate::crf::performance::bench::load_frames;

/// 走树统计各叶命中样本数
fn leaf_histogram(tree: &MaTree, pixels: &[i32], stride: usize) -> Vec<usize> {
    let mut hist: HashMap<usize, usize> = HashMap::new();
    for i in 0..pixels.len() {
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
        let leaf = tree.walk(l, t, tl, tr);
        *hist.entry(leaf).or_insert(0) += 1;
    }
    let max_leaf = tree.leaf_count();
    let mut out = vec![0usize; max_leaf.max(1)];
    for (leaf, count) in hist {
        if leaf < out.len() {
            out[leaf] = count;
        }
    }
    out
}

/// 运行探针。`root` 为 test/png 根目录（遍历全部子组）。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== MA 树叶数分布探针（直方图共享可行性）===");
    println!("root: {root}\n");

    let mut leaf_count_hist: HashMap<usize, usize> = HashMap::new();
    let mut total_frames = 0usize;
    let mut worst_skew: (f64, usize) = (0.0, 0); // (最大单叶占比, 叶数)
    let mut min_leaf_share = f64::MAX;
    let mut min_leaf_share_ctx = (0usize, String::new());

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
        let width = frames[0].width as usize;

        // 首帧（原始像素域）
        for (fi, frame) in frames.iter().take(2).enumerate() {
            let pixels: &[i32] = if fi == 0 {
                &frame.pixels
            } else if components == 3 {
                // 差分帧：RCT 残差域（与路径 G 一致）
                let mut diff_rgb = vec![0i32; frame.pixels.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, &frames[0].pixels, &mut diff_rgb);
                let diff = rct::rct_forward(&diff_rgb, components)
                    .map_err(|e| e.to_string())
                    .ok();
                match diff {
                    Some(d) => {
                        let leaked = Box::leak(d.into_boxed_slice());
                        leaked
                    }
                    None => continue,
                }
            } else {
                continue;
            };

            let stride = width * components;
            let tree = build_ma_tree(pixels, Some(stride)).map_err(|e| e.to_string())?;
            let hist = leaf_histogram(&tree, pixels, stride);
            let n_leaves = tree.leaf_count();
            *leaf_count_hist.entry(n_leaves).or_insert(0) += 1;
            total_frames += 1;

            // 最大单叶占比
            let total_samples: usize = hist.iter().sum();
            if total_samples > 0 {
                let max_share =
                    hist.iter().max().copied().unwrap_or(0) as f64 / total_samples as f64;
                if max_share > worst_skew.0 {
                    worst_skew = (max_share, n_leaves);
                }
                // 最小非零叶占比（叶使用是否均匀）
                let nonzero: Vec<usize> = hist.iter().copied().filter(|&c| c > 0).collect();
                if let Some(&min_c) = nonzero.iter().min() {
                    let share = min_c as f64 / total_samples as f64;
                    if share < min_leaf_share {
                        min_leaf_share = share;
                        min_leaf_share_ctx = (n_leaves, name.clone());
                    }
                }
            }
        }
    }

    println!("--- 叶数分布（{total_frames} 帧）---");
    let mut keys: Vec<usize> = leaf_count_hist.keys().copied().collect();
    keys.sort_unstable();
    for &k in &keys {
        println!(
            "  叶数 {k}: {:>3} 帧 ({:>5.1}%)",
            leaf_count_hist[&k],
            leaf_count_hist[&k] as f64 / total_frames.max(1) as f64 * 100.0
        );
    }

    println!("\n--- 叶使用特征 ---");
    println!(
        "最大单叶占比: {:.1}%（叶数 {}）",
        worst_skew.0 * 100.0,
        worst_skew.1
    );
    println!(
        "最小非零叶占比: {:.2}%（叶数 {}, 组 {}）",
        min_leaf_share * 100.0,
        min_leaf_share_ctx.0,
        min_leaf_share_ctx.1
    );

    // 裁决逻辑：叶数分布 + 使用偏斜
    let avg_leaves: f64 = keys
        .iter()
        .map(|&k| k as f64 * leaf_count_hist[&k] as f64)
        .sum::<f64>()
        / total_frames.max(1) as f64;
    println!("\n平均叶数: {avg_leaves:.1}");
    if avg_leaves < 4.0 {
        println!("判定: 平均叶数 <4，直方图共享收益窗口小——维持独立概率槽位");
    } else if worst_skew.0 > 0.8 {
        println!(
            "判定: 存在极端单叶主导（>{:.0}%），共享需谨慎避免稀释主导叶",
            worst_skew.0 * 100.0
        );
    } else {
        println!("判定: 叶数充足且分布不过度偏斜——直方图共享有实施价值，需进一步对比压缩率");
    }
    Ok(())
}
