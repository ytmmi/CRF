//! JPEG 源噪声感知（noise-aware lossy pipeline）
//!
//! 有损源（JPEG/WebP）差分残差的失真分量形态因差分类型而异：
//!
//! - **零中心稠密噪声**（同图不同压缩版本相减、重编码失真差）：
//!   残差围绕 0 对称分布——视觉不重要，可安全归零换取 RLE 行程；
//! - **时间差分**（gal 场景的光线/色调随时间变化）：残差场中心在
//!   Δ≠0 处，属于系统性偏移而非噪声——零中心滤波会削平分布单侧、
//!   破坏空间预测器的均匀性假设，反而膨胀码流（实测 +17%）；
//! - **天气效果**（雨/雪/云颗粒）：零中心高频分量与真实内容混合，
//!   温和阈值仅削弱弱颗粒，强雨线雪点保留；
//! - **服装/表情差分**（2000/c 组）：大面积精确静止 + 局部大幅结构
//!   变化——幅度分布两极化，任何自动滤波都不应触碰。
//!
//! 本模块实现"噪声归一化"：按分量统计残差的带符号中位数 `med` 与
//! 幅度 P25 低分位 `amp25`，施加**零中心性门控**：
//!
//! - 门控通过（|med| ≤ max(amp25, 2)，场围绕 0 对称）：
//!   T = clamp(τ·amp25, 0, CAP)，软阈值归零噪声主导的小残差；
//! - 门控不通过（系统性偏移 / 两极化结构场）：T = 0，零介入，
//!   交还给闭环量化管线处理。
//!
//! 作用点位于 golden 差分之后 / RCT 变换之前；仅作用于有损模式的
//! 差分帧（golden 首帧与无损模式绝不触碰）；解码端无需感知，格式零改动。

use crate::crf::core::bitstream::constants::BAND_HEIGHT;

/// 自动阈值的绝对上限：超过它的残差属于明确的结构信号，
/// 不允许被任何自动策略抹除。
const AMP_CAP: i32 = 24;

/// 闭环自适应步长的绝对上限（量化步长语义，对应 q≈20 档位）：
/// 死区跟随局部失真水平，但不允许劣化到比 q20 更粗。
const Q_BAND_CAP: u8 = 16;

/// 由差分帧推导**逐条带自适应量化步长表**（闭环噪声归一化的核心）
///
/// 对每条带统计三分量合并的 |v| 直方图，取 P25 分位 `amp25` 作为该
/// 条带的失真水平度量，有效步长：
///
/// ```text
/// Q_eff(band) = clamp(round(amp25 × τ/100), base_step, Q_BAND_CAP)
/// ```
///
/// - 失真稠密的条带（天气颗粒/色调漂移）：amp25 大 → 死区加宽，
///   小幅失真直接落入死区归零；
/// - 静止为主的条带（结构差分场景）：精确零占比高 → amp25 = 0 →
///   保持基础步长的精细度（两极化天然保护）。
///
/// 空间预测在闭环内自动完成"中心化"（常数漂移被邻居预测抵消），
/// 因此无需零中心性判定——这正是闭环方案优于前置软阈值之处。
pub fn estimate_band_quant_steps(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    base_step: u8,
    tau_x100: u16,
) -> Vec<u8> {
    let band_h = BAND_HEIGHT;
    let bands = height.div_ceil(band_h).max(1);
    let mut steps = vec![base_step; bands];
    if components == 0 || pixels.is_empty() || width == 0 {
        return steps;
    }

    // 按条带统计三分量合并的 |v| 直方图（值域 0..=255）
    let mut hists: Vec<[u32; 256]> = (0..bands).map(|_| [0u32; 256]).collect();
    let mut counts = vec![0u32; bands];
    let stride = width * components;
    for (y, row) in pixels.chunks_exact(stride).enumerate() {
        let band = (y / band_h).min(bands - 1);
        for &v in row {
            hists[band][v.unsigned_abs().min(255) as usize] += 1;
            counts[band] += 1;
        }
    }

    for (band, hist) in hists.iter().enumerate() {
        if counts[band] == 0 {
            continue;
        }
        // P25 低分位：四分之一最小幅像素的上界
        let quarter = counts[band] / 4;
        let mut acc = 0u32;
        let mut p25 = 0usize;
        for (bi, &cnt) in hist.iter().enumerate() {
            acc += cnt;
            if acc > quarter {
                p25 = bi;
                break;
            }
        }
        let q_eff =
            ((p25 as u32 * tau_x100 as u32) / 100).clamp(base_step as u32, Q_BAND_CAP as u32) as u8;
        steps[band] = q_eff.max(1);
    }
    steps
}

