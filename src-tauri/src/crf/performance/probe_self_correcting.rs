//! JXL Modular 自校正/加权预测器（weighted::State）收益探针
//!
//! **背景**：JPEG XL Modular 的 "weighted predictor"（`weighted::State`，即
//! 自校正预测器）维护 **4 个子预测器**的逐像素历史误差，用误差加权平均得到
//! 预测值，并在三邻居误差同号时跳过 clamp。CRF 现有 11 种预测器（MED/Paeth/
//! DC/…）均为**无状态**静态预测器，缺少误差反馈型。本探针测量自校正预测器
//! 相对 CRF 现有预测器的残差熵编码字节。
//!
//! **算法来源**：libjxl `lib/jxl/modular/encoding/context_predict.h`
//! `weighted::State::Predict` / `UpdateErrors` / `ErrorWeight` / `WeightedAverage`，
//! 参数取 `PredictorMode(1)`（lossless8 默认）。
//!
//! **探针口径**（每差分帧，RCT 残差 = `rct_forward(frame − golden)`）：
//! - **MED + RLE**：`apply_prediction_into(MED)` → RLE+Golomb；
//! - **Paeth + RLE**：`apply_prediction_into(Paeth)` → RLE+Golomb；
//! - **WP + RLE**：自校正预测器 → RLE+Golomb；
//! - **CRF 现有**：`encode_frame_adaptive`（自适应预测竞争 + CABAC/RLE/DCT）。
//!
//! **判定**：若「WP + RLE」显著优于「MED/Paeth + RLE」（≥3%），则自校正预测器
//! 在 CRF 残差上有价值。零码流改动、零外部数据集。
//!
//! CLI：`--probe-self-correcting [root]`（默认 `E:\CRF\test\png`）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData, PredictionMode};
use crate::crf::core::prediction::intra::apply_prediction_into;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

/// 每组参与探针的最大帧数（0 = 不限）。`CRF_PROBE_WP_MAX` 可覆盖。
fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_WP_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// libjxl `weighted::State::divlookup`：近似 2^24/(i+1)，避免除法。
const DIVLOOKUP: [u32; 64] = [
    16777216, 8388608, 5592405, 4194304, 3355443, 2796202, 2396745, 2097152, 1864135, 1677721,
    1525201, 1398101, 1290555, 1198372, 1118481, 1048576, 986895, 932067, 883011, 838860, 798915,
    762600, 729444, 699050, 671088, 645277, 621378, 599186, 578524, 559240, 541200, 524288, 508400,
    493447, 479349, 466033, 453438, 441505, 430185, 419430, 409200, 399457, 390167, 381300, 372827,
    364722, 356962, 349525, 342392, 335544, 328965, 322638, 316551, 310689, 305040, 299593, 294337,
    289262, 284359, 279620, 275036, 270600, 266305, 262144,
];

/// floor(log2(x))，要求 x > 0。
#[inline]
fn floor_log2_nonzero(x: u64) -> u32 {
    63 - x.leading_zeros()
}

/// JXL 加权/自校正预测器状态（逐分量独立）。
struct WpState {
    /// 两行环形缓冲：(width+2) 槽位 × 2 行。
    error: Vec<i32>,
    pred_errors: [Vec<u32>; 4],
    prediction: [i64; 4],
    pred: i64,
    width: usize,
    // 参数：PredictorMode(1)（lossless8 默认）
    w: [u32; 4],
    p1c: i64,
    p2c: i64,
    p3ca: i64,
    p3cb: i64,
    p3cc: i64,
    p3cd: i64,
    p3ce: i64,
}

impl WpState {
    fn new(width: usize) -> Self {
        let n = (width + 2) * 2;
        Self {
            error: vec![0; n],
            pred_errors: [vec![0; n], vec![0; n], vec![0; n], vec![0; n]],
            prediction: [0; 4],
            pred: 0,
            width,
            w: [0xd, 0xc, 0xc, 0xb],
            p1c: 8,
            p2c: 8,
            p3ca: 4,
            p3cb: 0,
            p3cc: 3,
            p3cd: 23,
            p3ce: 2,
        }
    }

