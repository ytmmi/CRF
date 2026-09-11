//! 平面空间预测 SIMD（components==1：planar 子平面 / 灰度帧）
//!
//! Horizontal / Vertical / Average / DC 为无递推依赖的因果模式，在单分量
//! （stride=width）布局下可 AVX2 批量预测；Med/Paeth/斜向/多参考等分支密集
//! 模式仍走标量 `predict_at`（本模块不覆盖）。
//!
//! 边界语义与 `core::prediction::intra::predict_at` 逐位一致：
//! - Horizontal: pred = x>0 ? left : 0（每行首元素 pred=0）
//! - Vertical:   pred = y>0 ? top  : 0（仅绝对首行 y==0 取 0；条带 y_start>0
//!   时上文行已存在，pred 取真实上邻）
//! - Average:    pred = (left + top) 向零 /2（缺失邻居取 0）
//! - DC:         pred = (left+top+top_left+top_right) 向零 /4（缺失邻居取 0）
//!
//! 向零除法（i32）实现：`x / (1<<k)` 向零 = `(x + (x<0 ? (1<<k)-1 : 0)) >> k`，
//! 与 Rust 整数除法的截断舍入逐位一致（SIMD 只有算术右移 floor 语义，
//! 故用「负值加偏置」修正）。

use super::simd::has_avx2;