/// 质量优先的减步长映射：`protection ≤ 100` 保持基础步长；
/// `> 100` 时每 25 减 1 步长（下限 1）。
#[inline]
fn reduce_step(base_step: u8, protection_x100: u16) -> u8 {
    if protection_x100 <= 100 {
        return base_step;
    }
    let delta = (((protection_x100 - 100) as u64) / 25).max(1);
    let floor = (base_step as u64).saturating_sub(1);
    base_step.saturating_sub(delta.min(floor) as u8).max(1)
}

/// 由差分帧推导**逐条带 activity 自适应量化步长表**（P4.2/P4.3/P4.4 感知量化）
///
/// 与 [`estimate_band_quant_steps`]（amp25 幅度感知，一阶统计）互补：
/// 本函数基于**空间梯度能量**（二阶统计）做三分分类——
/// - **纹理**（密集中等梯度）：量化噪声被纹理掩盖 → 加宽死区省码率；
/// - **边缘**（稀疏大梯度，如线稿/锐边）：量化噪声以 ringing/断裂形式可见 → 收窄死区保护；
/// - **平坦**（低梯度渐变）：量化噪声以 banding 形式可见 → 收窄死区防 banding。
///
/// 度量：每行 Y 分量水平梯度 `|Y[x] − Y[x−1]|` 的条带均值 avg 与
/// 非零平均梯度 nz_avg = Σ|∇| / #{|∇|>0}（区分「稀疏大梯度边缘」与
/// 「密集中等梯度纹理」——二者 avg 可能接近，但边缘的 nz_avg 远高）。
/// 中性参考 = 全帧 avg 均值。
///
/// ```text
/// 边缘（nz_avg > 2·ref）: Q = reduce(edge_protection)             # 减步长
/// 纹理（avg > ref）      : Q = base_step + clamp(Δ·(activity−100)/100, 0, Q_BAND_CAP−base_step)
/// 平坦（avg < ref/2）    : Q = reduce(flat_area_protection)       # 减步长
/// 其余                   : Q = base_step
/// ```
///
/// 解码端无感（与 [`estimate_band_quant_steps`] 相同的自描述残差语义，格式零改动）。
/// 默认 100 为中性（保持 base_step）；增步长省码率、减步长属质量优先。
pub fn estimate_band_activity_steps(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    base_step: u8,
    activity_masking_x100: u16,
    flat_area_protection_x100: u16,
    edge_protection_x100: u16,
) -> Vec<u8> {
    let band_h = BAND_HEIGHT;
    let bands = height.div_ceil(band_h).max(1);
    let mut steps = vec![base_step; bands];
    if components == 0 || pixels.is_empty() || width < 2 {
        return steps;
    }

    let stride = width * components;
    let mut grad_sum = vec![0u64; bands];
    let mut grad_cnt = vec![0u64; bands];
    let mut non_zero_cnt = vec![0u64; bands];
    for (y, row) in pixels.chunks_exact(stride).enumerate() {
        let band = (y / band_h).min(bands - 1);
        // Y 分量水平梯度（相邻像素同分量差，wrapping_sub 避免 debug 溢出 panic）
        for x in 1..width {
            let g = row[x * components]
                .wrapping_sub(row[(x - 1) * components])
                .unsigned_abs() as u64;
            grad_sum[band] += g;
            grad_cnt[band] += 1;
            if g > 0 {
                non_zero_cnt[band] += 1;
            }
        }
    }
    let avg: Vec<u64> = (0..bands)
        .map(|b| if grad_cnt[b] > 0 { grad_sum[b] / grad_cnt[b] } else { 0 })
        .collect();
    let non_zero_avg: Vec<u64> = (0..bands)
        .map(|b| {
            if non_zero_cnt[b] > 0 {
                grad_sum[b] / non_zero_cnt[b]
            } else {
                0
            }
        })
        .collect();

    // 中性参考 = 全帧平均梯度（含零 band，稳健区分纹理/平坦）。
    //
    // §28 探针缺陷修复：整数除法 Σavg/bands 在稀疏差分场景（二次元
    // 表情/口型差分主形态——静止条带占绝大多数、变化条带 avg 仅 1~3）
    // 下恒退化为 0，触发 `reference == 0` 短路使三分分类整体失效，
    // P4.2/P4.3/P4.4 三旋钮从未真正激活。修复为 `.max(1)`：reference
    // 下限 1 保证稀疏差分下分类激活（nz_avg > 2 命中边缘、avg > 1
    // 命中纹理），密集变化场景 Σavg/bands ≥ 1 时行为与修复前完全一致；
    // 默认旋钮 100 中性下映射不改变任何产物，逐字节不变。
    let reference = (avg.iter().sum::<u64>() / bands as u64).max(1);

    for band in 0..bands {
        let g = avg[band];
        if reference == 0 {
            continue; // 全平坦，无 activity 信号
        }
        if non_zero_avg[band] > reference.saturating_mul(2) {
            // 边缘（稀疏大梯度）：收窄死区防 ringing/断裂（质量优先）
            steps[band] = reduce_step(base_step, edge_protection_x100);
        } else if g > reference && activity_masking_x100 > 100 {
            // 纹理：加宽死区省码率（上限 Q_BAND_CAP）
            let delta = (g - reference) * (activity_masking_x100 - 100) as u64 / 100;
            let cap = Q_BAND_CAP.saturating_sub(base_step) as u64;
            steps[band] = base_step.saturating_add(delta.min(cap) as u8);
        } else if g < reference / 2 {
            // 平坦：收窄死区防 banding（质量优先）
            steps[band] = reduce_step(base_step, flat_area_protection_x100);
        }
    }
    steps
}