    /// 近似 `4 + (maxweight<<24)/(x+1)`（避免除法）。
    #[inline]
    fn error_weight(&self, x: u64, maxweight: u32) -> u32 {
        let mut shift = floor_log2_nonzero(x + 1) as i32 - 5;
        if shift < 0 {
            shift = 0;
        }
        let idx = ((x >> shift) as usize).min(63);
        4 + ((maxweight * DIVLOOKUP[idx]) >> shift)
    }

    /// 加权平均（权重和 ≥ 16），避免除法。
    #[inline]
    fn weighted_average(&self, p: &[i64; 4], mut w: [u32; 4]) -> i64 {
        let weight_sum0: u32 = w.iter().sum();
        let log_weight = floor_log2_nonzero(weight_sum0 as u64);
        let mut weight_sum = 0u32;
        for wi in w.iter_mut() {
            *wi >>= log_weight - 4;
            weight_sum += *wi;
        }
        let mut sum: i64 = (weight_sum >> 1) as i64 - 1;
        for (pi, wi) in p.iter().zip(w.iter()) {
            sum += pi * *wi as i64;
        }
        (sum * DIVLOOKUP[(weight_sum - 1) as usize] as i64) >> 24
    }

    /// 计算 (x,y) 处预测值。邻居：n=top、w=left、ne=topright、nw=topleft、nn=toptop。
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn predict(&mut self, x: usize, y: usize, n: i64, w: i64, ne: i64, nw: i64, nn: i64) -> i32 {
        let xsize = self.width;
        let cur_row = if y & 1 != 0 { 0 } else { xsize + 2 };
        let prev_row = if y & 1 != 0 { xsize + 2 } else { 0 };
        let pos_n = prev_row + x;
        let pos_ne = if x < xsize - 1 { pos_n + 1 } else { pos_n };
        let pos_nw = if x > 0 { pos_n - 1 } else { pos_n };

        let mut weights = [0u32; 4];
        for (i, wslot) in weights.iter_mut().enumerate() {
            let sum = self.pred_errors[i][pos_n]
                + self.pred_errors[i][pos_ne]
                + self.pred_errors[i][pos_nw];
            *wslot = self.error_weight(sum as u64, self.w[i]);
        }

        let n = n << 3;
        let w = w << 3;
        let ne = ne << 3;
        let nw = nw << 3;
        let nn = nn << 3;

        let te_w = if x == 0 {
            0i64
        } else {
            self.error[cur_row + x - 1] as i64
        };
        let te_n = self.error[pos_n] as i64;
        let te_nw = self.error[pos_nw] as i64;
        let sum_wn = te_n + te_w;
        let te_ne = self.error[pos_ne] as i64;

        self.prediction[0] = w + ne - n;
        self.prediction[1] = n - (((sum_wn + te_ne) * self.p1c) >> 5);
        self.prediction[2] = w - (((sum_wn + te_nw) * self.p2c) >> 5);
        self.prediction[3] = n
            - ((te_nw * self.p3ca
                + te_n * self.p3cb
                + te_ne * self.p3cc
                + (nn - n) * self.p3cd
                + (nw - w) * self.p3ce)
                >> 5);

        let mut pred = self.weighted_average(&self.prediction, weights);
        self.pred = pred;

        // 三邻居误差同号 → 跳过 clamp
        if ((te_n ^ te_w) | (te_n ^ te_nw)) > 0 {
            return ((pred + 3) >> 3) as i32;
        }
        let mx = w.max(ne).max(n);
        let mn = w.min(ne).min(n);
        pred = pred.max(mn).min(mx);
        ((pred + 3) >> 3) as i32
    }

