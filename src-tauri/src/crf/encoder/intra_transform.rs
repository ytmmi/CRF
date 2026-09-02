//! frame_type=8 正式格式：三平面预测后变换 + CABAC 系数编码
//!
//! 载荷布局（v1.14）：
//! ```text
//! [flags u8]              // bit0=has_modes(预留), 其余保留
//! [len_y u32 LE][y_payload]
//! [len_co u32 LE][co_payload]
//! [len_cg u32 LE][cg_payload]
//! ```
//! 每个子载荷 = [mode_stream(CABAC)][coeff_stream(CABAC)]
//! 模式：DC=transform skip, H/V/MED=DCT；系数：run-level 嵌入式 CABAC。
//!
//! 解码端对称（见 decoder/intra_transform.rs）：
//! 1. 解析 mode_stream → 每块模式
//! 2. CABAC 解码 coeff_stream → 每块 zigzag 系数
//! 3. DC 模式：逆量化残差 + 预测重建
//! 4. HVMED 模式：逆 zigzag → 逆量化 → 逆 DCT + 预测重建

use rayon::prelude::*;

use crate::crf::encoder::coeff_cabac::CoeffCABAC;
use crate::crf::error::CrfResult;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::core::prediction::intra::{apply_prediction, undo_prediction};
use crate::crf::core::transform::{dct8x8_forward_into, dct8x8_inverse_into};

const BLK: usize = 8;
const MODE_DC: i32 = 0;
const MODE_H: i32 = 1;
const MODE_V: i32 = 2;
const MODE_MED: i32 = 3;

/// 编码单平面 → (载荷字节, 重建像素)
fn encode_plane(
    plane: &[i32],
    width: usize,
    height: usize,
    q_step: u8,
    deadzone: i8,
) -> CrfResult<(Vec<u8>, Vec<i32>)> {
    let q = (q_step.max(1)) as i32;
    let mut recon = vec![0i32; plane.len()];
    let mut coeff_enc = CoeffCABAC::new();
    let mut modes: Vec<i32> = Vec::new();
    let mut block = [0i32; 64];

    for y0 in (0..height).step_by(BLK) {
        for x0 in (0..width).step_by(BLK) {
            let bw = BLK.min(width - x0);
            let bh = BLK.min(height - y0);

            // 邻域预测（引用已重建像素；边界外推=0）
            let mut best_mode = MODE_DC;
            let mut best_sad = u64::MAX;
            let mut best_pred = [[0i32; BLK]; BLK];
            for mode in [MODE_DC, MODE_H, MODE_V, MODE_MED] {
                let pred = predict_block(&recon, width, height, x0, y0, mode);
                let mut sad = 0u64;
                for by in 0..bh {
                    for bx in 0..bw {
                        sad += (plane[(y0 + by) * width + x0 + bx] - pred[by][bx]).unsigned_abs()
                            as u64;
                    }
                }
                if sad < best_sad {
                    best_sad = sad;
                    best_mode = mode;
                    best_pred = pred;
                }
            }
            modes.push(best_mode);

            // 残差 → [skip 或 DCT] → 量化 → zigzag → CABAC
            block.fill(0);
            for by in 0..bh {
                for bx in 0..bw {
                    block[by * BLK + bx] = plane[(y0 + by) * width + x0 + bx] - best_pred[by][bx];
                }
            }
            let scanned = if best_mode == MODE_DC {
                // transform skip：直通量化
                let mut qres = [0i32; 64];
                let mut dq = [0i32; 64];
                crate::crf::backend::ops::quantize_levels_biased(
                    &block,
                    &mut qres,
                    q_step,
                    deadzone,
                );
                for (dequantized, &level) in dq.iter_mut().zip(qres.iter()) {
                    *dequantized = level * q;
                }
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] = dq[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                crate::crf::core::entropy::scan::zigzag_scan(&qres, BLK)
            } else {
                // DCT 路径
                let mut freq = [0i32; 64];
                dct8x8_forward_into(&block, &mut freq);
                let mut qc = [0i32; 64];
                let mut dq = [0i32; 64];
                crate::crf::backend::ops::quantize_levels_biased(
                    &freq,
                    &mut qc,
                    q_step,
                    deadzone,
                );
                for (dequantized, &level) in dq.iter_mut().zip(qc.iter()) {
                    *dequantized = level * q;
                }
                let mut spatial = [0i32; 64];
                dct8x8_inverse_into(&dq, &mut spatial);
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] =
                            spatial[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                crate::crf::core::entropy::scan::zigzag_scan(&qc, BLK)
            };
            coeff_enc.encode_block(&scanned);
        }
    }

    // 模式表走 CABAC（v3 载荷 [k][body]）
    let (mode_body, mode_k) =
        crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive(&modes, Some(width / BLK))?;
    let coeff_stream = coeff_enc.finish();

    // 子载荷 = [mode_len u32 LE][k u8][mode_body][coeff_stream]
    let mode_total = 1 + mode_body.len();
    let mut payload = Vec::with_capacity(4 + mode_total + coeff_stream.len());
    payload.extend_from_slice(&(mode_total as u32).to_le_bytes());
    payload.push(mode_k);
    payload.extend_from_slice(&mode_body);
    payload.extend_from_slice(&coeff_stream);
    Ok((payload, recon))
}