/// 条带级残差幅度阈值估计（交织多分量，含零中心性门控）
///
/// 每分量统计带符号值直方图（值域 [-256,255]），推导：
/// - `med`：带符号中位数（场的中心位置）
/// - `amp25`：|v| 的 P25 低分位（小幅残差的典型水平）
///
/// 门控：仅当 |med| ≤ max(amp25, 2)（场围绕 0 对称）时启用软阈值；
/// 否则 T = 0 零介入。
///
/// 返回 thresholds[band][component] = clamp(τ·amp25, 0, AMP_CAP)。
pub fn estimate_interleaved_band_thresholds(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    tau_x100: u16,
) -> Vec<Vec<i32>> {
    let _ = width;
    let band_h = BAND_HEIGHT;
    let bands = height.div_ceil(band_h).max(1);
    debug_assert_eq!(pixels.len(), width * height * components);

    // 每分量独立的带符号直方图（索引 = v + 256，值域 [-256,255]）
    let mut hists: Vec<[u32; 512]> = (0..components).map(|_| [0u32; 512]).collect();
    let mut counts: Vec<u32> = vec![0; components];
    for chunk in pixels.chunks_exact(components) {
        for (c, &v) in chunk.iter().enumerate() {
            hists[c][(v + 256) as usize] += 1;
            counts[c] += 1;
        }
    }

    let mut thresholds = vec![vec![0i32; components]; bands];
    for c in 0..components {
        if counts[c] == 0 {
            continue;
        }
        let hist = &hists[c];
        let half = counts[c] / 2;

        // 带符号中位数：场的中心位置
        let mut acc = 0u32;
        let mut med_v = 0i32;
        for (bi, &cnt) in hist.iter().enumerate() {
            acc += cnt;
            if acc > half {
                med_v = bi as i32 - 256;
                break;
            }
        }

        // 幅度 P25：|v| 的低分位（小幅残差的典型水平）
        let quarter = counts[c] / 4;
        let mut acc2 = 0u32;
        let mut amp25 = 0usize;
        for (bi, &cnt) in hist.iter().enumerate() {
            acc2 += cnt;
            if acc2 > quarter {
                amp25 = (bi as i32 - 256).unsigned_abs() as usize;
                break;
            }
        }

        // 零中心性门控：中位数偏移不超过小幅度典型水平才视为对称场
        let zero_centered = med_v.unsigned_abs() as usize <= amp25.max(2);
        let t = if zero_centered {
            ((amp25 as u32 * tau_x100 as u32) / 100) as i32
        } else {
            0 // 系统性偏移场：交还闭环量化管线
        };
        for band in thresholds.iter_mut().take(bands) {
            band[c] = t.clamp(0, AMP_CAP);
        }
    }
    thresholds
}

