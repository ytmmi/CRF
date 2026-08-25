use crate::crf::error::{CrfError, CrfResult};

use super::rle_golomb;

/// 解码调色板帧（frame_type=4）
///
/// `coding_params`：帧头字节。bit0=0 → v1 载荷（旧格式，索引流直编）；
/// bit0=1 → v2 载荷（copy-above token 化，AV1 palette 思路适配）。
/// `width`：帧宽（v2 copy-above 的上方同列距离 = width）。
///
/// 载荷布局 v2：
/// [palette_count u16 LE][flags u8(bit0=copy_above)][k_token u8][pal_len u32 LE]
/// [palette 值位流 pal_len 字节（exp-Golomb zigzag）]
/// [token 位流（RLE+Golomb 自适应 k）：0=复制上方同列索引，非零=显式索引+1]
pub(crate) fn decode_palette_payload(
    data: &[u8],
    pixel_count: usize,
    coding_params: u8,
    width: usize,
) -> CrfResult<Vec<i32>> {
    let copy_above = coding_params & 0x01 != 0;

    if copy_above {
        decode_palette_payload_v2(data, pixel_count, width)
    } else {
        decode_palette_payload_v1(data, pixel_count)
    }
}

/// v2 载荷解码：copy-above token 化索引流
fn decode_palette_payload_v2(data: &[u8], pixel_count: usize, width: usize) -> CrfResult<Vec<i32>> {
    const HEADER_LEN: usize = 8; // [count u16][flags u8][k u8][pal_len u32]
    if data.len() < HEADER_LEN {
        return Err(CrfError::InsufficientData {
            expected: HEADER_LEN,
            actual: data.len(),
        });
    }
    let palette_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    let _flags = data[2]; // bit0 已由调用方判定进入本分支
    let k_token = data[3];
    let pal_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if HEADER_LEN + pal_len > data.len() {
        return Err(CrfError::InsufficientData {
            expected: HEADER_LEN + pal_len,
            actual: data.len(),
        });
    }

    // 解码调色板值表
    let mut dec_pal = crate::crf::decoder::exp_golomb::ExpGolombDecoder::new(
        &data[HEADER_LEN..HEADER_LEN + pal_len],
    );
    let mut palette = Vec::with_capacity(palette_count);
    for _ in 0..palette_count {
        palette.push(dec_pal.decode_signed());
    }

    // 解码 token 流
    let mut dec_idx = rle_golomb::RleGolombDecoder::new(&data[HEADER_LEN + pal_len..], k_token);
    let tokens = dec_idx.decode_signed_array(pixel_count);

    // token → 索引重建（copy-above + 显式转义）
    let stride = width.max(1);
    let mut out = Vec::with_capacity(pixel_count);
    for (i, &t) in tokens.iter().enumerate() {
        if t == 0 {
            // 复制上方同列索引；首行不应出现（编码端保证），防御性回退首索引
            if i < stride {
                out.push(palette.first().copied().unwrap_or(0));
            } else {
                out.push(out[i - stride]);
            }
        } else {
            let ix = t - 1;
            let ix_u = ix as usize;
            if ix < 0 || ix_u >= palette.len() {
                return Err(CrfError::InvalidCodingParams(format!(
                    "palette index {} out of range (palette size {})",
                    ix,
                    palette.len()
                )));
            }
            out.push(palette[ix_u]);
        }
    }
    Ok(out)
}

/// v1 载荷解码（旧格式兼容）：索引流直接 RLE+Golomb
fn decode_palette_payload_v1(data: &[u8], pixel_count: usize) -> CrfResult<Vec<i32>> {
    const HEADER_LEN: usize = 7; // [count u16][k u8][pal_len u32]
    if data.len() < HEADER_LEN {
        return Err(CrfError::InsufficientData {
            expected: HEADER_LEN,
            actual: data.len(),
        });
    }
    let palette_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    let k_index = data[2];
    let pal_len = u32::from_le_bytes([data[3], data[4], data[5], data[6]]) as usize;
    if HEADER_LEN + pal_len > data.len() {
        return Err(CrfError::InsufficientData {
            expected: HEADER_LEN + pal_len,
            actual: data.len(),
        });
    }

    // 解码调色板值表
    let mut dec_pal = crate::crf::decoder::exp_golomb::ExpGolombDecoder::new(
        &data[HEADER_LEN..HEADER_LEN + pal_len],
    );
    let mut palette = Vec::with_capacity(palette_count);
    for _ in 0..palette_count {
        palette.push(dec_pal.decode_signed());
    }

    // 解码索引流并映射回像素值
    let mut dec_idx = rle_golomb::RleGolombDecoder::new(&data[HEADER_LEN + pal_len..], k_index);
    let indices = dec_idx.decode_signed_array(pixel_count);

    let mut out = Vec::with_capacity(indices.len());
    for ix in indices {
        let ix_u = ix as usize;
        if ix >= palette.len() as i32 || (ix < 0 && ix_u != ix as usize) || ix_u >= palette.len() {
            return Err(CrfError::InvalidCodingParams(format!(
                "palette index {} out of range (palette size {})",
                ix,
                palette.len()
            )));
        }
        out.push(palette[ix_u]);
    }
    Ok(out)
}
