//! LIC（局部照明补偿）核心数学：帧级乘加加权参考
//!
//! 模型：`fit(x) = (a·x)/100 + b`（定点 a = a_num/100，a_num∈[80,120]；
//! b∈[-64,64]）。编码端在 golden 差分路径上附加 LIC 竞争候选——以
//! `LIC(golden)` 替代 `golden` 作差分参考，压缩闪光/阴影/光照渐变等
//! 整帧亮度乘加场景的残差（压缩算法探索 §9 建议 6；optimization-review
//! §40 探针闭环后转入正式实现）。
//!
//! 本模块为**纯数学**（解码端与编码端共用），不含任何码流/会话依赖：
//! - 搜索：粗-精两遍定点扫描（与 `performance/probe_lic.rs` 同口径）；
//! - 应用：向零截断整数语义，保证编解码两侧逐位一致。

/// 乘数定点下界（a = 0.80）
pub const LIC_A_NUM_MIN: i32 = 80;
/// 乘数定点上界（a = 1.20）
pub const LIC_A_NUM_MAX: i32 = 120;
/// 偏移下界
pub const LIC_B_MIN: i32 = -64;
/// 偏移上界
pub const LIC_B_MAX: i32 = 64;
/// 预筛收益门槛：采样 LIC SAD 至少低于基线 SAD 该比例（‰）才进入完整编码
/// 竞争——避免无收益帧在编码端浪费一次完整管线（单调不劣化仍由字节竞争兜底）。
pub const LIC_MIN_GAIN_X1000: u64 = 1;

/// LIC 拟合结果（采样域 SAD）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LicFit {
    /// 乘数定点：a = a_num / 100（[`LIC_A_NUM_MIN`]..=[`LIC_A_NUM_MAX`]）
    pub a_num: i32,
    /// 偏移 b（[`LIC_B_MIN`]..=[`LIC_B_MAX`]）
    pub b: i32,
    /// 基线采样 SAD（frame − golden）
    pub base_sad: u64,
    /// LIC 采样 SAD（frame − LIC(golden)）
    pub lic_sad: u64,
}

impl LicFit {
    /// 预筛：仅当 LIC 采样 SAD 显著低于基线时才值得进入完整编码竞争。
    pub fn worthwhile(&self) -> bool {
        if self.lic_sad >= self.base_sad {
            return false;
        }
        let drop = self.base_sad - self.lic_sad;
        drop.saturating_mul(1000) >= self.base_sad.saturating_mul(LIC_MIN_GAIN_X1000)
    }

    /// 采样域下降百分比（基线−LIC）/基线，用于统计。
    pub fn drop_percent(&self) -> f64 {
        if self.base_sad == 0 {
            0.0
        } else {
            (self.base_sad - self.lic_sad) as f64 / self.base_sad as f64 * 100.0
        }
    }
}

/// 定点预测：`fit(x) = (a_num·x)/100 + b`（向零截断整数语义，与探针一致）。
#[inline]
pub fn fit_predict(x: i32, a_num: i32, b: i32) -> i32 {
    x.saturating_mul(a_num) / 100 + b
}

/// 将加权参考写入 `out`：`out[i] = fit(golden[i])`。原地安全。
pub fn fit_into(golden: &[i32], a_num: i32, b: i32, out: &mut [i32]) {
    debug_assert_eq!(golden.len(), out.len());
    for (g, o) in golden.iter().zip(out.iter_mut()) {
        *o = fit_predict(*g, a_num, b);
    }
}

/// 解码端/重建链包装：帧头 u8 存储语义（lic_b 为 i8）。
pub fn apply_lic_weighted(src: &[i32], a_num: u8, b: u8) -> Vec<i32> {
    let mut out = vec![0i32; src.len()];
    fit_into(src, a_num as i32, b as i8 as i32, &mut out);
    out
}

/// 在给定采样步长下计算 (a_num, b) 的 SAD（与探针 `sad_for` 同口径）。
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

/// 基线 SAD（frame − golden，同采样步长）。
fn base_sad_for(golden: &[i32], frame: &[i32], step: usize) -> u64 {
    let mut sad = 0u64;
    let mut i = 0usize;
    while i < golden.len() {
        sad = sad.saturating_add(frame[i].saturating_sub(golden[i]).unsigned_abs() as u64);
        i += step;
    }
    sad
}