/// 对交织差分帧执行条带级软阈值（原地）
///
/// |v| ≤ T[band][c] 的样本置 0（噪声主导）；其余原样保留，
/// 精度交由后续量化管线。条带号由行号除以 BAND_HEIGHT 得出。
pub fn soft_threshold_interleaved(
    pixels: &mut [i32],
    width: usize,
    height: usize,
    components: usize,
    thresholds: &[Vec<i32>],
) {
    let band_h = BAND_HEIGHT;
    for y in 0..height {
        let band = (y / band_h).min(thresholds.len() - 1);
        let t_row = &thresholds[band]; // 每分量一个阈值
        let row = &mut pixels[y * width * components..(y + 1) * width * components];
        // SIMD 分派：同分量阈值一致时整行向量化（逐位一致）
        let uniform_t: Option<i32> = {
            let first = t_row[0];
            if t_row.iter().all(|&t| t == first) {
                Some(first)
            } else {
                None
            }
        };
        match uniform_t {
            Some(t) => crate::crf::backend::ops::soft_threshold_plane(row, t),
            None => {
                for (chunk_idx, chunk) in row.chunks_exact_mut(components).enumerate() {
                    for (c, v) in chunk.iter_mut().enumerate() {
                        let t = t_row[c];
                        // unsigned_abs keeps the i32::MIN edge case defined,
                        // matching the SIMD threshold kernel semantics.
                        if t > 0 && v.unsigned_abs() <= t as u32 {
                            *v = 0;
                        }
                    }
                    let _ = chunk_idx;
                }
            }
        }
    }
}

