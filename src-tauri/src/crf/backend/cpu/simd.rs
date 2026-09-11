//! CPU SIMD 向量化 kernel（原 format/simd.rs，P5 架构迁移）
//!
//! 运行时特性检测选择最优路径；SIMD 与标量回退逐位一致。
//! 量化使用可精确承载 i32×64 分子的 f64，其余 kernel 为纯整数运算，
//! 均由单元测试对拍保证。
//! 目标为无跨元素依赖的逐元素运算：差分、YCoCg-R 变换、软阈值、
//! 固定步长死区量化。空间预测因递推依赖保持标量。
//!
//! **迁移说明（P5）**：本模块原位于 `format/simd.rs`，现迁移到
//! `backend/cpu/simd`。`format/simd.rs` 保留为 `pub use` 转发层。

#[cfg(target_arch = "x86_64")]
pub(crate) fn has_avx2() -> bool {
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
        if pixels.len() >= 24 && has_avx2() {
            // SAFETY: avx2 已检测；kernel 对尾部使用标量处理。
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

// ===== 融合：差分 + YCoCg-R 正变换（一次遍历，省全帧内存往返）=====

/// `out = rct(frame − g_hat)`（3 分量交织）。与 `sub_i32` + `rct_forward_interleaved`
/// 逐位一致，但差分结果驻留寄存器/栈、不写回再读，省一次全帧内存往返。
pub fn sub_rct_forward(frame: &[i32], g_hat: &[i32], out: &mut [i32]) {
    assert_eq!(frame.len(), g_hat.len(), "sub_rct 输入长度必须一致");
    assert_eq!(frame.len(), out.len(), "sub_rct 输出长度必须一致");
    #[cfg(target_arch = "x86_64")]
    {
        if frame.len() >= 24 && has_avx2() {
            // SAFETY: avx2 已检测；kernel 对尾部使用标量处理。
            unsafe { sub_rct_fwd_avx2(frame, g_hat, out) };
            return;
        }
    }
    let done = (frame.len() / 3) * 3;
    for ((f, gg), o) in frame[..done]
        .chunks_exact(3)
        .zip(g_hat[..done].chunks_exact(3))
        .zip(out[..done].chunks_exact_mut(3))
    {
        let dr = f[0] - gg[0];
        let dg = f[1] - gg[1];
        let db = f[2] - gg[2];
        let co = dr - db;
        let t = db + (co >> 1);
        let cg = dg - t;
        o[0] = t + (cg >> 1);
        o[1] = co;
        o[2] = cg;
    }
    // 不足 3 的尾部仅差分（与 sub_i32 + rct（忽略余数）逐位一致）
    for i in done..frame.len() {
        out[i] = frame[i] - g_hat[i];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sub_rct_fwd_avx2(frame: &[i32], g_hat: &[i32], out: &mut [i32]) {
    use std::arch::x86_64::*;
    let n = frame.len();
    let mut off = 0;
    while off + 24 <= n {
        // 差分 24 i32（连续 SIMD，3 批 8）——驻留栈临时，不写回内存
        let d0 = _mm256_sub_epi32(
            _mm256_loadu_si256(frame.as_ptr().add(off).cast()),
            _mm256_loadu_si256(g_hat.as_ptr().add(off).cast()),
        );
        let d1 = _mm256_sub_epi32(
            _mm256_loadu_si256(frame.as_ptr().add(off + 8).cast()),
            _mm256_loadu_si256(g_hat.as_ptr().add(off + 8).cast()),
        );
        let d2 = _mm256_sub_epi32(
            _mm256_loadu_si256(frame.as_ptr().add(off + 16).cast()),
            _mm256_loadu_si256(g_hat.as_ptr().add(off + 16).cast()),
        );
        let mut d = [0i32; 24];
        _mm256_storeu_si256(d.as_mut_ptr().cast(), d0);
        _mm256_storeu_si256(d.as_mut_ptr().add(8).cast(), d1);
        _mm256_storeu_si256(d.as_mut_ptr().add(16).cast(), d2);
        // RCT（8 像素，跨步解包）
        let mut r = [0i32; 8];
        let mut g = [0i32; 8];
        let mut b = [0i32; 8];
        for k in 0..8 {
            r[k] = d[k * 3];
            g[k] = d[k * 3 + 1];
            b[k] = d[k * 3 + 2];
        }
        let vr = _mm256_loadu_si256(r.as_ptr().cast());
        let vg = _mm256_loadu_si256(g.as_ptr().cast());
        let vb = _mm256_loadu_si256(b.as_ptr().cast());
        let vco = _mm256_sub_epi32(vr, vb);
        let vt = _mm256_add_epi32(vb, _mm256_srai_epi32(vco, 1));
        let vcg = _mm256_sub_epi32(vg, vt);
        let vy = _mm256_add_epi32(vt, _mm256_srai_epi32(vcg, 1));
        let yv: [i32; 8] = std::mem::transmute(vy);
        let cov: [i32; 8] = std::mem::transmute(vco);
        let cgv: [i32; 8] = std::mem::transmute(vcg);
        for k in 0..8 {
            let q = out.as_mut_ptr().add(off).add(k * 3);
            *q = yv[k];
            *q.add(1) = cov[k];
            *q.add(2) = cgv[k];
        }
        off += 24;
    }
    // 尾部：差分（全）+ RCT（3 的倍数），与分离版逐位一致
    for i in off..n {
        out[i] = frame[i] - g_hat[i];
    }
    for px in out[off..].chunks_exact_mut(3) {
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
    #[cfg(target_arch = "x86_64")]
    {
        if pixels.len() >= 24 && has_avx2() {
            // SAFETY: avx2 已检测；kernel 对尾部使用标量处理。
            unsafe { rct_inv_avx2(pixels) };
            return;
        }
    }
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
        // unsigned_abs keeps i32::MIN well-defined (debug builds must not panic).
        if v.unsigned_abs() <= t as u32 {
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
        // VPABSD/two's-complement abs leaves INT_MIN negative. Clamp that
        // one value to INT_MAX so it follows the scalar unsigned_abs rule
        // instead of being incorrectly classified as within the threshold.
        let is_min = _mm256_cmpeq_epi32(abs_v, _mm256_set1_epi32(i32::MIN));
        let abs_v = _mm256_blendv_epi8(abs_v, _mm256_set1_epi32(i32::MAX), is_min);
        // |v| > t → 掩码全 1（保留）；否则清零
        let keep = _mm256_cmpgt_epi32(abs_v, vt);
        let kept = _mm256_and_si256(v, keep);
        _mm256_storeu_si256(pixels.as_mut_ptr().add(i).cast::<__m256i>(), kept);
        i += 8;
    }
    while i < pixels.len() {
        if pixels[i].unsigned_abs() <= t as u32 {
            pixels[i] = 0;
        }
        i += 1;
    }
}

// ===== 死区量化（有符号 level 输出） =====

/// 固定步长/偏置的批量死区量化，输出有符号 level。
///
/// AVX2 没有整数除法指令，因此用 4-lane f64 向量除法实现。i32 绝对值
/// 乘 64 后仍可被 f64 精确表示；除法后向零转换与 Rust 整数除法一致。
/// `i32::{MIN,MAX}` 单独走标量参考，避免转换结果越出 i32 level 范围。
pub fn quantize_levels_biased(values: &[i32], out: &mut [i32], q_step: u8, deadzone_bias: i8) {
    assert_eq!(values.len(), out.len(), "量化输入/输出长度必须一致");
    #[cfg(target_arch = "x86_64")]
    {
        if values.len() >= 4 && has_avx2() {
            // SAFETY: AVX2 已检测；kernel 仅访问等长切片范围。
            unsafe { quantize_levels_biased_avx2(values, out, q_step, deadzone_bias) };
            return;
        }
    }
    crate::crf::backend::scalar::quantize_levels_biased(values, out, q_step, deadzone_bias);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn quantize_levels_biased_avx2(
    values: &[i32],
    out: &mut [i32],
    q_step: u8,
    deadzone_bias: i8,
) {
    use std::arch::x86_64::*;

    let q = i32::from(q_step.max(1));
    let denom = _mm256_set1_pd((q * 64) as f64);
    let scale = _mm256_set1_pd(64.0);
    let half = _mm_set1_epi32(q * 32);
    let positive_bias = _mm_set1_epi32(i32::from(deadzone_bias));
    let negative_bias = _mm_set1_epi32(-i32::from(deadzone_bias));
    let min_value = _mm_set1_epi32(i32::MIN);
    let max_value = _mm_set1_epi32(i32::MAX);

    let mut i = 0;
    while i + 4 <= values.len() {
        let value = _mm_loadu_si128(values.as_ptr().add(i).cast::<__m128i>());
        let is_min = _mm_cmpeq_epi32(value, min_value);
        let is_max = _mm_cmpeq_epi32(value, max_value);
        if _mm_movemask_epi8(_mm_or_si128(is_min, is_max)) != 0 {
            crate::crf::backend::scalar::quantize_levels_biased(
                &values[i..i + 4],
                &mut out[i..i + 4],
                q_step,
                deadzone_bias,
            );
            i += 4;
            continue;
        }

        let sign = _mm_srai_epi32(value, 31);
        let magnitude = _mm_sub_epi32(_mm_xor_si128(value, sign), sign);
        let bias = _mm_blendv_epi8(positive_bias, negative_bias, sign);
        let offset = _mm_add_epi32(half, bias);
        let numerator = _mm256_add_pd(
            _mm256_mul_pd(_mm256_cvtepi32_pd(magnitude), scale),
            _mm256_cvtepi32_pd(offset),
        );
        let level = _mm256_cvttpd_epi32(_mm256_div_pd(numerator, denom));
        let signed_level = _mm_sub_epi32(_mm_xor_si128(level, sign), sign);
        _mm_storeu_si128(out.as_mut_ptr().add(i).cast::<__m128i>(), signed_level);
        i += 4;
    }

    crate::crf::backend::scalar::quantize_levels_biased(
        &values[i..],
        &mut out[i..],
        q_step,
        deadzone_bias,
    );
}

// ===== CfL 亮度预测扣除 / 还原 =====

/// 计算亮度线性预测 `pred[i] = (alpha * (y[i] - 128)) >> 4` 并从色度扣除：
/// `out[i] = chroma[i] - pred[i]`（编码端 apply_cfl；alpha=0 由调用方短路）。
///
/// alpha ∈ [-4, 4]、y ∈ [0, 255]（8bit），pred ∈ [-32, 32]，全程 i32 无溢出；
/// `>> 4` 为算术右移，与 Rust i32 右移及解码端 `⌊α·(Y−128)/16⌋` 逐位一致。
pub fn cfl_luma_subtract(chroma: &[i32], y: &[i32], alpha: i32, out: &mut [i32]) {
    debug_assert_eq!(chroma.len(), y.len());
    debug_assert_eq!(chroma.len(), out.len());
    #[cfg(target_arch = "x86_64")]
    {
        if chroma.len() >= 8 && has_avx2() {
            // SAFETY: avx2 已检测；切片边界内操作
            unsafe { cfl_sub_avx2(chroma, y, alpha, out) };
            return;
        }
    }
    for i in 0..chroma.len() {
        out[i] = chroma[i] - ((alpha * (y[i] - 128)) >> 4);
    }
}

/// 解码端 CfL 还原：`plane[i] += (alpha * (y[i] - 128)) >> 4`（原地）。
pub fn cfl_luma_add_in_place(plane: &mut [i32], y: &[i32], alpha: i32) {
    debug_assert_eq!(plane.len(), y.len());
    #[cfg(target_arch = "x86_64")]
    {
        if plane.len() >= 8 && has_avx2() {
            // SAFETY: avx2 已检测；切片边界内操作
            unsafe { cfl_add_avx2(plane, y, alpha) };
            return;
        }
    }
    for i in 0..plane.len() {
        plane[i] += (alpha * (y[i] - 128)) >> 4;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn cfl_sub_avx2(chroma: &[i32], y: &[i32], alpha: i32, out: &mut [i32]) {
    use std::arch::x86_64::*;
    let valpha = _mm256_set1_epi32(alpha);
    let v128 = _mm256_set1_epi32(128);
    let mut i = 0;
    while i + 8 <= chroma.len() {
        let vy = _mm256_loadu_si256(y.as_ptr().add(i).cast::<__m256i>());
        let vc = _mm256_loadu_si256(chroma.as_ptr().add(i).cast::<__m256i>());
        let d = _mm256_sub_epi32(vy, v128);
        let pred = _mm256_srai_epi32(_mm256_mullo_epi32(valpha, d), 4);
        let r = _mm256_sub_epi32(vc, pred);
        _mm256_storeu_si256(out.as_mut_ptr().add(i).cast::<__m256i>(), r);
        i += 8;
    }
    while i < chroma.len() {
        out[i] = chroma[i] - ((alpha * (y[i] - 128)) >> 4);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn cfl_add_avx2(plane: &mut [i32], y: &[i32], alpha: i32) {
    use std::arch::x86_64::*;
    let valpha = _mm256_set1_epi32(alpha);
    let v128 = _mm256_set1_epi32(128);
    let mut i = 0;
    while i + 8 <= plane.len() {
        let vy = _mm256_loadu_si256(y.as_ptr().add(i).cast::<__m256i>());
        let vp = _mm256_loadu_si256(plane.as_ptr().add(i).cast::<__m256i>());
        let d = _mm256_sub_epi32(vy, v128);
        let pred = _mm256_srai_epi32(_mm256_mullo_epi32(valpha, d), 4);
        let r = _mm256_add_epi32(vp, pred);
        _mm256_storeu_si256(plane.as_mut_ptr().add(i).cast::<__m256i>(), r);
        i += 8;
    }
    while i < plane.len() {
        plane[i] += (alpha * (y[i] - 128)) >> 4;
        i += 1;
    }
}

// ===== SAD 绝对值求和 =====

/// Σ|values[i]|（SAD 预筛求和），与标量 `unsigned_abs().sum()` 逐位一致。
///
/// 用于 banded 条带候选 SAD 统计（原本逐元素 `unsigned_abs` 求和）。
/// 补码绝对值对 i32::MIN 保留 0x80000000 位模式——作为 u32 解释即
/// 2147483648，与 `unsigned_abs()` 语义一致（不饱和、不溢出），
/// 因此 AVX2 路径与标量逐位等价。
#[allow(dead_code)] // 预留 SAD 预筛原语（banded 条带候选统计），待接入
pub fn sad_abs_sum(values: &[i32]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: avx2 已检测；切片边界内操作
            return unsafe { sad_abs_sum_avx2(values) };
        }
    }
    values.iter().map(|&v| v.unsigned_abs() as u64).sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(dead_code)] // 仅由预留的 sad_abs_sum 调用
unsafe fn sad_abs_sum_avx2(values: &[i32]) -> u64 {
    use std::arch::x86_64::*;
    let mut acc_lo = _mm256_setzero_si256(); // 低 4 个 u64 累加器
    let mut acc_hi = _mm256_setzero_si256(); // 高 4 个 u64 累加器
    let mut i = 0usize;
    while i + 8 <= values.len() {
        let v = _mm256_loadu_si256(values.as_ptr().add(i).cast::<__m256i>());
        // 补码绝对值：(v ^ (v>>31)) - (v>>31)
        let sign = _mm256_srai_epi32(v, 31);
        let abs = _mm256_sub_epi32(_mm256_xor_si256(v, sign), sign);
        // 拆低 4 高 4，零扩展为 u64 各累加
        let lo = _mm256_castsi256_si128(abs);
        let hi = _mm256_extracti128_si256(abs, 1);
        acc_lo = _mm256_add_epi64(acc_lo, _mm256_cvtepu32_epi64(lo));
        acc_hi = _mm256_add_epi64(acc_hi, _mm256_cvtepu32_epi64(hi));
        i += 8;
    }
    let acc = _mm256_add_epi64(acc_lo, acc_hi);
    let lanes: [u64; 4] = std::mem::transmute(acc);
    let mut sum = lanes.iter().sum::<u64>();
    for &v in &values[i..] {
        sum = sum.wrapping_add(v.unsigned_abs() as u64);
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rct_inv_avx2(pixels: &mut [i32]) {
    use std::arch::x86_64::*;
    let mut off = 0;
    while off + 24 <= pixels.len() {
        let p = pixels.as_ptr().add(off);
        let mut y = [0i32; 8];
        let mut co = [0i32; 8];
        let mut cg = [0i32; 8];
        for k in 0..8 {
            y[k] = *p.add(k * 3);
            co[k] = *p.add(k * 3 + 1);
            cg[k] = *p.add(k * 3 + 2);
        }
        let vy = _mm256_loadu_si256(y.as_ptr().cast::<__m256i>());
        let vco = _mm256_loadu_si256(co.as_ptr().cast::<__m256i>());
        let vcg = _mm256_loadu_si256(cg.as_ptr().cast::<__m256i>());
        let vt = _mm256_sub_epi32(vy, _mm256_srai_epi32(vcg, 1));
        let vg = _mm256_add_epi32(vcg, vt);
        let vb = _mm256_sub_epi32(vt, _mm256_srai_epi32(vco, 1));
        let vr = _mm256_add_epi32(vco, vb);
        let rv: [i32; 8] = std::mem::transmute(vr);
        let gv: [i32; 8] = std::mem::transmute(vg);
        let bv: [i32; 8] = std::mem::transmute(vb);
        for k in 0..8 {
            let q = pixels.as_mut_ptr().add(off).add(k * 3);
            *q = rv[k];
            *q.add(1) = gv[k];
            *q.add(2) = bv[k];
        }
        off += 24;
    }
    for px in pixels[off..].chunks_exact_mut(3) {
        let (y, co, cg) = (px[0], px[1], px[2]);
        let t = y - (cg >> 1);
        let g = cg + t;
        let b = t - (co >> 1);
        px[0] = co + b;
        px[1] = g;
        px[2] = b;
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

    /// 融合 `sub_rct_forward` 与「`sub_i32` + `rct_forward_interleaved`」逐位一致
    /// （含非 3 倍数尾部）。
    #[test]
    fn test_sub_rct_matches_separate() {
        let mut state = 0x5EED_5EEDu64;
        let n = 999; // 非 3 倍数：尾部覆盖
        let mut frame = vec![0i32; n];
        let mut g_hat = vec![0i32; n];
        for v in frame.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *v = ((state >> 33) as i32 % 512) - 256;
        }
        for v in g_hat.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *v = ((state >> 33) as i32 % 512) - 256;
        }
        let mut sep = vec![0i32; n];
        sub_i32(&frame, &g_hat, &mut sep);
        rct_forward_interleaved(&mut sep);
        let mut fus = vec![0i32; n];
        sub_rct_forward(&frame, &g_hat, &mut fus);
        assert_eq!(sep, fus, "融合 sub_rct 与分离版不一致");
    }

    #[test]
    fn test_rct_roundtrip_non_aligned_batch() {
        // 11 像素会经过一个 AVX2 批次并留下标量尾部。
        let original: Vec<i32> = (0..33).map(|i| (i * 17) - 240).collect();
        let mut transformed = original.clone();
        rct_forward_interleaved(&mut transformed);
        let expected_forward: Vec<i32> = original
            .chunks_exact(3)
            .flat_map(|px| {
                let co = px[0] - px[2];
                let t = px[2] + (co >> 1);
                let cg = px[1] - t;
                [t + (cg >> 1), co, cg]
            })
            .collect();
        assert_eq!(transformed, expected_forward);
        rct_inverse_interleaved(&mut transformed);
        assert_eq!(transformed, original);
    }

    #[test]
    fn test_soft_threshold_i32_min_is_safe() {
        let mut values = [i32::MIN, -3, 3, 9];
        soft_threshold_plane(&mut values, 3);
        assert_eq!(values, [i32::MIN, 0, 0, 9]);
    }

    /// sad_abs_sum 与标量 `unsigned_abs().sum()` 逐位一致（含 i32::MIN、
    /// 非对齐长度、大数组溢出路径）。
    #[test]
    fn test_sad_abs_sum_matches_scalar() {
        let mut state: u64 = 0xDEAD_BEEF_0123_4567;
        let next = |state: &mut u64| -> i32 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // 覆盖全 i32 值域：高位随机符号 + 低位随机幅度（含 MIN 边界）
            (*state >> 33) as i32
        };

        let lengths: [usize; 6] = [0, 1, 7, 8, 31, 1003];
        for &len in &lengths {
            let values: Vec<i32> = (0..len).map(|_| next(&mut state)).collect();
            let expected: u64 = values.iter().map(|&v| v.unsigned_abs() as u64).sum();
            assert_eq!(sad_abs_sum(&values), expected, "len={len}");
        }

        // i32::MIN 显式用例（补码绝对值为 0x8000_0000 → 2147483648）
        let min_case = [i32::MIN, 0, -1, 1, 5, -5];
        assert_eq!(sad_abs_sum(&min_case), 2147483648u64 + 1 + 1 + 5 + 5);
    }

    #[test]
    fn test_quantize_levels_biased_matches_scalar() {
        let mut state = 0x51A7_D20Eu64;
        let mut values = vec![0i32; 1003];
        for value in &mut values {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *value = ((state >> 32) as i32 % 1_000_000) - 500_000;
        }
        values[0] = i32::MIN;
        values[1] = i32::MAX;

        for q_step in [1u8, 2, 3, 5, 10, 20, 255] {
            for deadzone_bias in [-32i8, -4, 0, 4, 32] {
                let mut expected = vec![0i32; values.len()];
                let mut actual = vec![0i32; values.len()];
                crate::crf::backend::scalar::quantize_levels_biased(
                    &values,
                    &mut expected,
                    q_step,
                    deadzone_bias,
                );
                quantize_levels_biased(&values, &mut actual, q_step, deadzone_bias);
                assert_eq!(actual, expected, "q={q_step} bias={deadzone_bias}");
            }
        }
    }

    #[test]
    fn test_quantize_levels_biased_thresholds_and_tail() {
        let values = [-10, -6, -5, -4, -1, 0, 1, 4, 5, 6, 10];
        let mut expected = [0i32; 11];
        let mut actual = [0i32; 11];
        crate::crf::backend::scalar::quantize_levels_biased(&values, &mut expected, 10, 4);
        quantize_levels_biased(&values, &mut actual, 10, 4);
        assert_eq!(actual, expected);
    }

    /// CfL 亮度预测扣除/还原：AVX2 与标量逐位一致，sub/add 互逆。
    #[test]
    fn test_cfl_luma_subtract_add_matches_scalar_and_roundtrips() {
        let mut state: u64 = 0x00C0_FFEE_1234_5678;
        let next = |state: &mut u64| -> i32 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*state >> 33) as i32 % 512) - 256
        };
        // 覆盖 AVX2 批量路径（>=8）与标量尾部（<8）
        for n in [1usize, 7, 8, 9, 31, 1000] {
            for &alpha in &[-4i32, -3, -1, 0, 1, 2, 4] {
                let y: Vec<i32> = (0..n).map(|_| next(&mut state).rem_euclid(256)).collect();
                let chroma: Vec<i32> = (0..n).map(|_| next(&mut state)).collect();

                // subtract 对拍
                let mut actual_sub = vec![0i32; n];
                cfl_luma_subtract(&chroma, &y, alpha, &mut actual_sub);
                let expected_sub: Vec<i32> = chroma
                    .iter()
                    .zip(y.iter())
                    .map(|(&c, &yv)| c - ((alpha * (yv - 128)) >> 4))
                    .collect();
                assert_eq!(actual_sub, expected_sub, "alpha={alpha} n={n} subtract");

                // add 对拍 + 与 subtract 互逆
                let mut actual_add = chroma.clone();
                cfl_luma_add_in_place(&mut actual_add, &y, alpha);
                let expected_add: Vec<i32> = chroma
                    .iter()
                    .zip(y.iter())
                    .map(|(&c, &yv)| c + ((alpha * (yv - 128)) >> 4))
                    .collect();
                assert_eq!(actual_add, expected_add, "alpha={alpha} n={n} add");
                // add 应还原 subtract 的扣除
                let mut restored = actual_sub.clone();
                cfl_luma_add_in_place(&mut restored, &y, alpha);
                assert_eq!(restored, chroma, "alpha={alpha} n={n} roundtrip");
            }
        }
    }
}