    /// 用实际值更新误差状态（编码端与解码端共用）。
    #[inline]
    fn update_errors(&mut self, val: i32, x: usize, y: usize) {
        let xsize = self.width;
        let cur_row = if y & 1 != 0 { 0 } else { xsize + 2 };
        let prev_row = if y & 1 != 0 { xsize + 2 } else { 0 };
        let val = (val as i64) << 3;
        self.error[cur_row + x] = (self.pred - val) as i32;
        let prediction = self.prediction;
        for (i, pe) in self.pred_errors.iter_mut().enumerate() {
            let err = (((prediction[i] - val).abs() + 3) >> 3) as u32;
            pe[cur_row + x] = err;
            pe[prev_row + x + 1] += err;
        }
    }
}

/// 取 (x,y) 处分量 c 的因果邻居 (top, left, topright, topleft, toptop)。
#[inline]
#[allow(clippy::too_many_arguments)]
fn wp_neighbors(
    buf: &[i32],
    x: usize,
    y: usize,
    width: usize,
    components: usize,
    c: usize,
) -> (i64, i64, i64, i64, i64) {
    let stride = width * components;
    let idx = (y * width + x) * components + c;
    let left = if x > 0 {
        buf[idx - components]
    } else if y > 0 {
        buf[idx - stride]
    } else {
        0
    };
    let top = if y > 0 { buf[idx - stride] } else { left };
    let topleft = if x > 0 && y > 0 {
        buf[idx - stride - components]
    } else {
        left
    };
    let topright = if x + 1 < width && y > 0 {
        buf[idx - stride + components]
    } else {
        top
    };
    let toptop = if y > 1 { buf[idx - 2 * stride] } else { top };
    (
        top as i64,
        left as i64,
        topright as i64,
        topleft as i64,
        toptop as i64,
    )
}

/// 自校正预测器整帧预测 → 残差。
fn wp_predict_frame(pixels: &[i32], width: usize, height: usize, components: usize) -> Vec<i32> {
    let mut out = vec![0i32; pixels.len()];
    for c in 0..components {
        let mut wp = WpState::new(width);
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * components + c;
                let (n, w, ne, nw, nn) = wp_neighbors(pixels, x, y, width, components, c);
                let pred = wp.predict(x, y, n, w, ne, nw, nn);
                out[idx] = pixels[idx] - pred;
                wp.update_errors(pixels[idx], x, y);
            }
        }
    }
    out
}

/// 单帧探针结果。
struct FrameStat {
    med: usize,
    paeth: usize,
    wp: usize,
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

    let limit = group_frame_limit();
    println!("=== JXL 自校正/加权预测器收益探针 ===");
    println!("root: {root}");
    println!(
        "每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: RCT 残差 → {{MED, Paeth, WP}} 预测 → RLE+Golomb | CRF adaptive\n");

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

                // MED / Paeth 固定预测 → RLE+Golomb
                let mut med_res = vec![0i32; residuals.len()];
                apply_prediction_into(
                    &residuals,
                    &mut med_res,
                    width,
                    height,
                    components,
                    PredictionMode::Med,
                );
                let (med_buf, _) =
                    encode_frame_rle_golomb_adaptive(&med_res).map_err(|e| e.to_string())?;

                let mut paeth_res = vec![0i32; residuals.len()];
                apply_prediction_into(
                    &residuals,
                    &mut paeth_res,
                    width,
                    height,
                    components,
                    PredictionMode::Paeth,
                );
                let (paeth_buf, _) =
                    encode_frame_rle_golomb_adaptive(&paeth_res).map_err(|e| e.to_string())?;

                // WP 自校正预测 → RLE+Golomb
                let wp_res = wp_predict_frame(&residuals, width, height, components);
                let (wp_buf, _) =
                    encode_frame_rle_golomb_adaptive(&wp_res).map_err(|e| e.to_string())?;