/// 粗-精两遍定点扫描，求最优 (a_num, b) 与采样 SAD。
///
/// 与探针一致：粗扫 step=32（a 步长 4、b 步长 16）→ 精修邻域（a±4 步长 1、
/// b±16 步长 4）→ 最终度量 step=8。tie-break 优先靠近恒等 (100, 0)。
pub fn search_lic(golden: &[i32], frame: &[i32]) -> Option<LicFit> {
    use std::cmp::Ordering;

    if golden.is_empty() || golden.len() != frame.len() {
        return None;
    }

    // 第一遍：粗步长 32，覆盖全扫描面。
    const COARSE: usize = 32;
    let mut best_a = LIC_A_NUM_MIN;
    let mut best_b = LIC_B_MIN;
    let mut best_sad = u64::MAX;
    for a_num in (LIC_A_NUM_MIN..=LIC_A_NUM_MAX).step_by(4) {
        for b in (LIC_B_MIN..=LIC_B_MAX).step_by(16) {
            let sad = sad_for(golden, frame, COARSE, a_num, b);
            match sad.cmp(&best_sad) {
                Ordering::Less => {
                    best_sad = sad;
                    best_a = a_num;
                    best_b = b;
                }
                Ordering::Equal => {
                    // tie-break：优先更接近 (100, 0)——模型退化到纯差分时无信令
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

    // 第二遍：在粗最优邻域精扫 a±4 步长 1、b±16 步长 4。
    let a_lo = (best_a - 4).max(LIC_A_NUM_MIN);
    let a_hi = (best_a + 4).min(LIC_A_NUM_MAX);
    let b_lo = (best_b - 16).max(LIC_B_MIN);
    let b_hi = (best_b + 16).min(LIC_B_MAX);
    for a_num in a_lo..=a_hi {
        for b in (b_lo..=b_hi).step_by(4) {
            let sad = sad_for(golden, frame, COARSE, a_num, b);
            if sad < best_sad {
                best_sad = sad;
                best_a = a_num;
                best_b = b;
            }
        }
    }

    // 最终度量：步长 8 的基线 SAD 与 LIC SAD。
    let base = base_sad_for(golden, frame, 8);
    let lic = sad_for(golden, frame, 8, best_a, best_b);
    Some(LicFit {
        a_num: best_a,
        b: best_b,
        base_sad: base,
        lic_sad: lic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fit_predict_truncation() {
        // 向零截断整数语义：(a·x)/100
        assert_eq!(fit_predict(100, 100, 0), 100);
        assert_eq!(fit_predict(100, 80, 0), 80);
        assert_eq!(fit_predict(50, 120, 0), 60);
        assert_eq!(fit_predict(33, 120, 0), 39); // 39.6 → 39
        assert_eq!(fit_predict(100, 100, 5), 105);
        assert_eq!(fit_predict(100, 100, -5), 95);
        // 负 x：saturating_mul 防溢出，/100 向零截断
        assert_eq!(fit_predict(-100, 120, 0), -120);
        assert_eq!(fit_predict(-50, 80, 0), -40);
        // 饱和保护（i32 极值不溢出）
        assert_eq!(fit_predict(i32::MAX, 120, 0), i32::MAX.saturating_mul(120) / 100);
    }

    #[test]
    fn test_fit_into_matches_apply_lic() {
        let golden = vec![10, 20, 30, 40, 50, -10, 200, 255];
        let mut out = vec![0; golden.len()];
        fit_into(&golden, 96, 4, &mut out);
        let expected: Vec<i32> = golden.iter().map(|&x| fit_predict(x, 96, 4)).collect();
        assert_eq!(out, expected);
        // apply_lic_weighted（u8 存储语义）与 fit_into 一致
        let applied = apply_lic_weighted(&golden, 96, 4);
        assert_eq!(applied, expected);
        // 负 b 的 u8 存储往返
        let neg = apply_lic_weighted(&golden, 96, (-4i8) as u8);
        let expected_neg: Vec<i32> = golden.iter().map(|&x| fit_predict(x, 96, -4)).collect();
        assert_eq!(neg, expected_neg);
    }

    #[test]
    fn test_search_identity_when_no_benefit() {
        // 纯差分无乘加收益：最优应接近 (100, 0)
        let golden: Vec<i32> = (0..4096).map(|i| (i % 256) as i32).collect();
        // frame = golden + 少量随机扰动（无系统性乘加偏移）
        let frame: Vec<i32> = golden
            .iter()
            .enumerate()
            .map(|(i, &g)| g + (((i as i32) * 7) % 9) - 4)
            .collect();
        let fit = search_lic(&golden, &frame).expect("scan must find a fit");
        assert!((fit.a_num - 100).abs() <= 4, "a_num={}", fit.a_num);
        assert!(fit.b.abs() <= 16, "b={}", fit.b);
        // 无系统性偏移时 LIC 不会显著优于基线
        assert!(!fit.worthwhile() || fit.drop_percent() < 5.0);
    }

    #[test]
    fn test_search_detects_multiplicative_shading() {
        // 构造光照渐变：frame = (0.92·golden) + 3（整帧乘加，与模型同形）
        let golden: Vec<i32> = (0..8192).map(|i| (i % 200 + 20) as i32).collect();
        let frame: Vec<i32> = golden.iter().map(|&g| (g * 92) / 100 + 3).collect();
        let fit = search_lic(&golden, &frame).expect("scan must find a fit");
        assert!(fit.worthwhile(), "shading must be worthwhile: {fit:?}");
        // 搜索应恢复出接近 (92, 3) 的参数（容差：粗精网格 + 采样噪声）
        assert!((fit.a_num - 92).abs() <= 2, "a_num={}", fit.a_num);
        assert!((fit.b - 3).abs() <= 8, "b={}", fit.b);
        // 残差应大幅下降
        assert!(fit.drop_percent() > 50.0, "drop={}%", fit.drop_percent());
    }

    #[test]
    fn test_search_mismatched_length_returns_none() {
        assert!(search_lic(&[1, 2, 3], &[1, 2]).is_none());
        assert!(search_lic(&[], &[]).is_none());
    }

    #[test]
    fn test_worthwhile_threshold() {
        let big = LicFit {
            a_num: 90,
            b: 0,
            base_sad: 1_000_000,
            lic_sad: 900_000,
        };
        assert!(big.worthwhile());
        // 低于 0.1% 门槛 → 不预筛通过（避免边界抖动）
        let tiny = LicFit {
            a_num: 90,
            b: 0,
            base_sad: 1_000_000,
            lic_sad: 999_900,
        };
        assert!(!tiny.worthwhile());
        // LIC 更差 → 不预筛通过
        let worse = LicFit {
            a_num: 90,
            b: 0,
            base_sad: 1_000_000,
            lic_sad: 1_000_100,
        };
        assert!(!worse.worthwhile());
    }
}
