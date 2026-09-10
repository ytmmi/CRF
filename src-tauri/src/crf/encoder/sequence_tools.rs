//! P5 序列级工具：变化 mask、整数位移搜索与残差候选预处理。
//! 这些函数是纯 CPU 参考实现，编码器可在不改变码流语义的前提下替换为 SIMD kernel。

/// LIC 全局开关（A/B 验证与逃生门）：`CRF_DISABLE_LIC=1` 时编码端完全跳过
/// LIC 竞争（产物与 v1.15 前的 golden 差分语义一致，除帧头固定 14 字节）。
/// batch 与 streaming 共用，保证两条路径决策一致。
pub fn lic_globally_enabled() -> bool {
    !std::env::var("CRF_DISABLE_LIC")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// 以 tile 为单位计算变化掩码。返回行优先 tile 标志，true 表示需要编码。
pub fn change_mask(
    current: &[i32],
    reference: &[i32],
    width: usize,
    height: usize,
    components: usize,
    tile_size: usize,
    threshold: i32,
) -> Vec<bool> {
    if width == 0 || height == 0 || components == 0 || current.len() != reference.len() {
        return Vec::new();
    }
    let ts = tile_size.max(1);
    let tx = width.div_ceil(ts);
    let ty = height.div_ceil(ts);
    let mut out = vec![false; tx * ty];
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) * components;
            let changed =
                (0..components).any(|c| (current[p + c] - reference[p + c]).abs() > threshold);
            if changed {
                out[(y / ts) * tx + x / ts] = true;
            }
        }
    }
    out
}

/// 将 mask 应用于残差：静止 tile 置零，变化 tile 保持原值。
pub fn apply_change_mask(
    residual: &mut [i32],
    mask: &[bool],
    width: usize,
    height: usize,
    components: usize,
    tile_size: usize,
) {
    let ts = tile_size.max(1);
    let tx = width.div_ceil(ts);
    for y in 0..height {
        for x in 0..width {
            if !mask.get((y / ts) * tx + x / ts).copied().unwrap_or(true) {
                let p = (y * width + x) * components;
                if p + components <= residual.len() {
                    residual[p..p + components].fill(0);
                }
            }
        }
    }
}

/// 小范围整数位移搜索。返回 (dx,dy,SAD)，位移超出边界的像素被忽略。
pub fn search_integer_motion(
    current: &[i32],
    reference: &[i32],
    width: usize,
    height: usize,
    components: usize,
    range: u8,
) -> (i32, i32, u64) {
    let r = range as i32;
    let mut best = (0, 0, u64::MAX);
    for dy in -r..=r {
        for dx in -r..=r {
            let mut sad = 0u64;
            for y in 0..height as i32 {
                for x in 0..width as i32 {
                    let sx = x + dx;
                    let sy = y + dy;
                    let a = (y as usize * width + x as usize) * components;
                    if sx < 0 || sy < 0 || sx >= width as i32 || sy >= height as i32 {
                        for c in 0..components {
                            sad = sad.saturating_add(current[a + c].unsigned_abs() as u64);
                        }
                        continue;
                    }
                    let b = (sy as usize * width + sx as usize) * components;
                    for c in 0..components {
                        sad = sad.saturating_add(
                            (current[a + c] - reference[b + c]).unsigned_abs() as u64,
                        );
                    }
                }
            }
            if sad < best.2 {
                best = (dx, dy, sad);
            }
        }
    }
    best
}

/// 轻量残差专用预处理：对给定阈值做稀疏化并返回非零比例。
pub fn sparsify_residual(residual: &mut [i32], threshold: i32) -> f32 {
    let mut nz = 0usize;
    for v in &mut *residual {
        if v.abs() <= threshold {
            *v = 0;
        } else {
            nz += 1;
        }
    }
    if residual.is_empty() {
        0.0
    } else {
        nz as f32 / residual.len() as f32
    }
}

/// 按帧复杂度分配量化步长（P5.6）。复杂帧获得较小步长（更多码率），
/// 并受最小/最大步长约束；结果确定性且不依赖浮点运算。
pub fn allocate_quant_steps(
    complexity: &[u64],
    base_step: u8,
    min_step: u8,
    max_step: u8,
) -> Vec<u8> {
    if complexity.is_empty() {
        return Vec::new();
    }
    let min_c = *complexity.iter().min().unwrap_or(&0);
    let max_c = *complexity.iter().max().unwrap_or(&0);
    let span = max_c.saturating_sub(min_c);
    complexity
        .iter()
        .map(|&c| {
            let inv = if span == 0 {
                128
            } else {
                ((max_c.saturating_sub(c)).saturating_mul(255) / span).min(255) as u8
            };
            let delta = (base_step as i32 * (inv as i32 - 128) / 256) as i32;
            (base_step as i32 + delta).clamp(min_step as i32, max_step as i32) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_marks_changed_tile() {
        let mut b = vec![0; 4 * 4];
        b[5] = 3;
        let m = change_mask(&b, &[0; 16], 4, 4, 1, 2, 0);
        assert_eq!(m, vec![true, false, false, false]);
    }
    #[test]
    fn motion_finds_shift() {
        let mut r = vec![0; 16];
        r[1] = 10;
        let mut c = vec![0; 16];
        c[2] = 10;
        let (dx, _, _) = search_integer_motion(&c, &r, 4, 4, 1, 2);
        assert_eq!(dx, -1);
    }
}
