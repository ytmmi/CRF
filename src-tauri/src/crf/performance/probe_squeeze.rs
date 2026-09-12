//! JXL Modular Squeeze 小波变换收益探针
//!
//! **背景**：JPEG XL Modular 的 "Squeeze" 是类 Haar 的可逆小波：每次把一维
//! 减半为「平均（低通）+ 差分减趋势（高通）」，交替水平/垂直形成金字塔子带
//! （LL/LH/HL/HH）。CRF 现有路径未使用任何小波/子带分解。本探针测量 squeeze
//! 后子带独立熵编码相对直接编码的字节收益。
//!
//! **算法来源**：libjxl `transform/enc_squeeze.cc`（`FwdHSqueeze`/`FwdVSqueeze`）
//! 与 `transform/squeeze.h`（`SmoothTendency`、`AVERAGE`）。逐位复刻。
//!
//! **探针口径**（每差分帧，RCT 残差 = `rct_forward(frame − golden)`，分离 3 分量）：
//! - **无 squeeze**：3 分量各自 RLE+Golomb 累加；
//! - **squeeze**：3 分量各自多级 squeeze → 各子带 RLE+Golomb 累加；
//! - **CRF 现有**：`encode_frame_adaptive`（交织，自适应预测 + CABAC/RLE/DCT）。
//!
//! **判定**：若 squeeze 显著优于无 squeeze（≥3%），则子带分解在 CRF 残差上有价值。
//! 零码流改动、零外部数据集。
//!
//! CLI：`--probe-squeeze [root]`（默认 `E:\CRF\test\png`）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