                // CRF 现有完整自适应编码
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
                    med: med_buf.len(),
                    paeth: paeth_buf.len(),
                    wp: wp_buf.len(),
                    crf: out.data.len(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let s = |f: fn(&FrameStat) -> usize| stats.iter().map(f).sum::<usize>();
        let (med, paeth, wp, crf) = (s(|x| x.med), s(|x| x.paeth), s(|x| x.wp), s(|x| x.crf));
        let best_fixed = med.min(paeth);
        let pct = |a: usize, b: usize| -> f64 {
            if b == 0 {
                0.0
            } else {
                (a as f64 - b as f64) / b as f64 * 100.0
            }
        };
        println!(
            "组 {:<12} ({} 帧): MED {} | Paeth {} | WP {} ({:+.1}% vs best) | CRF {}",
            name,
            n - 1,
            med,
            paeth,
            wp,
            pct(wp, best_fixed),
            crf,
        );
        all.extend(stats);
    }

    println!("\n=== 汇总 ===");
    if all.is_empty() {
        println!("(无有效组：需 ≥2 帧且 RGB)");
        return Ok(());
    }
    let sum = |f: fn(&FrameStat) -> usize| all.iter().map(f).sum::<usize>();
    let (med, paeth, wp, crf) = (
        sum(|x| x.med),
        sum(|x| x.paeth),
        sum(|x| x.wp),
        sum(|x| x.crf),
    );
    let best_fixed = med.min(paeth);
    let pct = |a: usize, b: usize| -> f64 {
        if b == 0 {
            0.0
        } else {
            (a as f64 - b as f64) / b as f64 * 100.0
        }
    };
    println!("帧数 {}", all.len());
    println!("MED + RLE:    {med}");
    println!("Paeth + RLE:  {paeth}");
    println!(
        "WP + RLE:     {wp}  ({:+.1}% vs MED/Paeth 最优)",
        pct(wp, best_fixed)
    );
    println!("CRF adaptive: {crf}  ({:+.1}% vs WP)", pct(crf, wp));

    let gain = pct(wp, best_fixed);
    println!(
        "\n判定：WP 相对 MED/Paeth 最优 {:+.2}% —— {}",
        gain,
        if gain <= -3.0 {
            "自校正预测器有效（≥3%）"
        } else {
            "自校正预测器无显著增益（<3%）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WP 预测 + 重建往返：残差 + WP 预测应还原原图。
    #[test]
    fn wp_predict_undo_roundtrip() {
        let width = 17usize;
        let height = 13usize;
        let components = 3usize;
        // 渐变 + 边缘 + 周期纹理的合成图
        let pixels: Vec<i32> = (0..width * height * components)
            .map(|i| {
                let x = (i / components) % width;
                let y = (i / components) / width;
                let c = i % components;
                (c as i32 * 40 + x as i32 / 3 + y as i32 / 5 + ((x + y) % 7) as i32) as i32
            })
            .collect();

        let residuals = wp_predict_frame(&pixels, width, height, components);

        // 重建：逐分量用 WP 预测 + 残差
        let mut restored = vec![0i32; pixels.len()];
        for c in 0..components {
            let mut wp = WpState::new(width);
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) * components + c;
                    let (n, w, ne, nw, nn) = wp_neighbors(&restored, x, y, width, components, c);
                    let pred = wp.predict(x, y, n, w, ne, nw, nn);
                    restored[idx] = residuals[idx] + pred;
                    wp.update_errors(restored[idx], x, y);
                }
            }
        }
        assert_eq!(pixels, restored, "WP 预测往返失败");
    }

    #[test]
    fn floor_log2_basic() {
        assert_eq!(floor_log2_nonzero(1), 0);
        assert_eq!(floor_log2_nonzero(2), 1);
        assert_eq!(floor_log2_nonzero(3), 1);
        assert_eq!(floor_log2_nonzero(8), 3);
        assert_eq!(floor_log2_nonzero(9), 3);
    }
}
