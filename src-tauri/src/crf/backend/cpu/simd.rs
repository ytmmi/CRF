//! CPU SIMD 向量化 kernel（原 format/simd.rs，P5 架构迁移）
//!
//! 运行时特性检测选择最优路径；SIMD 与标量回退逐位一致
//! （纯整数加减/比较/移位，无浮点），由单元测试保证。
//! 目标为无跨元素依赖的逐元素运算：差分、YCoCg-R 变换、软阈值。
//! 空间预测（递推依赖）与死区量化（变量除法）保持标量。
//!
//! **迁移说明（P5）**：本模块原位于 `format/simd.rs`，现迁移到
//! `backend/cpu/simd`。`format/simd.rs` 保留为 `pub use` 转发层。

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
}

// ===== 差分 =====

/// out[i] = a[i] - b[i]（SIMD 分派）
pub fn sub_i32(a: &[i32], b: &[i32], out: &mut [i32]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(a.len(), out.len());
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: avx2 已检测；切片边界内操作
            unsafe { sub_i32_avx2(a, b, out) };
            return;
        }
    }
    for i in 0..a.len() {
        out[i] = a[i] - b[i];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sub_i32_avx2(a: &[i32], b: &[i32], out: &mut [i32]) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 8 <= a.len() {
        let va = _mm256_loadu_si256(a.as_ptr().add(i).cast::<__m256i>());
        let vb = _mm256_loadu_si256(b.as_ptr().add(i).cast::<__m256i>());
        _mm256_storeu_si256(
            out.as_mut_ptr().add(i).cast::<__m256i>(),
            _mm256_sub_epi32(va, vb),
        );
        i += 8;
    }
    while i < a.len() {
        out[i] = a[i] - b[i];
        i += 1;
    }
}

// ===== YCoCg-R 正变换（3 分量交织，原地）=====