/// squeeze 级数。`CRF_PROBE_SQUEEZE_LEVELS` 可覆盖（默认 2）。
fn squeeze_levels() -> usize {
    std::env::var("CRF_PROBE_SQUEEZE_LEVELS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(2)
}

/// 每组参与探针的最大帧数（0 = 不限）。`CRF_PROBE_SQUEEZE_MAX` 可覆盖。
fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_SQUEEZE_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// libjxl `AVERAGE(X,Y) = (X + Y + (X>Y ? 1 : 0)) >> 1`。
#[inline]
fn average(a: i64, b: i64) -> i64 {
    (a + b + if a > b { 1 } else { 0 }) >> 1
}

/// libjxl `SmoothTendency(B, a, n)`：估计 C−D 的平滑趋势（避免 ringing）。
#[inline]
fn smooth_tendency(b: i64, a: i64, n: i64) -> i64 {
    let mut diff = 0i64;
    if b >= a && a >= n {
        diff = (4 * b - 3 * n - a + 6) / 12;
        if diff - (diff & 1) > 2 * (b - a) {
            diff = 2 * (b - a) + 1;
        }
        if diff + (diff & 1) > 2 * (a - n) {
            diff = 2 * (a - n);
        }
    } else if b <= a && a <= n {
        diff = (4 * b - 3 * n - a - 6) / 12;
        if diff + (diff & 1) < 2 * (b - a) {
            diff = 2 * (b - a) - 1;
        }
        if diff - (diff & 1) < 2 * (a - n) {
            diff = 2 * (a - n);
        }
    }
    diff
}

/// 水平 squeeze（libjxl `FwdHSqueeze`）。输入 w×h，返回 (avg, residual)。
fn fwd_h_squeeze(input: &[i32], w: usize, h: usize) -> (Vec<i32>, Vec<i32>) {
    let out_w = w.div_ceil(2);
    let res_w = w - out_w;
    let mut avg = vec![0i32; out_w * h];
    let mut res = vec![0i32; res_w * h];
    for y in 0..h {
        let row = &input[y * w..(y + 1) * w];
        for x in 0..res_w {
            let a = row[x * 2] as i64;
            let b = row[x * 2 + 1] as i64;
            let av = average(a, b);
            avg[y * out_w + x] = av as i32;
            let diff = a - b;
            let next_avg = if x + 1 < res_w {
                average(row[x * 2 + 2] as i64, row[x * 2 + 3] as i64)
            } else if w & 1 != 0 {
                row[x * 2 + 2] as i64
            } else {
                av
            };
            let left = if x > 0 { row[x * 2 - 1] as i64 } else { av };
            let tendency = smooth_tendency(left, av, next_avg);
            res[y * res_w + x] = (diff - tendency) as i32;
        }
        if w & 1 != 0 {
            avg[y * out_w + out_w - 1] = row[(out_w - 1) * 2];
        }
    }
    (avg, res)
}

/// 垂直 squeeze（libjxl `FwdVSqueeze`）。输入 w×h，返回 (avg, residual)。
fn fwd_v_squeeze(input: &[i32], w: usize, h: usize) -> (Vec<i32>, Vec<i32>) {
    let out_h = h.div_ceil(2);
    let res_h = h - out_h;
    let mut avg = vec![0i32; w * out_h];
    let mut res = vec![0i32; w * res_h];
    for y in 0..res_h {
        let row0 = &input[(y * 2) * w..(y * 2 + 1) * w];
        let row1 = &input[(y * 2 + 1) * w..(y * 2 + 2) * w];
        for x in 0..w {
            let a = row0[x] as i64;
            let b = row1[x] as i64;
            let av = average(a, b);
            avg[y * w + x] = av as i32;
            let diff = a - b;
            let next_avg = if y + 1 < res_h {
                let r2 = &input[(y * 2 + 2) * w..(y * 2 + 3) * w];
                let r3 = &input[(y * 2 + 3) * w..(y * 2 + 4) * w];
                average(r2[x] as i64, r3[x] as i64)
            } else if h & 1 != 0 {
                input[(y * 2 + 2) * w + x] as i64
            } else {
                av
            };
            // libjxl: top = p_in[x - onerow] = 上一行 (2y-1) 的 x
            let top = if y > 0 {
                input[(y * 2 - 1) * w + x] as i64
            } else {
                av
            };
            let tendency = smooth_tendency(top, av, next_avg);
            res[y * w + x] = (diff - tendency) as i32;
        }
    }
    if h & 1 != 0 {
        let y = out_h - 1;
        let row = &input[(y * 2) * w..(y * 2 + 1) * w];
        avg[y * w..(y + 1) * w].copy_from_slice(row);
    }
    (avg, res)
}

/// 一级 2D squeeze：LL(低频) + LH(垂直高频) + HL(水平高频) + HH(对角)。
fn squeeze2d(input: &[i32], w: usize, h: usize) -> (Vec<i32>, Vec<i32>, Vec<i32>, Vec<i32>) {
    let (l, hr) = fwd_h_squeeze(input, w, h);
    let w2 = w.div_ceil(2);
    let hr_w = w - w2;
    let (ll, lv) = fwd_v_squeeze(&l, w2, h);
    let (hl, hh) = fwd_v_squeeze(&hr, hr_w, h);
    (ll, lv, hl, hh)
}

/// 多级 squeeze 金字塔：返回 (子带数据, 宽, 高) 列表（高频在前，低频在末尾）。
pub(crate) fn squeeze_pyramid(
    input: &[i32],
    w: usize,
    h: usize,
    levels: usize,
) -> Vec<(Vec<i32>, usize, usize)> {
    let mut bands = Vec::new();
    let (mut cur, mut cw, mut ch) = (input.to_vec(), w, h);
    for _ in 0..levels {
        if cw < 2 || ch < 2 {
            break;
        }
        let (ll, lv, hl, hh) = squeeze2d(&cur, cw, ch);
        let w2 = cw.div_ceil(2);
        let h2 = ch.div_ceil(2);
        let hr_w = cw - w2;
        let hr_h = ch - h2;
        if !lv.is_empty() {
            bands.push((lv, w2, hr_h)); // LH
        }
        if !hl.is_empty() {
            bands.push((hl, hr_w, h2)); // HL
        }
        if !hh.is_empty() {
            bands.push((hh, hr_w, hr_h)); // HH
        }
        cur = ll;
        cw = w2;
        ch = h2;
    }
    bands.push((cur, cw, ch)); // LL
    bands
}

/// 分离交织残差为 3 个平面分量。
pub(crate) fn split_components(pixels: &[i32], components: usize) -> Vec<Vec<i32>> {
    let n = pixels.len() / components;
    let mut planes = vec![vec![0i32; n]; components];
    for (i, plane) in planes.iter_mut().enumerate() {
        for (j, v) in plane.iter_mut().enumerate() {
            *v = pixels[j * components + i];
        }
    }
    planes
}

/// 单帧探针结果。
struct FrameStat {
    no_squeeze: usize,
    squeeze: usize,
    crf: usize,
}

/// 运行探针。`root` 为 test/png 根目录或单个组目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();
    if load_frames(root).map(|f| f.len() >= 2).unwrap_or(false) {
        groups = vec![std::path::PathBuf::from(root)];
    }

    let levels = squeeze_levels();
    let limit = group_frame_limit();
    println!("=== JXL Squeeze 小波变换收益探针 ===");
    println!("root: {root}");
    println!(
        "squeeze 级数: {levels}  |  每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: RCT 残差 → 分离 3 分量 → [多级 squeeze] → 子带 RLE+Golomb | CRF adaptive\n");

    let mut all: Vec<FrameStat> = Vec::new();

    for dir in &groups {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        if limit > 0 && frames.len() > limit {
            frames.truncate(limit);
        }
        let n = frames.len();
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let golden = &frames[0].pixels;
        let width = frames[0].width as usize;
        let height = frames[0].height as usize;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        let stats: Vec<FrameStat> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<FrameStat, String> {
                let frame = &frames[i];
                let mut diff_rgb = vec![0i32; golden.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let residuals =
                    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;

                // 无 squeeze：3 分量各自 RLE+Golomb
                let planes = split_components(&residuals, components);
                let mut no_squeeze = 0usize;
                for plane in &planes {
                    let (buf, _) =
                        encode_frame_rle_golomb_adaptive(plane).map_err(|e| e.to_string())?;
                    no_squeeze += buf.len();
                }

                // squeeze：各分量多级金字塔 → 子带 RLE+Golomb
                let mut squeeze = 0usize;
                for plane in &planes {
                    for (band, _bw, _bh) in squeeze_pyramid(plane, width, height, levels) {
                        if band.is_empty() {
                            continue;
                        }
                        let (buf, _) =
                            encode_frame_rle_golomb_adaptive(&band).map_err(|e| e.to_string())?;
                        squeeze += buf.len();
                    }
                }

                // CRF 现有完整自适应编码（交织）
                let eff = ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: residuals,
                };
                let out = encode_frame_adaptive(
                    &eff,
                    CompressionType::GolombRice,
                    8,
                    false,
                    FrameQuant::lossless(),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;

                Ok(FrameStat {
                    no_squeeze,
                    squeeze,
                    crf: out.data.len(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let s = |f: fn(&FrameStat) -> usize| stats.iter().map(f).sum::<usize>();
        let (ns, sq, crf) = (s(|x| x.no_squeeze), s(|x| x.squeeze), s(|x| x.crf));
        let pct = |a: usize, b: usize| -> f64 {
            if b == 0 {
                0.0
            } else {
                (a as f64 - b as f64) / b as f64 * 100.0
            }
        };
        println!(
            "组 {:<12} ({} 帧): 无 squeeze {} | squeeze {} ({:+.1}%) | CRF {} ({:+.1}% vs squeeze)",
            name,
            n - 1,
            ns,
            sq,
            pct(sq, ns),
            crf,
            pct(crf, sq),
        );
        all.extend(stats);
    }

    println!("\n=== 汇总 ===");
    if all.is_empty() {
        println!("(无有效组：需 ≥2 帧且 RGB)");
        return Ok(());
    }
    let sum = |f: fn(&FrameStat) -> usize| all.iter().map(f).sum::<usize>();
    let (ns, sq, crf) = (sum(|x| x.no_squeeze), sum(|x| x.squeeze), sum(|x| x.crf));
    let pct = |a: usize, b: usize| -> f64 {
        if b == 0 {
            0.0
        } else {
            (a as f64 - b as f64) / b as f64 * 100.0
        }
    };
    println!("帧数 {}", all.len());
    println!("无 squeeze: {ns}");
    println!("squeeze:    {sq}  ({:+.1}% vs 无 squeeze)", pct(sq, ns));
    println!("CRF 现有:   {crf}  ({:+.1}% vs squeeze)", pct(crf, sq));

    let gain = pct(sq, ns);
    println!(
        "\n判定：squeeze 相对无 squeeze {:+.2}% —— {}",
        gain,
        if gain <= -3.0 {
            "Squeeze 有效（≥3%）"
        } else {
            "Squeeze 无显著增益（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 水平 squeeze 逆变换（libjxl InvHSqueeze 标量版）。
    fn undo_h_squeeze(avg: &[i32], res: &[i32], out_w: usize, res_w: usize, h: usize) -> Vec<i32> {
        let w = out_w + res_w;
        let mut out = vec![0i32; w * h];
        for y in 0..h {
            for x in 0..res_w {
                let diff_minus_tendency = res[y * res_w + x] as i64;
                let a = avg[y * out_w + x] as i64;
                let next_avg = if x + 1 < out_w {
                    avg[y * out_w + x + 1] as i64
                } else {
                    a
                };
                let left = if x > 0 {
                    out[y * w + x * 2 - 1] as i64
                } else {
                    a
                };
                let tendency = smooth_tendency(left, a, next_avg);
                let diff = diff_minus_tendency + tendency;
                let va = a + diff / 2;
                out[y * w + x * 2] = va as i32;
                out[y * w + x * 2 + 1] = (va - diff) as i32;
            }
            if w & 1 != 0 {
                out[y * w + w - 1] = avg[y * out_w + out_w - 1];
            }
        }
        out
    }

    #[test]
    fn h_squeeze_roundtrip() {
        let w = 9usize;
        let h = 3usize;
        let input: Vec<i32> = (0..w * h)
            .map(|i| ((i * 7 + 3) % 251) as i32 - 125)
            .collect();
        let (avg, res) = fwd_h_squeeze(&input, w, h);
        let out_w = w.div_ceil(2);
        let res_w = w - out_w;
        let restored = undo_h_squeeze(&avg, &res, out_w, res_w, h);
        assert_eq!(restored, input, "水平 squeeze 往返失败");
    }

    #[test]
    fn smooth_tendency_flat_is_zero() {
        assert_eq!(smooth_tendency(10, 10, 10), 0);
        assert_eq!(smooth_tendency(0, 0, 0), 0);
    }

    #[test]
    fn squeeze_pyramid_shrinks_dimensions() {
        let w = 32usize;
        let h = 24usize;
        let input = vec![5i32; w * h];
        let bands = squeeze_pyramid(&input, w, h, 2);
        assert!(!bands.is_empty());
        let (_, lw, lh) = bands.last().unwrap();
        assert_eq!(*lw, 8);
        assert_eq!(*lh, 6);
    }
}
