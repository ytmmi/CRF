//! LIC（局部照明补偿）收益探针：整帧乘加模型可行性
//!
//! 压缩算法探索文档 §9 建议 6（LIC，采纳探索中高优先级）：闪光/阴影/光照
//! 渐变 = 整帧亮度乘加——纯像素差分残差骤增直击此症。H.264 Weighted
//! Prediction 实测 fade 场景最高省 67%。本探针在真实差分帧上量化模型
//! `fit ≈ a·golden + b`（定点 a∈[0.80,1.20]、b∈[-64,64]）的残差能量下降，
//! 为「是否实现帧级全局 a,b 信令」提供数据依据。零外部数据集，仅扫 test/png。

use crate::crf::core::domain::ImageData;
use crate::crf::performance::bench::load_frames;

/// 定点扫描范围:a = A_NUM/100 (0.80..1.20), b 整数
const A_NUM_MIN: i32 = 80;
const A_NUM_MAX: i32 = 120;
const B_MIN: i32 = -64;
const B_MAX: i32 = 64;

/// 拟合参数: `fit(x) = (a_num * x) / 100 + b`（向零截断整数语义）
fn fit_predict(x: i32, a_num: i32, b: i32) -> i32 {
    x.saturating_mul(a_num) / 100 + b
}

/// 在给定抽样步长下计算 (a_num,b) 的 SAD。
fn sad_for(golden: &[i32], frame: &[i32], step: usize, a_num: i32, b: i32) -> u64 {
    let mut sad = 0u64;
    let mut i = 0usize;
    while i < golden.len() {
        let fit = fit_predict(golden[i], a_num, b);
        sad = sad.saturating_add(frame[i].saturating_sub(fit).unsigned_abs() as u64);
        i += step;
    }
    sad
}

/// 粗–精两遍扫描:粗步长找邻域,精步长修最优。
/// 返回 (最优 a_num, 最优 b, 采样基线 SAD, 采样 LIC SAD)。
fn fit_lic(golden: &[i32], frame: &[i32]) -> Option<(i32, i32, u64, u64)> {
    use std::cmp::Ordering;

    if golden.is_empty() || golden.len() != frame.len() {
        return None;
    }

    // 第一遍:粗步长 32,覆盖全扫描面;记录最优。
    const COARSE: usize = 32;
    let mut best_a = A_NUM_MIN;
    let mut best_b = B_MIN;
    let mut best_sad = u64::MAX;
    for a_num in (A_NUM_MIN..=A_NUM_MAX).step_by(4) {
        for b in (B_MIN..=B_MAX).step_by(16) {
            let sad = sad_for(golden, frame, COARSE, a_num, b);
            match sad.cmp(&best_sad) {
                Ordering::Less => {
                    best_sad = sad;
                    best_a = a_num;
                    best_b = b;
                }
                Ordering::Equal => {
                    // tie-break: 优先更接近 (100, 0) —— 模型退化到纯差分时无信令
                    let cur = (best_a - 100).abs() + best_b.abs() * 2;
                    let cand = (a_num - 100).abs() + b.abs() * 2;
                    if cand < cur {
                        best_a = a_num;
                        best_b = b;
                    }
                }
                Ordering::Greater => {}
            }
        }
    }

    // 第二遍:在粗最优邻域精扫 a±4 步长 1、b±16 步长 4。
    let a_lo = (best_a - 4).max(A_NUM_MIN);
    let a_hi = (best_a + 4).min(A_NUM_MAX);
    let b_lo = (best_b - 16).max(B_MIN);
    let b_hi = (best_b + 16).min(B_MAX);
    for a_num in (a_lo..=a_hi).step_by(1) {
        for b in (b_lo..=b_hi).step_by(4) {
            let sad = sad_for(golden, frame, COARSE, a_num, b);
            if sad < best_sad {
                best_sad = sad;
                best_a = a_num;
                best_b = b;
            }
        }
    }

    // 最终度量:步长 8 的基线 SAD 与 LIC SAD
    let base = {
        let mut sad = 0u64;
        let mut i = 0usize;
        while i < golden.len() {
            sad = sad.saturating_add(frame[i].saturating_sub(golden[i]).unsigned_abs() as u64);
            i += 8;
        }
        sad
    };
    let lic = sad_for(golden, frame, 8, best_a, best_b);
    Some((best_a, best_b, base, lic))
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

    println!("=== LIC（整帧乘加照明补偿）收益探针 ===");
    println!("root: {root}\n");
    println!(
        "模型: fit = (a·golden)/100 + b,  a∈[{:.2},{:.2}], b∈[{B_MIN},{B_MAX}]",
        A_NUM_MIN as f64 / 100.0,
        A_NUM_MAX as f64 / 100.0
    );
    println!(
        "{:<14} {:>10} {:>10} {:>8} {:>12} {:>6}",
        "组", "基线SAD", "LIC SAD", "下降%", "a×100", "b"
    );

    let mut total_base = 0u64;
    let mut total_lic = 0u64;
    let mut frames_seen = 0usize;
    let mut frames_improved = 0usize;

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // golden = 首帧原始像素(RGB 域,与路径 C 差分基一致;路径 G 在控制域
        // 差分但 LIC 作为参考域加权候选独立评估,方向收益同量级)。
        let golden = &frames[0].pixels;
        for frame in frames.iter().skip(1).take(2) {
            if frame.pixels.len() != golden.len() {
                continue;
            }
            frames_seen += 1;
            if let Some((a_num, b, base, lic)) = fit_lic(golden, &frame.pixels) {
                total_base += base;
                total_lic += lic;
                if lic < base {
                    frames_improved += 1;
                }
                let pct = if base > 0 {
                    (base - lic) as f64 / base as f64 * 100.0
                } else {
                    0.0
                };
                println!("{name:<14} {base:>10} {lic:>10} {pct:>7.1}% {a_num:>12} {b:>6}");
            }
        }
    }

    println!("\n--- 汇总 ---");
    if frames_seen == 0 {
        println!("(无可用差分帧)");
        return Ok(());
    }
    let total_pct = if total_base > 0 {
        (total_base - total_lic) as f64 / total_base as f64 * 100.0
    } else {
        0.0
    };
    println!("帧数: {frames_seen}  改善帧数: {frames_improved}");
    println!("总基线 SAD: {total_base}  总 LIC SAD: {total_lic}  总体下降: {total_pct:.1}%");
    if total_pct >= 3.0 {
        println!("判定: LIC 收益 ≥3% 门槛 → 值得实现帧级全局 a,b 信令(2 字节定点,破坏式更新许可)");
    } else {
        println!("判定: LIC 收益 <3% 门槛 → 数据不足以支撑格式变更,维持关闭或转单帧探针");
    }
    Ok(())
}

#[allow(dead_code)]
fn _image_type_anchor(_: &ImageData) {}