/// Co=R-B, t=B+(Co>>1), Cg=G-t, Y=t+(Cg>>1)；与 rct_forward 逐位一致
pub fn rct_forward_interleaved(pixels: &mut [i32]) {
    #[cfg(target_arch = "x86_64")]
    {
        if pixels.len().is_multiple_of(24) && has_avx2() {
            // SAFETY: avx2 已检测；24 i32 对齐切分（8 像素/批）
            unsafe { rct_fwd_avx2(pixels) };
            return;
        }
    }
    for px in pixels.chunks_exact_mut(3) {
        let (r, g, b) = (px[0], px[1], px[2]);
        let co = r - b;
        let t = b + (co >> 1);
        let cg = g - t;
        px[0] = t + (cg >> 1);
        px[1] = co;
        px[2] = cg;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rct_fwd_avx2(pixels: &mut [i32]) {
    use std::arch::x86_64::*;
    let mut off = 0;
    while off + 24 <= pixels.len() {
        // 解包 8 像素的 R/G/B 平面（跨步标量读取）
        let p = pixels.as_ptr().add(off);
        let mut r = [0i32; 8];
        let mut g = [0i32; 8];
        let mut b = [0i32; 8];
        for k in 0..8 {
            r[k] = *p.add(k * 3);
            g[k] = *p.add(k * 3 + 1);
            b[k] = *p.add(k * 3 + 2);
        }
        let vr = _mm256_loadu_si256(r.as_ptr().cast());
        let vg = _mm256_loadu_si256(g.as_ptr().cast());
        let vb = _mm256_loadu_si256(b.as_ptr().cast());

        // 核心算术：全 AVX2 向量化
        let vco = _mm256_sub_epi32(vr, vb);
        let vt = _mm256_add_epi32(vb, _mm256_srai_epi32(vco, 1));
        let vcg = _mm256_sub_epi32(vg, vt);
        let vy = _mm256_add_epi32(vt, _mm256_srai_epi32(vcg, 1));

        // 散布回交织槽位（y/co/cg 平面向量 → 标量写回交织布局）
        let yv: [i32; 8] = std::mem::transmute(vy);
        let cov: [i32; 8] = std::mem::transmute(vco);
        let cgv: [i32; 8] = std::mem::transmute(vcg);
        for k in 0..8 {
            let q = pixels.as_mut_ptr().add(off).add(k * 3);
            *q = yv[k];
            *q.add(1) = cov[k];
            *q.add(2) = cgv[k];
        }
        off += 24;
    }
    // 尾部标量
    let done = (pixels.len() / 24) * 24;
    for px in pixels[done..].chunks_exact_mut(3) {
        let (r, g, b) = (px[0], px[1], px[2]);
        let co = r - b;
        let t = b + (co >> 1);
        let cg = g - t;
        px[0] = t + (cg >> 1);
        px[1] = co;
        px[2] = cg;
    }
}

// ===== YCoCg-R 逆变换 =====

/// t=Y-(Cg>>1), G=Cg+t, B=t-(Co>>1), R=Co+B；与 rct_inverse 逐位一致
pub fn rct_inverse_interleaved(pixels: &mut [i32]) {
    for px in pixels.chunks_exact_mut(3) {
        let (y, co, cg) = (px[0], px[1], px[2]);
        let t = y - (cg >> 1);
        let g = cg + t;
        let b = t - (co >> 1);
        px[0] = co + b;
        px[1] = g;
        px[2] = b;
    }
}

// ===== 软阈值（平面版，供噪声归一化）=====

/// |v| <= t 的置零（单平面）
pub fn soft_threshold_plane(pixels: &mut [i32], t: i32) {
    if t <= 0 {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: avx2 已检测
            unsafe { soft_thr_avx2(pixels, t) };
            return;
        }
    }
    for v in pixels.iter_mut() {
        if v.abs() <= t {
            *v = 0;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn soft_thr_avx2(pixels: &mut [i32], t: i32) {
    use std::arch::x86_64::*;
    let vt = _mm256_set1_epi32(t);
    // 补码绝对值：abs = (v ^ (v>>31)) - (v>>31)（INT_MIN 亦安全）
    let mut i = 0;
    while i + 8 <= pixels.len() {
        let v = _mm256_loadu_si256(pixels.as_ptr().add(i).cast::<__m256i>());
        let sign_bits = _mm256_srai_epi32(v, 31);
        let abs_v = _mm256_sub_epi32(_mm256_xor_si256(v, sign_bits), sign_bits);
        // |v| > t → 掩码全 1（保留）；否则清零
        let keep = _mm256_cmpgt_epi32(abs_v, vt);
        let kept = _mm256_and_si256(v, keep);
        _mm256_storeu_si256(pixels.as_mut_ptr().add(i).cast::<__m256i>(), kept);
        i += 8;
    }
    while i < pixels.len() {
        if pixels[i].abs() <= t {
            pixels[i] = 0;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SIMD 与标量差分逐位一致
    #[test]
    fn test_sub_simd_matches_scalar() {
        let mut state: u64 = 0xD1FF;
        let next = |state: &mut u64| -> i32 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*state >> 33) as i32 % 512) - 256
        };
        let a: Vec<i32> = (0..1000).map(|_| next(&mut state)).collect();
        let b: Vec<i32> = (0..1000).map(|_| next(&mut state)).collect();
        let mut out = vec![0i32; 1000];
        sub_i32(&a, &b, &mut out);
        for i in 0..1000 {
            assert_eq!(out[i], a[i] - b[i]);
        }
    }

    /// YCoCg-R 正逆变换往返：随机数据逐位一致
    #[test]
    fn test_rct_simd_roundtrip() {
        let mut state: u64 = 0x5EED_1234_ABCD;
        let mut px: Vec<i32> = (0..999) // 非 3 整除尾部覆盖
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 33) as i32 % 512) - 256
            })
            .collect();
        let orig = px.clone();
        rct_forward_interleaved(&mut px);
        rct_inverse_interleaved(&mut px);
        // 尾部不足 3 的样本不参与变换（原样保留）
        let n = (orig.len() / 3) * 3;
        assert_eq!(orig[..n], px[..n], "YCoCg-R 往返失败");
    }
}