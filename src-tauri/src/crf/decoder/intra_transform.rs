//! frame_type=8 解码：三平面预测后变换 + CABAC 系数解码
//!
//! 与 encoder/intra_transform.rs 对称：
//! 1. 解析子载荷 [len][mode_stream][coeff_stream]
//! 2. CABAC 解码模式表 + 系数流
//! 3. DC 模式：逆量化残差 + 预测重建
//! 4. HVMED 模式：逆 zigzag → 逆量化 → 逆 DCT + 预测重建

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::transform::dct8x8_inverse_into;

const BLK: usize = 8;
const MODE_DC: i32 = 0;
const MODE_H: i32 = 1;
const MODE_V: i32 = 2;
#[allow(dead_code)]
const MODE_MED: i32 = 3;

/// 解码 frame_type=8 载荷 → 重建整帧 RGB
pub fn decode_intra_transform(
    data: &[u8],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
    chroma_step: u8,
) -> CrfResult<Vec<i32>> {
    if components != 3 || data.is_empty() {
        return Err(CrfError::InvalidCodingParams(
            "intra_transform: bad params".into(),
        ));
    }
    let q = (q_step.max(1)) as i32;
    let mut offset = 1; // 跳过 flags

    let mut planes: Vec<Vec<i32>> = Vec::with_capacity(3);
    for pi in 0..3 {
        if offset + 4 > data.len() {
            return Err(CrfError::InsufficientData {
                expected: offset + 4,
                actual: data.len(),
            });
        }
        let plen = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if offset + plen > data.len() {
            return Err(CrfError::InsufficientData {
                expected: offset + plen,
                actual: data.len(),
            });
        }
        let sub = &data[offset..offset + plen];
        offset += plen;
        let p_q = if pi == 0 {
            q
        } else {
            chroma_step.max(1) as i32
        };
        planes.push(decode_plane(sub, width, height, p_q)?);
    }

    // 交织三平面 → RGB
    let mut out = Vec::with_capacity(width * height * components);
    #[allow(clippy::needless_range_loop)]
    for i in 0..width * height {
        for c in 0..3 {
            out.push(planes[c][i]);
        }
    }
    Ok(out)
}

fn decode_plane(data: &[u8], width: usize, height: usize, q: i32) -> CrfResult<Vec<i32>> {
    if data.len() < 4 {
        return Err(CrfError::InsufficientData {
            expected: 4,
            actual: data.len(),
        });
    }
    let mode_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if 4 + mode_len > data.len() {
        return Err(CrfError::InsufficientData {
            expected: 4 + mode_len,
            actual: data.len(),
        });
    }
    let mode_data = &data[4..4 + mode_len];
    let coeff_data = &data[4 + mode_len..];

    // 解码模式表（RLE+CABAC）
    let block_count = (width.div_ceil(BLK)) * (height.div_ceil(BLK));
    // mode_stream 走 v3 payload [k][body]，k 在首字节
    let k = if mode_data.is_empty() {
        0
    } else {
        mode_data[0]
    };
    let modes = crate::crf::decoder::rle_cabac::decode_frame_rle_cabac(
        &mode_data[1..],
        k,
        block_count,
        Some(width / BLK),
    );

    // 解码系数流（CoeffCABAC：3 上下文 + 直通）
    let mut coeff_dec = crate::crf::decoder::coeff_cabac::CoeffCABACDecoder::new(coeff_data);

    // 重建
    let mut recon = vec![0i32; width * height];
    let mut mode_idx = 0;
    let mut block = [0i32; 64];

    for y0 in (0..height).step_by(BLK) {
        for x0 in (0..width).step_by(BLK) {
            let bw = BLK.min(width - x0);
            let bh = BLK.min(height - y0);
            let mode = modes.get(mode_idx).copied().unwrap_or(MODE_DC);
            mode_idx += 1;

            // 从 CABAC 系数流解码一个块（zigzag 序）
            let qz = coeff_dec.decode_block();

            // 逆 zigzag → 行优先
            let spatial_coeffs = crate::crf::core::entropy::scan::zigzag_inverse(&qz, BLK);

            if mode == MODE_DC {
                // transform skip：逆量化残差
                for i in 0..64 {
                    block[i] = spatial_coeffs[i] * q;
                }
            } else {
                // DCT：逆量化 → 逆 DCT
                let mut dq = [0i32; 64];
                for i in 0..64 {
                    dq[i] = spatial_coeffs[i] * q;
                }
                dct8x8_inverse_into(&dq, &mut block);
            }

            // 加回预测
            let pred = predict_block(&recon, width, height, x0, y0, mode);
            for by in 0..bh {
                for bx in 0..bw {
                    recon[(y0 + by) * width + x0 + bx] = block[by * BLK + bx] + pred[by][bx];
                }
            }
        }
    }
    Ok(recon)
}

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
                        *v = (a + b - c).clamp(a.min(b).min(c), a.max(b).max(c));
                    }
                }
            }
        }
    }
    out
}
