//! SSIM（结构相似性指数）——质量标定必报指标（first-frame-optimization-plan §6.2）
//!
//! 标准 Wang et al. 2004 实现：11×11 高斯窗口（σ=1.5）、8-bit 灰度、
//! C1=(0.01·L)²、C2=(0.03·L)²，返回全图 SSIM map 的算术平均。
//! 与 PSNR 同为静态图客观质量指标；不替代主观视觉检查。

use crate::crf::core::domain::ImageData;

/// SSIM 窗口尺寸（Wang et al. 2004 标准 11×11）
const SSIM_WIN: usize = 11;
/// 高斯窗口标准差
const SSIM_SIGMA: f64 = 1.5;
/// 动态范围（8-bit）
const L: f64 = 255.0;

/// 计算两帧的全局平均 SSIM（要求尺寸一致；支持 RGB / Gray 输入）。
pub fn ssim(a: &ImageData, b: &ImageData) -> f64 {
    let w = a.width as usize;
    let h = a.height as usize;
    if w == 0 || h == 0 || b.width != a.width || b.height != a.height {
        return f64::NAN;
    }
    let ga = to_gray(a, w, h);
    let gb = to_gray(b, w, h);
    ssim_gray(&ga, &gb, w, h)
}

/// 转灰度（RGB → 亮度；单分量直接取用）
fn to_gray(img: &ImageData, w: usize, h: usize) -> Vec<f64> {
    let comp = img.color_format.component_count();
    let mut out = vec![0.0; w * h];
    for (i, slot) in out.iter_mut().enumerate() {
        if comp >= 3 {
            let r = img.pixels[i * comp] as f64;
            let g = img.pixels[i * comp + 1] as f64;
            let b = img.pixels[i * comp + 2] as f64;
            *slot = 0.299 * r + 0.587 * g + 0.114 * b;
        } else {
            *slot = img.pixels[i * comp] as f64;
        }
    }
    out
}

/// 一维高斯核（归一化）
fn gaussian_kernel() -> Vec<f64> {
    let c = (SSIM_WIN / 2) as f64;
    let mut k: Vec<f64> = (0..SSIM_WIN)
        .map(|i| {
            let x = i as f64 - c;
            (-(x * x) / (2.0 * SSIM_SIGMA * SSIM_SIGMA)).exp()
        })
        .collect();
    let sum: f64 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= sum;
    }
    k
}

/// 可分离高斯模糊（边界 replicate/clamp）
fn blur(src: &[f64], w: usize, h: usize, kernel: &[f64]) -> Vec<f64> {
    let r = SSIM_WIN / 2;
    let mut tmp = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (ki, &kv) in kernel.iter().enumerate() {
                let xx = (x + ki).saturating_sub(r).min(w - 1);
                s += src[y * w + xx] * kv;
            }
            tmp[y * w + x] = s;
        }
    }
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (ki, &kv) in kernel.iter().enumerate() {
                let yy = (y + ki).saturating_sub(r).min(h - 1);
                s += tmp[yy * w + x] * kv;
            }
            out[y * w + x] = s;
        }
    }
    out
}

/// 灰度 SSIM：局部均值/方差/协方差 → SSIM map 平均
fn ssim_gray(ga: &[f64], gb: &[f64], w: usize, h: usize) -> f64 {
    let k = gaussian_kernel();
    let n = w * h;
    let mut ga2 = vec![0.0; n];
    let mut gb2 = vec![0.0; n];
    let mut gab = vec![0.0; n];
    for i in 0..n {
        ga2[i] = ga[i] * ga[i];
        gb2[i] = gb[i] * gb[i];
        gab[i] = ga[i] * gb[i];
    }
    let mu_a = blur(ga, w, h, &k);
    let mu_b = blur(gb, w, h, &k);
    let mu_a2 = blur(&ga2, w, h, &k);
    let mu_b2 = blur(&gb2, w, h, &k);
    let mu_ab = blur(&gab, w, h, &k);

    let c1 = (0.01 * L) * (0.01 * L);
    let c2 = (0.03 * L) * (0.03 * L);
    let mut sum = 0.0;
    for i in 0..n {
        let ma = mu_a[i];
        let mb = mu_b[i];
        let sa = mu_a2[i] - ma * ma;
        let sb = mu_b2[i] - mb * mb;
        let sab = mu_ab[i] - ma * mb;
        let num = (2.0 * ma * mb + c1) * (2.0 * sab + c2);
        let den = (ma * ma + mb * mb + c1) * (sa + sb + c2);
        sum += num / den;
    }
    sum / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::core::domain::{ColorFormat, ImageData};

    fn gray_img(w: u16, h: u16, v: &[i32]) -> ImageData {
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Gray,
            pixels: v.to_vec(),
        }
    }

    #[test]
    fn test_identical_images_ssim_one() {
        let a = gray_img(32, 32, &(0..1024).map(|i| (i % 256) as i32).collect::<Vec<_>>());
        let s = ssim(&a, &a);
        assert!((s - 1.0).abs() < 1e-9, "identical SSIM 应为 1，实际 {s}");
    }

    #[test]
    fn test_noise_lowers_ssim() {
        let base: Vec<i32> = (0..1024).map(|i| ((i * 7) % 256) as i32).collect();
        let noisy: Vec<i32> = base.iter().map(|&v| (v + 40).min(255)).collect();
        let a = gray_img(32, 32, &base);
        let b = gray_img(32, 32, &noisy);
        let s = ssim(&a, &b);
        assert!(s < 1.0 && s > -1.0, "含噪 SSIM 应在 (-1,1)，实际 {s}");
    }

    #[test]
    fn test_size_mismatch_nan() {
        let a = gray_img(16, 16, &vec![0; 256]);
        let b = gray_img(32, 32, &vec![0; 1024]);
        assert!(ssim(&a, &b).is_nan());
    }
}
