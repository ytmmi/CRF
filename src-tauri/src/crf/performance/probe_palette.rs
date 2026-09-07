//! palette 色数分布探针：现有 palette(frame_type=4) 按「分量级唯一值 ≤256」
//! 判定可行性，但真实插画 RGB 的像素级(三元组)色数通常数千。本探针区分
//! 两尺度统计，并测量差分帧(RCT 残差域)的 palette 可行性——调色板真正
//! 参与竞争的语义位置，为 delta palette(§8.4 候选)/ palette 剪枝提供
//! 数据驱动依据。零外部数据集，仅扫 test/png。

use std::collections::HashSet;

use crate::crf::core::color::rct;
use crate::crf::performance::bench::load_frames;

/// 分量级唯一值数（当前 palette 的判定尺度：交织数组整体去重）。
fn component_unique(pixels: &[i32]) -> usize {
    let mut set: HashSet<i32> = HashSet::with_capacity(4096);
    set.extend(pixels.iter().copied());
    set.len()
}

/// 像素级唯一值数（RGB 三元组去重；delta palette 的语义尺度）。
fn pixel_unique(pixels: &[i32], components: usize) -> usize {
    let mut set = HashSet::with_capacity(4096);
    for chunk in pixels.chunks(components) {
        // 编码为 u64 键：按分量级宽 x 编码为固定哈希（简单拼接即可，
        // 只用于计数不需要抗碰撞）
        let mut key = 0u64;
        for &v in chunk.iter().take(4) {
            key = key.wrapping_mul(0x1_0000_01B3).wrapping_add(v as u64 & 0xFFFF);
        }
        set.insert(key);
    }
    set.len()
}

/// 抽样通过率（palette_plausible 的 step_by(7) 语义）。
fn plausible_by_sample(pixels: &[i32], max_colors: usize) -> bool {
    let mut set: HashSet<i32> = HashSet::with_capacity(max_colors + 32);
    for v in pixels.iter().step_by(7) {
        if set.insert(*v) && set.len() > max_colors {
            return false;
        }
    }
    true
}

/// 运行探针。`root` 为 test/png 根目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    const PALETTE_MAX: usize = 256;

    println!("=== palette 色数分布探针 ===");
    println!("root: {root}\n");

    let mut frame_stats_first = Vec::new();
    let mut frame_stats_diff = Vec::new();
    let mut diff_plausible = 0usize;
    let mut diff_total = 0usize;

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let rgb = frames[0].color_format.component_count() == 3;
        let components = frames[0].color_format.component_count();

        // 首帧（原始像素域）
        let f0 = &frames[0];
        let cu = component_unique(&f0.pixels);
        let pu = pixel_unique(&f0.pixels, components);
        frame_stats_first.push((name.clone(), cu, pu));

        // 差分帧：RCT 域残差（与路径 G 差分帧语义一致），取前 2 帧
        for frame in frames.iter().skip(1).take(2) {
            let diff = if rgb {
                rct::rct_forward(&frame.pixels, 3)
                    .map_err(|e| e.to_string())
                    .ok()
            } else {
                Some(frame.pixels.clone())
            };
            if let Some(diff_px) = diff {
                let cu_d = component_unique(&diff_px);
                let pu_d = pixel_unique(&diff_px, components);
                frame_stats_diff.push((name.clone(), cu_d, pu_d));
                diff_total += 1;
                if plausible_by_sample(&diff_px, PALETTE_MAX) {
                    diff_plausible += 1;
                }
            }
        }
    }

    println!("--- 首帧（原始像素域）---");
    println!(
        "{:<14} {:>10} {:>10}",
        "组", "分量级唯一", "像素级唯一"
    );
    for (name, cu, pu) in &frame_stats_first {
        println!("{name:<14} {cu:>10} {pu:>10}");
    }

    println!("\n--- 差分帧（RCT 残差域，前 2 帧）---");
    println!(
        "{:<14} {:>10} {:>10} {:>8}",
        "组", "分量级唯一", "像素级唯一", "抽样≤256"
    );
    for (name, cu, pu) in &frame_stats_diff {
        println!("{name:<14} {cu:>10} {pu:>10} {:>8}", *cu <= PALETTE_MAX);
    }

    println!("\n--- 汇总 ---");
    let first_comp_max = frame_stats_first
        .iter()
        .map(|(_, cu, _)| *cu)
        .max()
        .unwrap_or(0);
    let first_pix_max = frame_stats_first
        .iter()
        .map(|(_, _, pu)| *pu)
        .max()
        .unwrap_or(0);
    println!("首帧分量级唯一值范围: 1..{first_comp_max}");
    println!("首帧像素级唯一值范围: 1..{first_pix_max}");
    println!(
        "差分帧分量级 ≤256（当前 palette 可行）: {}/{diff_total} ({:.1}%)",
        frame_stats_diff
            .iter()
            .filter(|(_, cu, _)| *cu <= PALETTE_MAX)
            .count(),
        frame_stats_diff
            .iter()
            .filter(|(_, cu, _)| *cu <= PALETTE_MAX)
            .count() as f64
            / diff_total.max(1) as f64
            * 100.0
    );
    println!(
        "差分帧抽样(step_by 7)通过 palette_plausible: {diff_plausible}/{diff_total} ({:.1}%)",
        diff_plausible as f64 / diff_total.max(1) as f64 * 100.0
    );
    Ok(())
}