/// 诊断：输出单平面 |Laplacian| 的 P25/P50/P75/P90 分位（原始桶值）
///
/// 用于判断差分帧的失真形态：P75≫P25 且按 8 像素周期分布 = 典型块效应。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn laplacian_percentile_diag(plane: &[i32], width: usize, height: usize) -> [u32; 4] {
    let at = |x: usize, y: usize| plane[y * width + x];
    let mut hist = [0u32; 4096];
    let mut count = 0usize;
    for y in 1..height.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            let l = at(x - 1, y);
            let r = at(x + 1, y);
            let t = at(x, y - 1);
            let b = at(x, y + 1);
            let tl = at(x - 1, y - 1);
            let tr = at(x + 1, y - 1);
            let bl = at(x - 1, y + 1);
            let br = at(x + 1, y + 1);
            let resp = 4 * at(x, y) + tl + tr + bl + br - 2 * (t + b + l + r);
            hist[resp.unsigned_abs().min(4095) as usize] += 1;
            count += 1;
        }
    }
    let mut out = [0u32; 4];
    let mut acc = 0u32;
    let mut hi = 0usize;
    let targets = [
        (count / 4) as u32,
        (count / 2) as u32,
        (count * 3 / 4) as u32,
        (count * 9 / 10) as u32,
    ];
    for (bi, &cnt) in hist.iter().enumerate() {
        acc += cnt;
        while hi < 4 && acc > targets[hi] {
            out[hi] = bi as u32;
            hi += 1;
        }
        if hi >= 4 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 结构型两极化场（服装/表情差分形态）：
    /// 大面积精确零 + 少量大边缘 → amp25 = 0 → 零介入
    #[test]
    fn test_threshold_bimodal_field_stays_zero() {
        let w = 64usize;
        let h = 32usize;
        let mut px = vec![0i32; w * h * 3];
        // 20% 像素为大结构残差，其余精确零
        for i in 0..w * h {
            if i % 5 == 0 {
                let idx = i * 3;
                px[idx] = if i % 10 == 0 { 150 } else { -180 };
            }
        }
        let th = estimate_interleaved_band_thresholds(&px, w, h, 3, 150);
        assert!(
            th.iter().all(|row| row.iter().all(|&t| t == 0)),
            "两极化结构场的自动阈值应为 0（零介入），实际 {:?}",
            th
        );
    }

    /// 时间差分形态（系统性色调偏移）：场中心 Δ≠0，
    /// 零中心性门控应拦截 → T = 0（杜绝反向膨胀）
    #[test]
    fn test_threshold_offset_field_gated_out() {
        let w = 64usize;
        let h = 32usize;
        let mut state: u64 = 0xAAAA_1111_BBBB_2222;
        let mut px = vec![0i32; w * h * 3];
        for v in px.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let mag = 3 + ((state >> 33) % 10) as i32;
            // 中心在 +12（系统性色调漂移），非零中心
            *v = 12 + if (state >> 50) & 1 == 0 { mag } else { -mag };
        }
        let th = estimate_interleaved_band_thresholds(&px, w, h, 3, 150);
        assert!(
            th.iter().all(|row| row.iter().all(|&t| t == 0)),
            "非零中心的偏移场应被门控拦截（T=0），实际 {:?}",
            th
        );
    }

    /// 零中心噪声场（同图不同压缩版本相减）：门控放行 → 有效阈值
    #[test]
    fn test_threshold_zero_centered_noise_active() {
        let w = 64usize;
        let h = 32usize;
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let mut px = vec![0i32; w * h * 3];
        for v in px.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let mag = 3 + ((state >> 33) % 10) as i32;
            *v = if (state >> 50) & 1 == 0 { mag } else { -mag };
        }
        let th = estimate_interleaved_band_thresholds(&px, w, h, 3, 150);
        for row in &th {
            for &t in row {
                assert!((1..=24).contains(&t), "零中心场阈值应在 (0,24]，实际 {}", t);
            }
        }

        // 滤波后大量像素归零（RLE 收益来源）
        soft_threshold_interleaved(&mut px, w, h, 3, &th);
        let zeros = px.iter().filter(|&&v| v == 0).count();
        assert!(
            zeros * 100 >= px.len() * 40,
            "滤波后零值占比应 ≥40%，实际 {:.1}%",
            zeros as f64 / px.len() as f64 * 100.0
        );
    }

    /// 软阈值语义：噪声级扰动归零，强信号原样保留
    #[test]
    fn test_soft_threshold_zeroes_noise_keeps_signal() {
        let w = 8usize;
        let h = 32usize; // 单条带
        let mut px = vec![0i32; w * h * 3];
        // R 平面：小扰动（应被清零）；G 平面：强边缘（应保留）
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 3;
                px[idx] = match (x + y) % 4 {
                    0 => 5,
                    1 => -6,
                    2 => 3,
                    _ => -4,
                };
                px[idx + 1] = match x % 2 {
                    0 => 90,
                    _ => -120,
                };
                px[idx + 2] = 0;
            }
        }
        let thresholds = vec![vec![10, 10, 10]; 1];
        soft_threshold_interleaved(&mut px, w, h, 3, &thresholds);

        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 3;
                assert_eq!(px[idx], 0, "R 扰动 |v|≤10 应回零");
                assert_ne!(px[idx + 1], 0, "G 强边缘不应被动");
                assert_eq!(px[idx + 2], 0);
            }
        }
    }

    /// activity 步长估计：纹理条带增步长、平坦条带减步长（双向）
    #[test]
    fn test_activity_steps_bidirectional() {
        let w = 64usize;
        let h = 64usize; // BAND_HEIGHT=32 → 2 条带
        let comps = 3;
        let base = 6u8;
        let mut px = vec![0i32; w * h * comps];
        // 条带 0（前 32 行）：平坦（Y 全零，梯度=0）
        // 条带 1（后 32 行）：纹理（Y 交替 ±100，高梯度）
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * comps;
                if y >= 32 {
                    px[idx] = if x % 2 == 0 { 100 } else { -100 };
                }
            }
        }
        // activity_masking=200（纹理增步长）+ flat_area_protection=150（平坦减 2 步长）
        let steps = estimate_band_activity_steps(&px, w, h, comps, base, 200, 150, 100);
        assert_eq!(steps.len(), 2, "BAND_HEIGHT=32 时 64 行应产生 2 条带");
        assert!(steps[0] < base, "平坦条带应减步长防 banding：{:?}", steps);
        assert!(steps[1] > base, "纹理条带应增步长省码率：{:?}", steps);
    }

    /// activity 步长边缘保护：稀疏大梯度（线稿边缘）应减步长
    #[test]
    fn test_activity_steps_edge_reduces_step() {
        let w = 64usize;
        let h = 64usize; // 2 条带
        let comps = 3;
        let base = 6u8;
        let mut px = vec![0i32; w * h * comps];
        // 条带 0（前 32 行）：均匀中等梯度纹理（Y 交替 ±50，梯度=100）
        // 条带 1（后 32 行）：稀疏大梯度边缘（每 10 像素一个 200，其余 0）
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * comps;
                if y >= 32 {
                    if x % 10 == 0 {
                        px[idx] = 200;
                    }
                } else {
                    px[idx] = if x % 2 == 0 { 50 } else { -50 };
                }
            }
        }
        // edge_protection=150（边缘减 2 步长），activity/flat 中性
        let steps = estimate_band_activity_steps(&px, w, h, comps, base, 100, 100, 150);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], base, "纹理条带（activity 中性）应保持基础步长");
        assert!(steps[1] < base, "边缘条带应减步长防 ringing：{:?}", steps);
    }

    /// activity 步长中性：100/100/100 保持基础步长（默认不改变既有产物）
    #[test]
    fn test_activity_steps_neutral_keeps_base() {
        let w = 64usize;
        let h = 64usize;
        let comps = 3;
        let mut px = vec![0i32; w * h * comps];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * comps;
                if y >= 32 {
                    px[idx] = if x % 2 == 0 { 100 } else { -100 };
                }
            }
        }
        let steps = estimate_band_activity_steps(&px, w, h, comps, 6, 100, 100, 100);
        assert!(
            steps.iter().all(|&s| s == 6),
            "中性 100/100/100 应保持基础步长：{:?}",
            steps
        );
    }

    /// activity 步长估计退化输入：width<2 或空输入返回全基础步长
    #[test]
    fn test_activity_steps_degenerate_inputs() {
        let empty = estimate_band_activity_steps(&[], 0, 0, 0, 5, 200, 150, 100);
        assert!(empty.iter().all(|&s| s == 5));
        let narrow = estimate_band_activity_steps(&[0; 3], 1, 1, 3, 5, 200, 150, 100);
        assert!(narrow.iter().all(|&s| s == 5), "width<2 应保持基础步长");
    }

    /// activity 步长上限：极端高梯度条带步长不得超过 Q_BAND_CAP
    #[test]
    fn test_activity_steps_capped() {
        let w = 32usize;
        let h = 64usize; // 2 条带
        let comps = 3;
        let mut px = vec![0i32; w * h * comps];
        // 条带 0：平坦；条带 1：极端高梯度纹理
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * comps;
                if y >= 32 {
                    px[idx] = if x % 2 == 0 { 10000 } else { -10000 };
                }
            }
        }
        let steps = estimate_band_activity_steps(&px, w, h, comps, 4, 1000, 100, 100);
        assert!(
            steps.iter().all(|&s| s <= Q_BAND_CAP),
            "步长不应超过 Q_BAND_CAP：{:?}",
            steps
        );
    }

    /// §28 缺陷修复回归：稀疏差分场景（二次元表情/口型差分主形态——
    /// 静止条带占绝大多数、变化条带 avg 仅 1~3）下 reference 不得退化为
    /// 0，edge 分类必须激活。修复前 Σavg/bands 整数除法 = 0 触发短路、
    /// 全部条带保持 base_step（分类整体失效）。
    #[test]
    fn test_activity_steps_sparse_diff_reference_activates() {
        let w = 16usize;
        let h = 57 * 32; // 57 条带（模拟 1000 组 1024×1820 的条带数）
        let comps = 3;
        let base = 6u8;
        let mut px = vec![0i32; w * h * comps];
        // 条带 3（行 96..128）：低梯度稀疏变化——每行仅 x=1 处一个 30，
        // 其余全零（avg = 30/15 = 2，nz_avg = 30，稀疏大梯度形态）。
        // 其余 56 条带完全静止。
        for y in (3 * 32)..(4 * 32) {
            let idx = (y * w + 1) * comps;
            px[idx] = 30;
        }
        // 修复前：reference = Σavg/bands = 2/57 = 0 → 分类短路，全 base_step。
        // 修复后：reference = max(0,1) = 1 → nz_avg=30 > 2 → edge 减步长。
        let steps = estimate_band_activity_steps(&px, w, h, comps, base, 100, 100, 150);
        assert_eq!(steps.len(), 57);
        assert!(
            steps[3] < base,
            "稀疏差分变化条带应被 edge 分类保护（减步长），实际 {}",
            steps[3]
        );
        for (band, &s) in steps.iter().enumerate() {
            if band != 3 {
                assert_eq!(s, base, "静止条带 {band} 应保持基础步长，实际 {s}");
            }
        }
        // 中性旋钮下修复不改变任何产物（100/100/100 全 base）
        let neutral = estimate_band_activity_steps(&px, w, h, comps, base, 100, 100, 100);
        assert!(neutral.iter().all(|&s| s == base));
    }
}