/// 对 components==1 平面执行 SIMD 预测。返回 true 表示已写入 `out`；
/// 返回 false 表示该模式未向量化，调用方回退标量 `predict_at`。
///
/// `out` 为**紧凑**缓冲：长度 = (y_end - y_start) * width，与
/// `apply_prediction_band_into` 语义一致（`apply_prediction_range_into` 在
/// y_start=0 时紧凑 == 整帧，可直接复用）。
#[inline]
pub fn predict_plane_avx2(
    pixels: &[i32],
    out: &mut [i32],
    width: usize,
    mode: u8,
    y_start: usize,
    y_end: usize,
) -> bool {
    // mode 取值与 core::domain::PredictionMode 的 discriminants 一致：
    // 1=Horizontal, 2=Vertical, 3=Average, 4=DC。
    const MODE_H: u8 = 1;
    const MODE_V: u8 = 2;
    const MODE_AVG: u8 = 3;
    const MODE_DC: u8 = 4;
    if width == 0 || y_end <= y_start {
        return true;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            match mode {
                MODE_H => {
                    // SAFETY: avx2 已检测；kernel 在切片边界内操作
                    unsafe { predict_horizontal_avx2(pixels, out, width, y_start, y_end) };
                    return true;
                }
                MODE_V => {
                    unsafe { predict_vertical_avx2(pixels, out, width, y_start, y_end) };
                    return true;
                }
                MODE_AVG => {
                    unsafe { predict_average_avx2(pixels, out, width, y_start, y_end) };
                    return true;
                }
                MODE_DC => {
                    unsafe { predict_dc_avx2(pixels, out, width, y_start, y_end) };
                    return true;
                }
                _ => return false,
            }
        }
    }
    let _ = (pixels, out, width, mode, y_start, y_end);
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn predict_horizontal_avx2(
    pixels: &[i32],
    out: &mut [i32],
    width: usize,
    y_start: usize,
    y_end: usize,
) {
    use std::arch::x86_64::*;
    for y in y_start..y_end {
        let base_in = y * width;
        let base_out = (y - y_start) * width;
        // x=0：pred=0，直接拷贝
        *out.get_unchecked_mut(base_out) = *pixels.get_unchecked(base_in);
        let mut x = 1usize;
        while x + 8 <= width {
            let cur = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x).cast::<__m256i>());
            let left = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - 1).cast::<__m256i>());
            let r = _mm256_sub_epi32(cur, left);
            _mm256_storeu_si256(out.as_mut_ptr().add(base_out + x).cast::<__m256i>(), r);
            x += 8;
        }
        while x < width {
            *out.get_unchecked_mut(base_out + x) =
                *pixels.get_unchecked(base_in + x) - *pixels.get_unchecked(base_in + x - 1);
            x += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn predict_vertical_avx2(
    pixels: &[i32],
    out: &mut [i32],
    width: usize,
    y_start: usize,
    y_end: usize,
) {
    use std::arch::x86_64::*;
    let mut y = y_start;
    // 绝对首行（y==0）：pred=0，直接拷贝。仅当 y_start==0 时出现。
    if y == 0 {
        let mut x = 0usize;
        while x + 8 <= width {
            let cur = _mm256_loadu_si256(pixels.as_ptr().add(x).cast::<__m256i>());
            _mm256_storeu_si256(out.as_mut_ptr().add(x).cast::<__m256i>(), cur);
            x += 8;
        }
        while x < width {
            *out.get_unchecked_mut(x) = *pixels.get_unchecked(x);
            x += 1;
        }
        y = 1;
    }
    for y in y..y_end {
        let base_in = y * width;
        let base_out = (y - y_start) * width;
        let mut x = 0usize;
        while x + 8 <= width {
            let cur = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x).cast::<__m256i>());
            let top =
                _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - width).cast::<__m256i>());
            let r = _mm256_sub_epi32(cur, top);
            _mm256_storeu_si256(out.as_mut_ptr().add(base_out + x).cast::<__m256i>(), r);
            x += 8;
        }
        while x < width {
            *out.get_unchecked_mut(base_out + x) =
                *pixels.get_unchecked(base_in + x) - *pixels.get_unchecked(base_in + x - width);
            x += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn predict_average_avx2(
    pixels: &[i32],
    out: &mut [i32],
    width: usize,
    y_start: usize,
    y_end: usize,
) {
    use std::arch::x86_64::*;
    let one = _mm256_set1_epi32(1);
    for y in y_start..y_end {
        let base_in = y * width;
        let base_out = (y - y_start) * width;
        let has_top = y > 0;
        // x=0：left=0 → pred = top / 2（向零）
        {
            let top0 = if has_top {
                *pixels.get_unchecked(base_in - width)
            } else {
                0
            };
            let sum = top0;
            let pred = (sum + ((sum >> 31) & 1)) >> 1;
            *out.get_unchecked_mut(base_out) = *pixels.get_unchecked(base_in) - pred;
        }
        let mut x = 1usize;
        while x + 8 <= width {
            let cur = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x).cast::<__m256i>());
            let left = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - 1).cast::<__m256i>());
            let top = if has_top {
                _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - width).cast::<__m256i>())
            } else {
                _mm256_setzero_si256()
            };
            let sum = _mm256_add_epi32(left, top);
            let bias = _mm256_and_si256(_mm256_srai_epi32(sum, 31), one);
            let pred = _mm256_srai_epi32(_mm256_add_epi32(sum, bias), 1);
            let r = _mm256_sub_epi32(cur, pred);
            _mm256_storeu_si256(out.as_mut_ptr().add(base_out + x).cast::<__m256i>(), r);
            x += 8;
        }
        while x < width {
            let left = *pixels.get_unchecked(base_in + x - 1);
            let top = if has_top {
                *pixels.get_unchecked(base_in + x - width)
            } else {
                0
            };
            let sum = left + top;
            let pred = (sum + ((sum >> 31) & 1)) >> 1;
            *out.get_unchecked_mut(base_out + x) = *pixels.get_unchecked(base_in + x) - pred;
            x += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn predict_dc_avx2(
    pixels: &[i32],
    out: &mut [i32],
    width: usize,
    y_start: usize,
    y_end: usize,
) {
    use std::arch::x86_64::*;
    let three = _mm256_set1_epi32(3);
    for y in y_start..y_end {
        let base_in = y * width;
        let base_out = (y - y_start) * width;
        let has_top = y > 0;
        // 边界列 x=0 与 x=width-1 单独处理（top_right / left / top_left 边界）
        let x_last = width.saturating_sub(1);
        for &x_edge in &[0usize, x_last] {
            if x_edge >= width {
                continue;
            }
            let left = if x_edge > 0 {
                *pixels.get_unchecked(base_in + x_edge - 1)
            } else {
                0
            };
            let top = if has_top {
                *pixels.get_unchecked(base_in + x_edge - width)
            } else {
                0
            };
            let top_left = if x_edge > 0 && has_top {
                *pixels.get_unchecked(base_in + x_edge - width - 1)
            } else {
                0
            };
            let top_right = if x_edge + 1 < width && has_top {
                *pixels.get_unchecked(base_in + x_edge - width + 1)
            } else {
                0
            };
            let sum = left + top + top_left + top_right;
            let pred = (sum + ((sum >> 31) & 3)) >> 2;
            *out.get_unchecked_mut(base_out + x_edge) =
                *pixels.get_unchecked(base_in + x_edge) - pred;
        }
        // 内部列 x in 1..width-1：四邻居齐全（当 has_top 时）
        let mut x = 1usize;
        while x + 8 <= width.saturating_sub(1) {
            let cur = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x).cast::<__m256i>());
            let left = _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - 1).cast::<__m256i>());
            let sum = if has_top {
                let top =
                    _mm256_loadu_si256(pixels.as_ptr().add(base_in + x - width).cast::<__m256i>());
                let top_left = _mm256_loadu_si256(
                    pixels
                        .as_ptr()
                        .add(base_in + x - width - 1)
                        .cast::<__m256i>(),
                );
                let top_right = _mm256_loadu_si256(
                    pixels
                        .as_ptr()
                        .add(base_in + x - width + 1)
                        .cast::<__m256i>(),
                );
                let s0 = _mm256_add_epi32(left, top);
                let s1 = _mm256_add_epi32(top_left, top_right);
                _mm256_add_epi32(s0, s1)
            } else {
                // 首行（y==0）：top/top_left/top_right = 0，仅 left 参与
                left
            };
            let bias = _mm256_and_si256(_mm256_srai_epi32(sum, 31), three);
            let pred = _mm256_srai_epi32(_mm256_add_epi32(sum, bias), 2);
            let r = _mm256_sub_epi32(cur, pred);
            _mm256_storeu_si256(out.as_mut_ptr().add(base_out + x).cast::<__m256i>(), r);
            x += 8;
        }
        while x < width.saturating_sub(1) {
            let left = *pixels.get_unchecked(base_in + x - 1);
            let top = if has_top {
                *pixels.get_unchecked(base_in + x - width)
            } else {
                0
            };
            let top_left = if has_top {
                *pixels.get_unchecked(base_in + x - width - 1)
            } else {
                0
            };
            let top_right = if has_top {
                *pixels.get_unchecked(base_in + x - width + 1)
            } else {
                0
            };
            let sum = left + top + top_left + top_right;
            let pred = (sum + ((sum >> 31) & 3)) >> 2;
            *out.get_unchecked_mut(base_out + x) = *pixels.get_unchecked(base_in + x) - pred;
            x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::core::domain::PredictionMode;
    use crate::crf::core::prediction::intra::predict_at;

    /// 标量参考：components==1 的条带预测（逐像素 predict_at）。
    fn scalar_predict_band(
        pixels: &[i32],
        out: &mut [i32],
        width: usize,
        mode: PredictionMode,
        y_start: usize,
        y_end: usize,
    ) {
        for y in y_start..y_end {
            let base_in = y * width;
            let base_out = (y - y_start) * width;
            for x in 0..width {
                let idx = base_in + x;
                let predicted = predict_at(pixels, idx, x, y, width, 1, width, mode);
                out[base_out + x] = pixels[idx] - predicted;
            }
        }
    }

    /// SIMD 与标量参考逐位一致（覆盖非对齐尺寸、条带起点、首行/尾行边界）。
    #[test]
    fn test_predict_plane_matches_scalar() {
        let mut state: u64 = 0x5EED_1234_ABCD_EF01;
        let next = |state: &mut u64| -> i32 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*state >> 33) as i32 % 512) - 256
        };

        let cases: [(usize, usize, usize, usize); 9] = [
            (1, 1, 0, 1),
            (2, 3, 0, 3),
            (7, 5, 0, 5),
            (8, 8, 0, 8),
            (16, 9, 0, 9),
            (33, 17, 0, 17),
            (37, 21, 5, 21),
            (100, 3, 0, 3),
            (65, 65, 0, 65),
        ];

        for (width, height, y_start, y_end) in cases {
            let n = width * height;
            let pixels: Vec<i32> = (0..n).map(|_| next(&mut state)).collect();
            let expected_len = (y_end - y_start) * width;
            for mode in [
                PredictionMode::Horizontal,
                PredictionMode::Vertical,
                PredictionMode::Average,
                PredictionMode::DC,
            ] {
                let mut expected = vec![0i32; expected_len];
                scalar_predict_band(&pixels, &mut expected, width, mode, y_start, y_end);

                let mut actual = vec![0i32; expected_len];
                let ok =
                    predict_plane_avx2(&pixels, &mut actual, width, mode as u8, y_start, y_end);
                if ok {
                    assert_eq!(
                        actual, expected,
                        "mode={mode:?} w={width} h={height} ys={y_start} 对拍失败"
                    );
                }
                // ok=false（非 x86_64 或非 AVX2）时跳过，标量路径由 intra.rs 兜底。
            }
        }
    }

    /// 向零除法辅助的正确性：负值截断（/2、/4）逐位一致。
    #[test]
    fn test_trunc_div_bias_formula() {
        for &x in &[
            -9, -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
        ] {
            let div2 = (x + ((x >> 31) & 1)) >> 1;
            assert_eq!(div2, x / 2, "div2 x={x}");
            let div4 = (x + ((x >> 31) & 3)) >> 2;
            assert_eq!(div4, x / 4, "div4 x={x}");
        }
    }
}
