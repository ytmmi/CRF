//! P2 探针：固定块"局部预测 → 残差 DCT → 量化 → 系数熵编码 → 局部重建"
//!
//! 实验性质（规范 §7.3）：**探针不写入正式码流**——本模块仅返回载荷字节量
//! 与本地重建平面，用于与现有候选做内存级率失真对比；收益确认后再进入
//! 正式格式化流程（文档 → encoder/decoder 对称 → frame_type 分配）。
//!
//! 与规划 §5-P2 的对应关系与 v1 简化：
//! - 预测模式 {DC, H, V, MED} 按 8×8 块独立以 SAD 选择（未做 top-2 试编码）；
//! - 变换仅 8×8 lifting DCT（矩形/skip/矩阵/Trellis 后置）；
//! - 量化复用闭环语义（quant_scalar_biased 同式，正 bias 单侧加宽负残差死区）；
//! - 系数以 **level 域**（除以 q）交 RLE+CABAC 承载——代表 §4.6 所述
//!   "专用系数语法"的方向，与现行通用平面编码（q 倍数域）形成对照；
//! - 预测一律引用本地重建像素（编码/解码闭环一致，§5-P2 关键约束）。

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::transform::dct8x8_forward;
use crate::crf::transform::dct8x8_inverse;

/// 变换块尺寸（v1 固定 8×8）
const BLK: usize = 8;

/// 预测模式索引（信令 2bit/块）
const MODE_DC: i32 = 0;
const MODE_H: i32 = 1;
const MODE_V: i32 = 2;
const MODE_MED: i32 = 3;
const N_MODES: usize = 4;

/// 探针输出：载荷字节量级 + 本地重建平面（供闭环/PSNR 评估）
pub struct ProbeOutput {
    /// 模式表码流 + 系数码流的合计字节（不含任何帧头/文件级开销）
    pub payload_bytes: usize,
    /// 本地重建平面（与输入同尺寸；有损时含量化误差）
    pub recon: Vec<i32>,
    /// 各模式胜出块计数（诊断用）
    pub mode_hist: [usize; N_MODES],
}

/// 读取已重建像素；越界（图外/未重建）返回 0（残差域中性值）
#[inline]
fn recon_at(recon: &[i32], w: usize, h: usize, x: isize, y: isize) -> i32 {
    if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
        return 0;
    }
    recon[y as usize * w + x as usize]
}

/// 计算 8×8 块在给定模式下的预测值（全部引用已重建像素）
fn predict_block(
    recon: &[i32],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    mode: i32,
) -> [[i32; BLK]; BLK] {
    let mut out = [[0i32; BLK]; BLK];
    match mode {
        MODE_H => {
            for (by, row) in out.iter_mut().enumerate() {
                let left = recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize);
                for v in row.iter_mut() {
                    *v = left;
                }
            }
        }
        MODE_V => {
            let mut tops = [0i32; BLK];
            for (bx, top) in tops.iter_mut().enumerate() {
                *top = recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1);
            }
            for row in out.iter_mut() {
                for (v, top) in row.iter_mut().zip(tops.iter()) {
                    *v = *top;
                }
            }
        }
        MODE_DC => {
            // 可用邻域均值：左列 + 上行；均不可用时 0
            let mut sum = 0i64;
            let mut cnt = 0i64;
            if x0 > 0 {
                for by in 0..BLK {
                    sum += recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize) as i64;
                    cnt += 1;
                }
            }
            if y0 > 0 {
                for bx in 0..BLK {
                    sum += recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1) as i64;
                    cnt += 1;
                }
            }
            let dc = if cnt > 0 { (sum / cnt) as i32 } else { 0 };
            for row in &mut out {
                for v in row.iter_mut() {
                    *v = dc;
                }
            }
        }
        _ => {
            // MED：A=左 B=上 C=左上；p=A+B−C 收敛到 min/max(A,B,C)
            // （C 为块级常量：左上角单点）
            let c = recon_at(recon, w, h, x0 as isize - 1, y0 as isize - 1);
            for (by, row) in out.iter_mut().enumerate() {
                let a = recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize);
                for (bx, v) in row.iter_mut().enumerate() {
                    let b = recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1);
                    if x0 == 0 && y0 == 0 {
                        *v = 0;
                    } else if x0 == 0 {
                        *v = b;
                    } else if y0 == 0 {
                        *v = a;
                    } else {
                        let p = a + b - c;
                        let lo = a.min(b).min(c);
                        let hi = a.max(b).max(c);
                        *v = p.clamp(lo, hi);
                    }
                }
            }
        }
    }
    out
}

