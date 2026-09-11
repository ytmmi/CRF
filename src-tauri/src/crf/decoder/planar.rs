//! 三平面打包解码（frame_type=3）：子帧解码 + CfL 还原 + 色度上采样

use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::ColorFormat;
use crate::crf::error::{CrfError, CrfResult};

use super::reconstruct::reconstruct_frame;

/// 双线性 2× 上采样（色度半分辨率还原）
///
/// 每个输出像素由输入四邻加权：奇数坐标落在采样点之间时线性插值，
/// 边缘 clamp。与编码端的 2×2 均值下采样配对构成标准 420 变换链。
///
/// # 定点化（与 f64 逐位一致）
///
/// 输出坐标映射 `s = t/2 − 0.25`，使插值权重只取三组值——
/// 首列（`s<0` 被 clamp）：`{1.25, −0.25}`；奇数坐标：`{0.75, 0.25}`；
/// 偶数坐标：`{0.25, 0.75}`。乘 4 后为 `{5,−1}` / `{3,1}` / `{1,3}`，
/// 全为整数，且原始权重是分母为 4 的二进制精确分数，故 f64 的
/// 乘加无任何舍入误差。两次 1-D 插值合成为
/// `out = round_half_away_from_zero(N / 16)`，N 为整数——与 f64
/// `.round()`（半值远离零）严格逐位一致，由差分测试锁定。
fn upsample_2x_bilinear(small: &[i32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<i32> {
    let mut out = vec![0i32; dw * dh];
    #[cfg(target_arch = "x86_64")]
    {
        // SIMD 使用 i32 中间量；最坏 N ≤ 36·|v|，要求 |v| ≤ 2^25 才无溢出。
        // 色度平面幅值远低于此界（≤ 16bit），实测恒走 SIMD；超界自动回退标量。
        const SIMD_ABS_BOUND: u32 = 1 << 25;
        let bounded = small.iter().all(|&v| v.unsigned_abs() <= SIMD_ABS_BOUND);
        if bounded && std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 已检测；输入幅值受 SIMD_ABS_BOUND 限制，i32 中间量无溢出。
            unsafe { upsample_2x_bilinear_avx2(small, sw, sh, dw, dh, &mut out) };
            return out;
        }
    }
    upsample_2x_bilinear_scalar(small, sw, sh, dw, dh, &mut out);
    out
}

/// 输出坐标 `t`（0..n）到源邻域 `(i0, i1)` 与定点权重 `(w0, w1)`（×4）。
///
/// 首列（t==0）权重 `{5,−1}` 对应被 clamp 的 `s=−0.25`；奇数 `{3,1}`；
/// 偶数 `{1,3}`。i1 越上界时 clamp 到 n−1（右/下边缘退化邻域）。
#[inline]
fn up2_weights(t: usize, n: usize) -> (usize, usize, i32, i32) {
    if t == 0 {
        (0, 1.min(n - 1), 5, -1)
    } else if t & 1 == 1 {
        let k = (t - 1) / 2;
        (k, (k + 1).min(n - 1), 3, 1)
    } else {
        let k = t / 2 - 1;
        (k, k + 1, 1, 3)
    }
}

/// 半值远离零地计算 `round(n / 16)`，并饱和到 i32（与 `f64::round() as i32` 一致）。
#[inline]
fn round_half_away_16(n: i64) -> i32 {
    let mag = n.unsigned_abs();
    let q = (mag + 8) / 16;
    let q = if n < 0 { -(q as i64) } else { q as i64 };
    q.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// 标量定点实现（i64 中间量，任意 i32 输入下与 f64 逐位一致）。
fn upsample_2x_bilinear_scalar(
    small: &[i32],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    out: &mut [i32],
) {
    for y in 0..dh {
        let (y0, y1, wy0, wy1) = up2_weights(y, sh);
        for x in 0..dw {
            let (x0, x1, wx0, wx1) = up2_weights(x, sw);
            let v00 = small[y0 * sw + x0] as i64;
            let v10 = small[y0 * sw + x1] as i64;
            let v01 = small[y1 * sw + x0] as i64;
            let v11 = small[y1 * sw + x1] as i64;
            let top4 = v00 * wx0 as i64 + v10 * wx1 as i64;
            let bot4 = v01 * wx0 as i64 + v11 * wx1 as i64;
            let n = top4 * wy0 as i64 + bot4 * wy1 as i64;
            out[y * dw + x] = round_half_away_16(n);
        }
    }
}

/// AVX2 实现：每次处理 8 个输出像素，用 gather 读取源邻域，
/// 整数乘加 + 半值远离零取整，与标量定点逐位一致。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn upsample_2x_bilinear_avx2(
    small: &[i32],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    out: &mut [i32],
) {
    use std::arch::x86_64::*;
    let swm1 = (sw - 1) as i32;
    let swm1v = _mm256_set1_epi32(swm1);
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let five = _mm256_set1_epi32(5);
    let three = _mm256_set1_epi32(3);
    let mone = _mm256_set1_epi32(-1);
    let eight = _mm256_set1_epi32(8);

    for y in 0..dh {
        let (y0, y1, wy0, wy1) = up2_weights(y, sh);
        let wy0v = _mm256_set1_epi32(wy0);
        let wy1v = _mm256_set1_epi32(wy1);
        let row0 = small.as_ptr().add(y0 * sw);
        let row1 = small.as_ptr().add(y1 * sw);

        let mut x = 0usize;
        while x + 8 <= dw {
            let idx = _mm256_setr_epi32(
                x as i32,
                (x + 1) as i32,
                (x + 2) as i32,
                (x + 3) as i32,
                (x + 4) as i32,
                (x + 5) as i32,
                (x + 6) as i32,
                (x + 7) as i32,
            );
            // x0 = max((x-1)>>1, 0)（算术右移）；x1 = min(x0+1, sw-1)
            let t = _mm256_sub_epi32(idx, one);
            let x0 = _mm256_max_epi32(_mm256_srai_epi32(t, 1), zero);
            let x1 = _mm256_min_epi32(_mm256_add_epi32(x0, one), swm1v);

            // 权重：首列 {5,−1}，奇数 {3,1}，偶数 {1,3}
            let is_zero = _mm256_cmpeq_epi32(idx, zero);
            let is_odd = _mm256_cmpeq_epi32(_mm256_and_si256(idx, one), one);
            let w0 = _mm256_blendv_epi8(one, three, is_odd);
            let w0 = _mm256_blendv_epi8(w0, five, is_zero);
            let w1 = _mm256_blendv_epi8(three, one, is_odd);
            let w1 = _mm256_blendv_epi8(w1, mone, is_zero);

            let v00 = _mm256_i32gather_epi32(row0, x0, 4);
            let v10 = _mm256_i32gather_epi32(row0, x1, 4);
            let v01 = _mm256_i32gather_epi32(row1, x0, 4);
            let v11 = _mm256_i32gather_epi32(row1, x1, 4);

            let top4 = _mm256_add_epi32(_mm256_mullo_epi32(v00, w0), _mm256_mullo_epi32(v10, w1));
            let bot4 = _mm256_add_epi32(_mm256_mullo_epi32(v01, w0), _mm256_mullo_epi32(v11, w1));
            let n = _mm256_add_epi32(
                _mm256_mullo_epi32(top4, wy0v),
                _mm256_mullo_epi32(bot4, wy1v),
            );

            // 半值远离零取整：sign(n) * ((|n|+8)>>4)
            let mag = _mm256_abs_epi32(n);
            let q = _mm256_srli_epi32(_mm256_add_epi32(mag, eight), 4);
            let negq = _mm256_sub_epi32(zero, q);
            let is_neg = _mm256_cmpgt_epi32(zero, n);
            let res = _mm256_blendv_epi8(q, negq, is_neg);

            _mm256_storeu_si256(out.as_mut_ptr().add(y * dw + x).cast::<__m256i>(), res);
            x += 8;
        }
        // 尾部（不足 8 个）走标量，保证任意宽度逐位一致
        for x in x..dw {
            let (x0, x1, wx0, wx1) = up2_weights(x, sw);
            let v00 = small[y0 * sw + x0] as i64;
            let v10 = small[y0 * sw + x1] as i64;
            let v01 = small[y1 * sw + x0] as i64;
            let v11 = small[y1 * sw + x1] as i64;
            let top4 = v00 * wx0 as i64 + v10 * wx1 as i64;
            let bot4 = v01 * wx0 as i64 + v11 * wx1 as i64;
            let n = top4 * wy0 as i64 + bot4 * wy1 as i64;
            out[y * dw + x] = round_half_away_16(n);
        }
    }
}

/// 解码三平面打包帧（frame_type=3）
///
/// 载荷布局与 encode_planar_payload 对应：
/// [ss_cfl u16 LE]   低字节=[cfl_flags]（αc/αg）；高字节 bit0=色度半分辨率标志
/// [len1 u32 LE][sub_frame1(Gray, 全分辨率)]
/// [len2 u32 LE][sub_frame2(Gray, 视标志而定)]
/// [len3 u32 LE][sub_frame3(Gray, 同上)]
///
/// CfL 还原：Co/Cg 平面加回 `⌊α·(Y−128)/16⌋`（Y 已先解码重建，因果安全）；
/// 半分辨率时 Co/Cg 先双线性上采样到全尺寸再做 CfL 与交错。
pub(crate) fn decode_planar(data: &[u8], header: &CrfHeader) -> CrfResult<Vec<i32>> {
    let full_w = header.width as usize;
    let full_h = header.height as usize;
    let pixel_count = full_w * full_h;
    if data.len() < 2 {
        return Err(CrfError::InsufficientData {
            expected: 2,
            actual: data.len(),
        });
    }
    // 字节序：低字节 cfl_flags，高字节 ss_flags（bit0=半分辨率）
    let ss_flags = data[1];
    let half_res = ss_flags & 0x01 != 0;

    let cfl_byte = data[0];
    let alpha_c = ((cfl_byte >> 4) as i32) - 8;
    let alpha_g = ((cfl_byte & 0x0F) as i32) - 8;

    let cw = full_w.div_ceil(2);
    let ch = full_h.div_ceil(2);
    let _chroma_pixels = if half_res { cw * ch } else { pixel_count };

    let mut planes: Vec<Vec<i32>> = Vec::with_capacity(3);
    let mut offset = 2;

    for plane_idx in 0..3 {
        if offset + 4 > data.len() {
            return Err(CrfError::InvalidCodingParams(format!(
                "planar: sub{} len-hdr OOB (off={} total={})",
                plane_idx,
                offset,
                data.len()
            )));
        }
        let sub_len = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + sub_len > data.len() {
            return Err(CrfError::InvalidCodingParams(format!(
                "planar: sub{} payload OOB (len={} off={} total={})",
                plane_idx,
                sub_len,
                offset,
                data.len()
            )));
        }

        // 构造单分量虚拟文件头，让子帧复用整条解码管线
        // Y 平面全分辨率；Co/Cg 在半分辨率模式下为 (cw)×(ch)
        let mut sub_header = header.clone();
        sub_header.color_format = ColorFormat::Gray;
        if half_res && plane_idx > 0 {
            sub_header.width = cw as u16;
            sub_header.height = ch as u16;
        }
        let sub_frame = reconstruct_frame(&data[offset..offset + sub_len], &sub_header)?;
        planes.push(sub_frame.pixels);
        offset += sub_len;
    }

    if planes.len() < 3 || planes.iter().any(|p| p.is_empty()) {
        return Err(CrfError::InvalidCodingParams(
            "planar payload plane size mismatch".to_string(),
        ));
    }

    // 半分辨率：Co/Cg 双线性上采样到全尺寸
    if half_res {
        for plane_idx in [1usize, 2] {
            let small = std::mem::take(&mut planes[plane_idx]);
            planes[plane_idx] = upsample_2x_bilinear(&small, cw, ch, full_w, full_h);
        }
    }

    if planes.iter().any(|p| p.len() != pixel_count) {
        return Err(CrfError::InvalidCodingParams(
            "planar payload plane size mismatch".to_string(),
        ));
    }

    // CfL 还原：色度平面加回亮度线性预测（Y 已重建，因果安全）
    let (y_plane, chroma_planes) = planes.split_at_mut(1);
    let y = &y_plane[0];
    for (plane_idx, alpha) in [(0usize, alpha_c), (1, alpha_g)] {
        if alpha != 0 {
            crate::crf::backend::ops::cfl_luma_add_in_place(
                &mut chroma_planes[plane_idx],
                y,
                alpha,
            );
        }
    }

    // 交错还原：[Y,Co,Cg] 逐像素拼接
    let mut out = vec![0i32; pixel_count * 3];
    for i in 0..pixel_count {
        out[i * 3] = y[i];
        out[i * 3 + 1] = chroma_planes[0][i];
        out[i * 3 + 2] = chroma_planes[1][i];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原 f64 实现（作为逐位一致的参考基准）
    fn upsample_f64_reference(
        small: &[i32],
        sw: usize,
        sh: usize,
        dw: usize,
        dh: usize,
    ) -> Vec<i32> {
        let mut out = vec![0i32; dw * dh];
        for y in 0..dh {
            let sy = (y as f64 + 0.5) / 2.0 - 0.5;
            let y0 = sy.floor().max(0.0) as usize;
            let y1 = (y0 + 1).min(sh - 1);
            let fy = sy - y0 as f64;
            for x in 0..dw {
                let sx = (x as f64 + 0.5) / 2.0 - 0.5;
                let x0 = sx.floor().max(0.0) as usize;
                let x1 = (x0 + 1).min(sw - 1);
                let fx = sx - x0 as f64;

                let v00 = small[y0 * sw + x0] as f64;
                let v10 = small[y0 * sw + x1] as f64;
                let v01 = small[y1 * sw + x0] as f64;
                let v11 = small[y1 * sw + x1] as f64;
                let top = v00 * (1.0 - fx) + v10 * fx;
                let bot = v01 * (1.0 - fx) + v11 * fx;
                out[y * dw + x] = (top * (1.0 - fy) + bot * fy).round() as i32;
            }
        }
        out
    }

    fn rand_small(sw: usize, sh: usize, seed: u64, range: i32) -> Vec<i32> {
        let mut state = seed;
        let n = sw * sh;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push(((state >> 33) as i32 % range) - range / 2);
        }
        v
    }

    #[test]
    fn test_upsample_fixed_matches_f64_various_sizes() {
        // 覆盖 8 的整倍数、非对齐宽度（AVX2 尾部路径）、奇数尺寸与 1×N 退化。
        let cases: [(usize, usize, usize, usize); 8] = [
            (1, 1, 2, 2),
            (1, 4, 2, 8),
            (4, 1, 8, 2),
            (3, 5, 6, 10),
            (7, 3, 14, 6),
            (8, 8, 16, 16),
            (9, 9, 18, 18),
            (10, 11, 20, 22),
        ];
        for (sw, sh, dw, dh) in cases {
            let small = rand_small(sw, sh, 0xDEAD_BEEF ^ sw as u64, 511);
            let expect = upsample_f64_reference(&small, sw, sh, dw, dh);
            let actual = upsample_2x_bilinear(&small, sw, sh, dw, dh);
            assert_eq!(actual, expect, "sw={sw} sh={sh} dw={dw} dh={dh}");
        }
    }

    #[test]
    fn test_upsample_fixed_matches_f64_random() {
        // 随机多轮，覆盖 i32 幅值范围（含负值、大值），验证 AVX2 与标量路径均逐位一致。
        for round in 0..50u64 {
            let sw = 2 + (round % 7) as usize;
            let sh = 2 + ((round * 3) % 7) as usize;
            let dw = sw * 2;
            let dh = sh * 2;
            let small = rand_small(sw, sh, 0xC0FFEE00 ^ round, 1_000_000);
            let expect = upsample_f64_reference(&small, sw, sh, dw, dh);
            let actual = upsample_2x_bilinear(&small, sw, sh, dw, dh);
            assert_eq!(actual, expect, "round={round} sw={sw} sh={sh}");
        }
    }
}