/// 编码三平面 frame_type=8 载荷
pub fn encode_intra_transform_payload(
    image: &ImageData,
    compression_type: CompressionType,
    q_step: u8,
    deadzone: i8,
    chroma_step: u8,
    chroma_bias: i8,
) -> CrfResult<Vec<u8>> {
    if compression_type != CompressionType::GolombRice {
        return Err(crate::crf::error::CrfError::InvalidCodingParams(
            "intra_transform requires GolombRice".into(),
        ));
    }
    let components = image.color_format.component_count();
    if components != 3 {
        return Err(crate::crf::error::CrfError::InvalidCodingParams(
            "intra_transform requires 3-component".into(),
        ));
    }
    let w = image.width as usize;
    let h = image.height as usize;

    // 拆三平面
    let mut planes: [Vec<i32>; 3] = std::array::from_fn(|_| Vec::with_capacity(w * h));
    for px in image.pixels.chunks_exact(3) {
        planes[0].push(px[0]);
        planes[1].push(px[1]);
        planes[2].push(px[2]);
    }

    // P1 首帧加速：三平面（Y/Co/Cg）编码互相独立（各自 CoeffCABAC + 重建
    // 缓冲局部状态，无跨平面依赖），并行计算各平面载荷。rayon collect 保序
    // + 后续按 Y/Co/Cg 顺序拼接，字节与串行版逐位一致。
    let plane_payloads: Vec<Vec<u8>> = planes
        .par_iter()
        .enumerate()
        .map(|(pi, plane)| -> CrfResult<Vec<u8>> {
            let (p_q, p_bias) = if pi == 0 {
                (q_step, deadzone) // Y: 亮度步长+偏置
            } else {
                (chroma_step.max(1), chroma_bias) // Co/Cg: 色度步长+偏置
            };
            let (payload, _recon) = encode_plane(plane, w, h, p_q, p_bias)?;
            Ok(payload)
        })
        .collect::<CrfResult<Vec<_>>>()?;

    let mut out = vec![0u8]; // flags 预留
    for payload in plane_payloads {
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
    }
    Ok(out)
}

// ===== 内部辅助（与 intra_probe 共享逻辑，独立维护避免耦合） =====

fn recon_at(recon: &[i32], w: usize, h: usize, x: isize, y: isize) -> i32 {
    if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
        return 0;
    }
    recon[y as usize * w + x as usize]
}

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
            for (bx, t) in tops.iter_mut().enumerate() {
                *t = recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1);
            }
            for row in out.iter_mut() {
                for (v, t) in row.iter_mut().zip(tops.iter()) {
                    *v = *t;
                }
            }
        }
        MODE_DC => {
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
                        *v = p.clamp(a.min(b).min(c), a.max(b).max(c));
                    }
                }
            }
        }
    }
    out
}
