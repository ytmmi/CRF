//! 帧内块复制解码（frame_type=7，与 encoder/intrabc 严格对称）
//!
//! 重建缓冲逐块填充：COPY 块从已重建区域整体拷贝（严格因果约束由
//! 编码端保证，此处按同判定防御性校验）；PRED 块从残差流消费
//! BS²·comp 个样本并逐像素逆预测（邻居取自含已填充 COPY 块的缓冲）。

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::predict_at;

const BS: usize = 8;

/// IntraBC 载荷解码入口
///
/// 输出即像素域样本（PRED 块已在内部完成逆预测），调用方须跳过
/// 外层 undo_prediction（语义同 frame_type=6）。
pub(crate) fn decode_intrabc_payload(
    data: &[u8],
    width: usize,
    height: usize,
    components: usize,
) -> CrfResult<Vec<i32>> {
    const HEADER: usize = 4 + 1 + 1 + 1 + 4;
    if data.len() < HEADER {
        return Err(CrfError::InsufficientData {
            expected: HEADER,
            actual: data.len(),
        });
    }
    let block_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let pred_mode = PredictionMode::from_u8(data[4]);
    let k_dec = data[5];
    let k_res = data[6];

    let mut pos = 7usize;
    let read_seg = |pos: &mut usize| -> CrfResult<&[u8]> {
        if *pos + 4 > data.len() {
            return Err(CrfError::InsufficientData {
                expected: *pos + 4,
                actual: data.len(),
            });
        }
        let len = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]])
            as usize;
        *pos += 4;
        if *pos + len > data.len() {
            return Err(CrfError::InsufficientData {
                expected: *pos + len,
                actual: data.len(),
            });
        }
        let seg = &data[*pos..*pos + len];
        *pos += len;
        Ok(seg)
    };

    let dec_bytes = read_seg(&mut pos)?;
    let vec_bytes = read_seg(&mut pos)?;
    let res_bytes = read_seg(&mut pos)?;

    // 决策序列
    let decisions = crate::crf::decoder::rle_golomb::RleGolombDecoder::new(dec_bytes, k_dec)
        .decode_signed_array(block_count);

    let stride = width * components;

    let bw_grid = width.div_ceil(BS);
    if decisions.len() < block_count {
        return Err(CrfError::InsufficientData {
            expected: block_count,
            actual: decisions.len(),
        });
    }

    // PRED 残差流必须**一次性**解码：RLE 零行程可跨块边界延续，
    // 若按块切片消费，decode_signed_array 的 remaining 截断会丢弃
    // 跨界行程的尾部（历史缺陷：块中间样本突然错位的根源）。
    // 先汇总 PRED 总量，整体解码后按块切片取用。
    let mut pred_total = 0usize;
    for (b, decision) in decisions.iter().enumerate() {
        if *decision == 0 {
            let bgy = (b / bw_grid) * BS;
            let bgx = (b % bw_grid) * BS;
            pred_total += BS.min(height - bgy) * BS.min(width - bgx) * components;
        }
    }
    let all_residuals = crate::crf::decoder::rle_golomb::RleGolombDecoder::new(res_bytes, k_res)
        .decode_signed_array(pred_total);

    let mut pixels = vec![0i32; height * stride];
    let mut vec_dec = crate::crf::decoder::exp_golomb::ExpGolombDecoder::new(vec_bytes);
    let mut res_pos = 0usize;

    #[allow(clippy::needless_range_loop)] // 长函数体索引多缓冲区，保持下标语义一致
    for b in 0..block_count {
        let bgy = (b / bw_grid) * BS;
        let bgx = (b % bw_grid) * BS;
        let bh = BS.min(height - bgy);
        let bw = BS.min(width - bgx);

        if decisions[b] == 1 {
            // COPY 块：读取向量并从已重建区拷贝
            let dx = vec_dec.decode_signed() as isize;
            let dy = vec_dec.decode_signed() as isize;
            let sx = bgx as isize + dx;
            let sy = bgy as isize + dy;
            if sx < 0 || sy < 0 {
                return Err(CrfError::InvalidCodingParams(format!(
                    "IntraBC 非法向量 ({dx},{dy})"
                )));
            }
            let (sxu, syu) = (sx as usize, sy as usize);
            // 严格因果（完备形式，与编码端 find_match 同一判定）：
            //   a) 整体在上方行带：syu + BS ≤ bgy；或
            //   b) 整体在左侧：sxu + BS ≤ bgx 且 syu ≤ bgy。
            let causal = syu + BS <= bgy || (sxu + BS <= bgx && syu <= bgy);
            if !causal || syu + BS > height || sxu + BS > width {
                return Err(CrfError::InvalidCodingParams(format!(
                    "IntraBC 向量 ({dx},{dy}) 违反因果/边界约束 @({bgx},{bgy})"
                )));
            }
            for r in 0..bh {
                let src = (syu + r) * stride + sxu * components;
                let dst = (bgy + r) * stride + bgx * components;
                // 重叠引用合法（LZ 式纹理延伸），copy_within 为 memmove 语义
                pixels.copy_within(src..src + bw * components, dst);
            }
        } else {
            // PRED 块：从整体残差流切片并逐像素逆预测
            let count = bw * bh * components;
            let residuals = &all_residuals[res_pos..res_pos + count];
            res_pos += count;
            for r in 0..bh {
                for x in 0..bw {
                    let idx = (bgy + r) * stride + (bgx + x) * components;
                    for c in 0..components {
                        let sidx = idx + c;
                        let predicted = predict_at(
                            &pixels,
                            sidx,
                            bgx + x,
                            bgy + r,
                            stride,
                            components,
                            width,
                            pred_mode,
                        );
                        pixels[sidx] =
                            residuals[r * bw * components + x * components + c] + predicted;
                    }
                }
            }
        }
    }

    Ok(pixels)
}