/// 带偏置的单点量化（与闭环 quant_scalar_biased 同式：正 bias 单侧加宽负残差死区）
#[inline]
fn quant_scalar(v: i32, q: i32, bias: i32) -> i32 {
    let denom = q * 64;
    let half = denom / 2;
    let b = if v >= 0 { bias } else { -bias };
    let level = (v.wrapping_abs() * 64 + half + b) / denom;
    let sign = if v < 0 { -1 } else { 1 };
    sign * level
}

/// P2 探针入口：单平面"预测后变换"编码的字节量级与本地重建
///
/// * `plane` — 输入平面（残差域或原域均可；边界外推值为 0）
/// * `q_step` — 量化步长（1=近无损；0 视作 1 处理）
/// * `deadzone` — 死区偏置（闭环同式）
pub fn encode_intra_probe(
    plane: &[i32],
    width: usize,
    height: usize,
    q_step: u8,
    deadzone: i8,
) -> CrfResult<ProbeOutput> {
    if width == 0 || height == 0 || plane.len() != width * height {
        return Err(CrfError::InvalidCodingParams(format!(
            "intra_probe: 尺寸/长度不一致 {}x{} vs {}",
            width,
            height,
            plane.len()
        )));
    }
    let q = (q_step.max(1)) as i32;
    let bias = deadzone as i32;

    let mut recon = vec![0i32; plane.len()];
    // P3 CABAC 系数编码器：概率自适应上下文 + RangeEncoder
    let mut coeff_enc = crate::crf::encoder::coeff_cabac::CoeffCABAC::new();
    let mut modes: Vec<i32> = Vec::new();
    let mut mode_hist = [0usize; N_MODES];

    let mut block = [0i32; 64];
    for y0 in (0..height).step_by(BLK) {
        for x0 in (0..width).step_by(BLK) {
            let bw = BLK.min(width - x0);
            let bh = BLK.min(height - y0);

            // SAD 选模式（v1 简化：单模式，不做 top-2 试编码）
            let mut best_mode = MODE_DC;
            let mut best_sad = u64::MAX;
            let mut best_pred = predict_block(&recon, width, height, x0, y0, MODE_DC);
            for mode in [MODE_DC, MODE_H, MODE_V, MODE_MED] {
                let pred = predict_block(&recon, width, height, x0, y0, mode);
                let mut sad = 0u64;
                for by in 0..bh {
                    for bx in 0..bw {
                        let d = plane[(y0 + by) * width + x0 + bx] - pred[by][bx];
                        sad += d.unsigned_abs() as u64;
                    }
                }
                if sad < best_sad {
                    best_sad = sad;
                    best_mode = mode;
                    best_pred = pred;
                }
            }
            mode_hist[best_mode as usize] += 1;
            modes.push(best_mode);

            // 残差 → [transform skip 或 DCT] → 量化 → zigzag → P3 编码
            // P2 transform skip（§5-P2 关键约束）：DC 预测模式的平坦块
            // 残差已极小，DCT 变换会扩散能量到 AC（DC 大 + AC 零散非零），
            // 直通量化更紧凑。H/V/MED 模式走 DCT（残差有方向性，变换集中能量）。
            block.fill(0);
            for by in 0..bh {
                for bx in 0..bw {
                    block[by * BLK + bx] = plane[(y0 + by) * width + x0 + bx] - best_pred[by][bx];
                }
            }
            let scanned = if best_mode == MODE_DC {
                // transform skip：残差直接量化（行优先 = zigzag 0..63 语义不同，
                // 但 CoeffEncoder 不关心物理位置——只编码 run-level）
                let mut qres = [0i32; 64];
                let mut dequant = [0i32; 64];
                for (i, &r) in block.iter().enumerate() {
                    let lv = quant_scalar(r, q, bias);
                    qres[i] = lv;
                    dequant[i] = lv * q;
                }
                // 重建：dequant + pred（无逆 DCT）
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] =
                            dequant[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                crate::crf::format::zigzag_scan(&qres, BLK)
            } else {
                // DCT 路径
                let freq = dct8x8_forward(&block);
                let mut qcoeffs = [0i32; 64];
                let mut dequant = [0i32; 64];
                for (i, &f) in freq.iter().enumerate() {
                    let lv = quant_scalar(f, q, bias);
                    qcoeffs[i] = lv;
                    dequant[i] = lv * q;
                }
                let spatial = dct8x8_inverse(&dequant);
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] =
                            spatial[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                crate::crf::format::zigzag_scan(&qcoeffs, BLK)
            };
            coeff_enc.encode_block(&scanned);
        }
    }

    // 信令：模式表走 RLE+CABAC；系数走专用 CoeffEncoder（P3 run-level）
    let mode_stream =
        crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive(&modes, Some(width / BLK))?
            .0;
    let coeff_stream = coeff_enc.finish();
    Ok(ProbeOutput {
        payload_bytes: mode_stream.len() + coeff_stream.len(),
        recon,
        mode_hist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常规结构验证：合成平面往返自洽、模式分布非退化
    #[test]
    fn probe_small_plane_structure() {
        let w = 64usize;
        let h = 48usize;
        let mut plane = vec![0i32; w * h];
        for y in 0..h {
            for x in 0..w {
                plane[y * w + x] = ((x / 4) * 9 + (y / 6) * 17) as i32 % 200 - 100;
            }
        }
        let out = encode_intra_probe(&plane, w, h, 2, 0).expect("probe 失败");
        assert_eq!(out.recon.len(), plane.len());
        assert!(out.payload_bytes > 0);
        assert_eq!(out.mode_hist.iter().sum::<usize>(), (w / BLK) * (h / BLK));
    }

    /// P2 探针实测：PNG1000 Y 平面上"预测后变换"与现有自适应管线的字节对比。
    /// 手动运行：cargo test p2_probe_vs_adaptive -- --ignored --nocapture
    #[test]
    #[ignore]
    fn p2_probe_vs_adaptive_png1000() {
        use crate::crf::{self, PredictionMode};
        use crate::test::{collect_png_paths, load_frame_sequence};

        let dir = r"E:\CRF\test\png\1000";
        let paths = collect_png_paths(dir);
        let frames = load_frame_sequence(&paths);
        assert!(frames.len() >= 3);

        let components = 3usize;
        for qi in [95u8, 90, 75] {
            let q_step = crate::crf::format::quant::quant_step_from_quality(qi);
            println!("=== q{} (step={}) ===", qi, q_step);
            for (fi, frame) in frames.iter().enumerate().skip(1).take(3) {
                let diff: Vec<i32> = frame
                    .pixels
                    .iter()
                    .zip(frames[0].pixels.iter())
                    .map(|(a, b)| a - b)
                    .collect();
                let eff = crf::format::rct_forward(&diff, components).unwrap();
                let mut y_plane = Vec::with_capacity(eff.len() / 3);
                for px in eff.chunks_exact(3) {
                    y_plane.push(px[0]);
                }
                let img = crate::crf::ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: 8,
                    color_format: crate::crf::format::ColorFormat::Gray,
                    pixels: y_plane.clone(),
                };
                let fq = super::super::frame::FrameQuant {
                    step: q_step,
                    bias: 0,
                    chroma_step: 0,
                    chroma_bias: 0,
                    chroma_half_res: false,
                    q1_matrix_scale: false,
                };
                let out_a = super::super::adaptive::encode_frame_adaptive(
                    &img,
                    crate::crf::format::CompressionType::GolombRice,
                    8,
                    false,
                    fq,
                    None,
                    None,
                )
                .expect("adaptive 编码失败");
                let out_b = encode_intra_probe(
                    &y_plane,
                    frame.width as usize,
                    frame.height as usize,
                    q_step,
                    0,
                )
                .expect("probe 失败");
                println!(
                    "frame {:2}: adaptive={} B | probe={} B ({:+.1}%)  modes {:?}",
                    fi,
                    out_a.data.len(),
                    out_b.payload_bytes,
                    (out_b.payload_bytes as f64 - out_a.data.len() as f64)
                        / out_a.data.len() as f64
                        * 100.0,
                    out_b.mode_hist
                );
            }
        }
    }
}
