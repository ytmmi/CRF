//! 帧内块复制（IntraBC / frame_type=7，v1.11 新增）
//!
//! ## 定位
//!
//! 首帧（golden 自然图像）的重复纹理特化：服装花纹、背景图案、网点等
//! 精确重复的 8×8 区域以「块复制向量」承载，仅需十余字节；差分帧大面积
//! 为零（RLE 已极致高效），故本候选仅在无损路径参与竞争。
//!
//! ## 载荷语法
//!
//! ```text
//! [block_count u32 LE][pred_mode u8][k_dec u8][k_res u8]
//! [len_dec u32 LE]  决策流 RLE+Golomb（逐块 {0=PRED, 1=COPY}）
//! [len_vec u32 LE]  COPY 向量流 exp-Golomb zigzag（逐 COPY 块 dx,dy）
//! [len_res u32 LE]  PRED 残差流 RLE+Golomb（PRED 块像素按序、
//!                   经 pred_mode 空间预测后的残差）
//! ```
//!
//! ## 关键正确性约束
//!
//! - **严格因果**：COPY 源块须整体位于已处理区（`sy < by`，或同行且
//!   `sx + BS ≤ bx`），编解码两端以同一判定保证重建一致；
//! - **像素粒度匹配**：交织数据按整像素（全分量）比较，杜绝跨分量错配；
//! - **单调不劣化**：全 PRED 时退化为普通预测 RLE + 少量决策流开销，
//!   由字节最小者胜出的多路竞争兜底。

use std::collections::HashMap;

use crate::crf::error::CrfResult;
use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::predict_at;
use crate::crf::format::sad_for_mode_sampled;

/// 块边长（像素）
const BS: usize = 8;
/// 每个 hash 桶保留的最近位置数（超出淘汰最旧）
const MAX_BUCKET: usize = 4;
/// 行段辅助索引深度（行）：仅索引最近 N 行的非对齐源
const SEG_DEPTH: u16 = 32;
/// 行段插入步长（像素）：内存/命中率折衷
const SEG_STEP: usize = 2;

/// 块签名：FNV-1a over 固定 8 采样点（四角 + 四边中点）的全分量值
///
/// 碰撞由候选全块精确比对兜底，key 仅用于缩小搜索域。
#[inline]
fn block_key(
    pixels: &[i32],
    stride: usize,
    components: usize,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
) -> u64 {
    let probes = [
        (0usize, 0usize),
        (bw.saturating_sub(1), 0),
        (0, bh.saturating_sub(1)),
        (bw.saturating_sub(1), bh.saturating_sub(1)),
        (bw / 2, 0),
        (0, bh / 2),
        (bw / 2, bh.saturating_sub(1)),
        (bw.saturating_sub(1), bh / 2),
    ];
    let mut hash: u64 = 0xcbf29ce484222325;
    for (px, py) in probes {
        let base = ((by + py) * stride) + (bx + px) * components;
        for c in 0..components {
            let v = pixels[base + c] as u64;
            hash ^= v.wrapping_mul(0x100000001b3);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// 行段签名（v1.11 双粒度辅助索引）：以像素 (x,y) 为起点的连续
/// BS 像素段的 FNV-1a。支持**非块对齐**的重复源定位——真实插画
/// 花纹平移量与块网格不对齐是常态（对称镜像图案尤甚）。
#[inline]
fn seg_hash(pixels: &[i32], stride: usize, components: usize, x: usize, y: usize) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for i in 0..BS {
        let base = y * stride + (x + i) * components;
        for c in 0..components {
            let v = pixels[base + c] as u64;
            hash ^= v.wrapping_mul(0x100000001b3);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// 块内容精确比对（像素粒度，仅有效区域）
#[inline]
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
fn block_equal(
    pixels: &[i32],
    stride: usize,
    components: usize,
    ax: usize,
    ay: usize,
    bx: usize,
    by: usize,
    pw: usize,
    ph: usize,
) -> bool {
    for r in 0..ph {
        let row_a = (ay + r) * stride + ax * components;
        let row_b = (by + r) * stride + bx * components;
        for i in 0..pw * components {
            if pixels[row_a + i] != pixels[row_b + i] {
                return false;
            }
        }
    }
    true
}

/// 在已处理区搜索当前块的精确匹配，返回相对向量 (dx, dy)
///
/// 候选来源（按序）：
/// 1. 块对齐签名桶（最近记录）——零成本快速路径；
/// 2. 三固定锚点（左/上/左上邻块）；
/// 3. 行段辅助索引——非块对齐的像素级源定位（深度 [`SEG_DEPTH`] 行，
///    惰性过期）。第一个精确命中即返回（LZ 式贪心，向量质量次要）。
#[allow(clippy::too_many_arguments)]
fn find_match(
    pixels: &[i32],
    stride: usize,
    components: usize,
    width: usize,
    height: usize,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
    table: &mut HashMap<u64, Vec<(u16, u16)>>,
    seg_index: &mut HashMap<u64, Vec<(u16, u16)>>,
) -> Option<(isize, isize)> {
    let key = block_key(pixels, stride, components, bx, by, bw, bh);
    let mut candidates: Vec<(isize, isize)> = Vec::with_capacity(MAX_BUCKET + 3);
    if let Some(list) = table.get(&key) {
        for &(sx, sy) in list.iter().rev() {
            candidates.push((sx as isize - bx as isize, sy as isize - by as isize));
        }
    }
    candidates.push((-(BS as isize), 0));
    candidates.push((0, -(BS as isize)));
    candidates.push((-(BS as isize), -(BS as isize)));

    // 行段辅助索引：当前块首行段 hash → 历史同行/近行起点。
    // 惰性过期：超出深度窗口的条目就地清除。
    let skey = seg_hash(pixels, stride, components, bx, by);
    if let Some(list) = seg_index.get_mut(&skey) {
        list.retain(|&(ey, _)| by.saturating_sub(ey as usize) <= SEG_DEPTH as usize);
        for &(ex, ey) in list.iter().rev() {
            candidates.push((ex as isize - bx as isize, ey as isize - by as isize));
        }
    }

    for (dx, dy) in candidates {
        let sx = bx as isize + dx;
        let sy = by as isize + dy;
        if sx < 0 || sy < 0 {
            continue;
        }
        let (sxu, syu) = (sx as usize, sy as usize);
        if sxu + BS > width || syu + BS > height {
            continue;
        }
        // 严格因果（完备形式）：源块必须整体落于已处理区。
        //   a) 整体在上方行带：syu + BS ≤ by；
        //   b) 整体在当前行带起点之上或本行、且位于左侧：sxu + BS ≤ bx。
        // 历史缺陷：旧条件 `syu < by` 允许了「源块纵跨当前行带且不在
        // 左侧」的非法引用——源块底部行在解码端尚未重建（缓冲为 0），
        // 编码端却读到原图值，两端不对称。
        let causal = syu + BS <= by || (sxu + BS <= bx && syu <= by);
        if !causal {
            continue;
        }
        if block_equal(pixels, stride, components, sxu, syu, bx, by, bw, bh) {
            return Some((dx, dy));
        }
    }
    None
}

/// PRED 块的空间预测模式：行采样 SAD 选取（一次性评估）
///
/// 候选**必须排除跨块右引用的模式**：DC 与 TopRight 的 top_right=
/// `(x+1, y−1)` 在 IntraBC 块级光栅序下，当 x 位于块最右列时会落入
/// **右邻块的同一行带**——编码端可见完整原图而解码端尚未重建该位置，
/// 破坏两端对称性（历史缺陷：块中间样本突然错位的根源）。
/// 其余模式仅依赖左侧/上方因果邻居，安全。
fn pick_pred_mode(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
) -> PredictionMode {
    const CANDIDATES: [PredictionMode; 6] = [
        PredictionMode::Horizontal,
        PredictionMode::Vertical,
        PredictionMode::Average,
        PredictionMode::Med,
        PredictionMode::Paeth,
        PredictionMode::Diagonal,
    ];
    CANDIDATES
        .iter()
        .copied()
        .min_by_key(|&m| sad_for_mode_sampled(pixels, width, height, components, m))
        .unwrap_or(PredictionMode::Paeth)
}

/// IntraBC 载荷编码入口（无损路径专用）
///
/// 返回完整载荷（不含帧头）；调用方经 [`crate::crf::encoder::assemble_frame`]
/// 包装为 frame_type=7 参与字节竞争。
pub(crate) fn encode_intrabc_payload(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
) -> CrfResult<Vec<u8>> {
    let stride = width * components;
    let _bw_grid = width.div_ceil(BS);

    let pred_mode = pick_pred_mode(pixels, width, height, components);

    let mut decisions: Vec<i32> = Vec::new();
    let mut copy_vecs: Vec<(isize, isize)> = Vec::new();
    let mut residuals: Vec<i32> = Vec::new();
    let mut table: HashMap<u64, Vec<(u16, u16)>> = HashMap::new();
    let mut seg_index: HashMap<u64, Vec<(u16, u16)>> = HashMap::new();

    for by in (0..height).step_by(BS) {
        let bh = BS.min(height - by);
        // 本行已插入段索引的右界（同行增量插入游标）
        let mut row_seg_upto = 0usize;
        for bx in (0..width).step_by(BS) {
            let bw = BS.min(width - bx);

            // 纯色块直接 PRED（预测后残差趋零，RLE 比 COPY token 更省）
            let first = by * stride + bx * components;
            let (mut mn, mut mx) = (pixels[first], pixels[first]);
            let mut uniform = true;
            'scan: for r in 0..bh {
                let base = (by + r) * stride + bx * components;
                for i in 0..bw * components {
                    let v = pixels[base + i];
                    if v < mn {
                        mn = v;
                    }
                    if v > mx {
                        mx = v;
                    }
                    if mx - mn > 0 {
                        uniform = false;
                        break 'scan;
                    }
                }
            }

            let hit = if uniform {
                None
            } else {
                find_match(
                    pixels,
                    stride,
                    components,
                    width,
                    height,
                    bx,
                    by,
                    bw,
                    bh,
                    &mut table,
                    &mut seg_index,
                )
            };

            match hit {
                Some((dx, dy)) => {
                    decisions.push(1);
                    copy_vecs.push((dx, dy));
                }
                None => {
                    decisions.push(0);
                    // PRED 块残差（逐像素因果预测，邻居含已处理的 COPY 块）
                    for r in 0..bh {
                        for x in 0..bw {
                            let idx = (by + r) * stride + (bx + x) * components;
                            for c in 0..components {
                                let sidx = idx + c;
                                let predicted = predict_at(
                                    pixels,
                                    sidx,
                                    bx + x,
                                    by + r,
                                    stride,
                                    components,
                                    width,
                                    pred_mode,
                                );
                                residuals.push(pixels[sidx] - predicted);
                            }
                        }
                    }
                }
            }

            // 同行段索引增量插入：本块起点左侧、距其至少一个完整段的区域
            // （源起点 sx 需满足 sx+BS ≤ bx 才可能通过因果校验）
            let seg_limit = bx.saturating_sub(BS);
            while row_seg_upto + BS <= width && row_seg_upto <= seg_limit {
                let sk = seg_hash(pixels, stride, components, row_seg_upto, by);
                let bucket = seg_index.entry(sk).or_default();
                if bucket.len() >= MAX_BUCKET {
                    bucket.remove(0);
                }
                bucket.push((by as u16, row_seg_upto as u16));
                row_seg_upto += SEG_STEP;
            }

            // 登记当前块签名（无论 PRED/COPY，均可被后续块引用）
            let key = block_key(pixels, stride, components, bx, by, bw, bh);
            let bucket = table.entry(key).or_default();
            if bucket.len() >= MAX_BUCKET {
                bucket.remove(0);
            }
            bucket.push((bx as u16, by as u16));
        }

        // 行尾补插：本行剩余可建段位置（供下一行及以后查询）
        while row_seg_upto + BS <= width {
            let sk = seg_hash(pixels, stride, components, row_seg_upto, by);
            let bucket = seg_index.entry(sk).or_default();
            if bucket.len() >= MAX_BUCKET {
                bucket.remove(0);
            }
            bucket.push((by as u16, row_seg_upto as u16));
            row_seg_upto += SEG_STEP;
        }
    }

    // ===== 三段序列化 =====
    let block_count = decisions.len();
    let mut dec_enc = crate::crf::encoder::rle_golomb::RleGolombEncoder::adaptive(&decisions);
    let k_dec = dec_enc.k;
    dec_enc.encode_signed_array(&decisions);
    let dec_bytes = dec_enc.finish();

    let mut vec_enc = crate::crf::encoder::exp_golomb::ExpGolombEncoder::new();
    for &(dx, dy) in &copy_vecs {
        vec_enc.encode_signed(dx as i32);
        vec_enc.encode_signed(dy as i32);
    }
    let vec_bytes = vec_enc.finish();

    let mut res_enc = crate::crf::encoder::rle_golomb::RleGolombEncoder::adaptive(&residuals);
    let k_res = res_enc.k;
    res_enc.encode_signed_array(&residuals);
    let res_bytes = res_enc.finish();

    let mut out = Vec::with_capacity(22 + dec_bytes.len() + vec_bytes.len() + res_bytes.len());
    out.extend_from_slice(&(block_count as u32).to_le_bytes());
    out.push(pred_mode as u8);
    out.push(k_dec);
    out.push(k_res);
    out.extend_from_slice(&(dec_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&dec_bytes);
    out.extend_from_slice(&(vec_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&vec_bytes);
    out.extend_from_slice(&(res_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&res_bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::encoder::encode_sequence;
    use crate::crf::core::domain::{ColorFormat, EncodeParams, PredictionMode};
    use crate::crf::ImageData;

    /// 安全取像素（越界返回 -9，仅诊断用）
    fn px_get(buf: &[i32], x: usize, y: usize, w: usize) -> i32 {
        if x >= w || y >= buf.len() / w {
            -9
        } else {
            buf[y * w + x]
        }
    }

    /// 合成重复花纹：左半区图案在右半区精确重复 → 应产生 COPY 块，
    /// 且载荷显著小于全 PRED 基线（决策+残差流）。
    #[test]
    fn test_intrabc_captures_repeated_pattern() {
        let w = 64usize;
        let h = 64usize;
        let mut px = vec![0i32; w * h];
        // 左半：伪随机纹理；右半：镜像重复（精确匹配源充足）
        for y in 0..h {
            for x in 0..w / 2 {
                let v = ((x * 37 + y * 17) % 191) as i32;
                px[y * w + x] = v;
                px[y * w + (w - 1 - x)] = v;
            }
        }
        let payload = encode_intrabc_payload(&px, w, h, 1).unwrap();
        // 决策流应包含 COPY（值 1 存在于解码结果中——间接验证见往返测试）
        assert!(!payload.is_empty());

        // 对照基线：同数据走纯预测 RLE（frame_type=1 语义）
        let baseline =
            crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive(&px).unwrap();
        assert!(
            payload.len() + 11 < baseline.0.len(),
            "IntraBC 载荷({})应显著小于纯 RLE 基线({})",
            payload.len(),
            baseline.0.len()
        );
    }

    /// 无重复内容的噪声场：全 PRED 退化路径仍须产出合法载荷
    /// （竞争淘汰兜底，此处验证不 panic 且结构完整）。
    #[test]
    fn test_intrabc_noise_falls_back_to_all_pred() {
        let w = 32usize;
        let h = 32usize;
        let px: Vec<i32> = (0..w * h)
            .map(|i| ((i as u64).wrapping_mul(2654435761) % 97) as i32)
            .collect();
        let payload = encode_intrabc_payload(&px, w, h, 1).unwrap();
        // 头部：block_count + pred_mode + k_dec + k_res + len_dec
        let block_count = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        assert_eq!(block_count, 16); // 32/8 × 32/8
        assert!(payload.len() > 20);
    }

    /// 三分量交织数据往返（配合 decoder/intrabc 对称实现）：
    /// 重复图案应被捕捉且解码逐位还原。
    #[test]
    fn test_intrabc_interleaved_roundtrip() {
        let w = 48usize;
        let h = 32usize;
        let comp = 3usize;
        let mut px = vec![0i32; w * h * comp];
        for y in 0..h {
            for x in 0..w / 2 {
                let base_l = (y * w + x) * comp;
                let base_r = (y * w + (w - 1 - x)) * comp;
                for c in 0..comp {
                    let v = ((x * 31 + y * 13 + c * 7) % 200) as i32 - 100;
                    px[base_l + c] = v;
                    px[base_r + c] = v;
                }
            }
        }
        let payload = encode_intrabc_payload(&px, w, h, comp).unwrap();
        let decoded =
            crate::crf::decoder::intrabc::decode_intrabc_payload(&payload, w, h, comp).unwrap();
        assert_eq!(px, decoded, "交织数据 IntraBC 往返失败");
    }

    /// 最小复现回归锚点（历史缺陷：DC 的 top_right 跨块引用破坏
    /// 块级光栅序因果性 → 已从 PRED 候选中剔除 DC）。
    #[test]
    fn debug_minimal_all_pred_roundtrip() {
        let w = 17usize;
        let h = 19usize;
        let px: Vec<i32> = (0..w * h)
            .map(|i| ((i as u64).wrapping_mul(2654435761) % 251) as i32 - 125)
            .collect();
        let payload = encode_intrabc_payload(&px, w, h, 1).unwrap();
        let decoded =
            crate::crf::decoder::intrabc::decode_intrabc_payload(&payload, w, h, 1).unwrap();
        assert_eq!(px, decoded);
    }

    /// 差分帧形态专项：90% 零场 + 两处相同的非零图案块。
    /// COPY 应大量命中；往返必须逐位还原（含 PRED/COPY 交错的
    /// 残差流切片与向量流顺序）。
    #[test]
    fn test_intrabc_differential_frame_roundtrip() {
        let w = 64usize;
        let h = 64usize;
        let mut px = vec![0i32; w * h * 3];
        for (ox, oy) in [(10usize, 10usize), (40usize, 40usize)] {
            for y in 0..12usize {
                for x in 0..12usize {
                    for c in 0..3usize {
                        px[((oy + y) * w + ox + x) * 3 + c] =
                            ((x * 7 + y * 11 + c * 5) % 60) as i32 + 1;
                    }
                }
            }
        }
        let payload = encode_intrabc_payload(&px, w, h, 3).unwrap();
        let dec = crate::crf::decoder::intrabc::decode_intrabc_payload(&payload, w, h, 3).unwrap();
        assert_eq!(px, dec, "差分形态 IntraBC 往返失败");
    }

    /// 端到端诊断：差分帧 IntraBC（CRF_ITBC_DIFF=1）在 planar 竞争场景下
    /// 的逐位一致性排查。复现 test_lossy_mode_error_bound_and_size 的
    /// 数据构造，dump 帧头类型与失配位置。
    #[test]
    fn debug_itbc_diff_frame_e2e() {
        use crate::crf::core::bitstream::constants::HEADER_SIZE;
        let frames: Vec<ImageData> = (0..3)
            .map(|fi| {
                let pixels: Vec<i32> = (0..64 * 64 * 3)
                    .map(|j| {
                        let base = ((j * 31) % 211) as i32 - 105;
                        let delta = (((j * 7 + fi * 13) % 9) as i32) - 4;
                        base + delta
                    })
                    .collect();
                ImageData {
                    width: 64,
                    height: 64,
                    bit_depth: 8,
                    color_format: ColorFormat::Rgb,
                    pixels,
                }
            })
            .collect();
        let params = EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: PredictionMode::Average,
            adaptive_prediction: true,
            lossy_quality: None,
            lossy_tuning: None,
            input_original_frames: true,
            user_metadata: None,
        };
        let data = encode_sequence(&frames, &params).unwrap();

        // 帧头类型 dump
        let mut off = HEADER_SIZE + 3 * 8;
        for fi in 0..3 {
            let fsz = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                as usize;
            println!(
                "frame {}: type={} cp={:#04x} pm={:#04x} size={}",
                fi,
                data[off + 8],
                data[off + 9],
                data[off + 10],
                fsz
            );
            off += 11 + fsz;
        }

        // 手算预期：type=7 差分帧的解码输出经外层 rct_inverse 后应为
        // RGB 域差分 frame_i − frame_0（golden 还原由调用方负责）
        let diff_rgb = |fi: usize| -> Vec<i32> {
            frames[fi]
                .pixels
                .iter()
                .zip(frames[0].pixels.iter())
                .map(|(a, b)| a - b)
                .collect()
        };

        // ===== 载荷级隔离复现：对 frame1 的 YCoCg 差分直接往返 =====
        {
            let diff = diff_rgb(1);
            let ycocg = crate::crf::core::color::rct::rct_forward(&diff, 3).unwrap();
            let itbc_payload = encode_intrabc_payload(&ycocg, 64, 64, 3).unwrap();
            // 诊断：解析决策与向量
            {
                let bc = u32::from_le_bytes([
                    itbc_payload[0],
                    itbc_payload[1],
                    itbc_payload[2],
                    itbc_payload[3],
                ]) as usize;
                let kd = itbc_payload[5];
                let mut p = 7usize;
                let mut rdseg = |p: &mut usize| {
                    let l = u32::from_le_bytes([
                        itbc_payload[*p],
                        itbc_payload[*p + 1],
                        itbc_payload[*p + 2],
                        itbc_payload[*p + 3],
                    ]) as usize;
                    *p += 4;
                    let s = itbc_payload[*p..*p + l].to_vec();
                    *p += l;
                    s
                };
                let dec_b = rdseg(&mut p);
                let vec_b = rdseg(&mut p);
                let decisions = crate::crf::decoder::rle_golomb::RleGolombDecoder::new(&dec_b, kd)
                    .decode_signed_array(bc);
                println!("[diag] blocks={bc} decisions={decisions:?}");
                let mut vec_dec = crate::crf::decoder::exp_golomb::ExpGolombDecoder::new(&vec_b);
                for (b, &d) in decisions.iter().enumerate() {
                    if d == 1 {
                        let dx = vec_dec.decode_signed();
                        let dy = vec_dec.decode_signed();
                        let bgx = (b % 8) * 8;
                        let bgy = (b / 8) * 8;
                        println!("[diag] COPY 块#{b} @({bgx},{bgy}) v=({dx},{dy})");
                    }
                }
            }
            let itbc_back =
                crate::crf::decoder::intrabc::decode_intrabc_payload(&itbc_payload, 64, 64, 3)
                    .unwrap();
            if itbc_back != ycocg {
                let p = ycocg
                    .iter()
                    .zip(itbc_back.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(usize::MAX);
                panic!(
                    "载荷级往返失配 @{p}（像素 {}）：期 {} 得 {}",
                    p / 3,
                    ycocg[p],
                    itbc_back[p]
                );
            }
            println!("载荷级隔离往返 ✓");
        }

        let dec = crate::crf::decode_from_bytes(&data).unwrap();
        for (fi, d) in dec.frames.iter().enumerate() {
            if fi == 0 {
                continue;
            }
            let expect = diff_rgb(fi);
            if d.pixels[..] != expect[..] {
                let pos = expect.iter().zip(d.pixels.iter()).position(|(a, b)| a != b);
                panic!(
                    "frame {} type=7 差分域重建失配 @{:?} 期 {} 得 {}",
                    fi,
                    pos,
                    pos.map(|p| expect[p]).unwrap_or(0),
                    pos.map(|p| d.pixels[p]).unwrap_or(0),
                );
            }
        }
        println!("差分域逐位一致 ✓");
    }

    /// 灰度往返 + 残缺边界块（非 8 对齐尺寸）。
    #[test]
    fn test_intrabc_gray_roundtrip_partial_blocks() {
        let w = 45usize; // 非 8 对齐
        let h = 27usize;
        let mut px = vec![0i32; w * h];
        for y in 0..h {
            for x in 0..w / 2 {
                let v = ((x * 23 + y * 41) % 250) as i32;
                px[y * w + x] = v;
                px[y * w + (w - 1 - x)] = v;
            }
        }
        let payload = encode_intrabc_payload(&px, w, h, 1).unwrap();
        // ===== 临时诊断：解析决策流并重放 =====
        let block_count = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let pm = payload[4];
        let k_dec = payload[5];
        let k_res = payload[6];
        let rd = |o: usize| -> usize {
            u32::from_le_bytes([payload[o], payload[o + 1], payload[o + 2], payload[o + 3]])
                as usize
        };
        let len_dec = rd(7);
        let dec_bytes = &payload[11..11 + len_dec];
        let decisions = crate::crf::decoder::rle_golomb::RleGolombDecoder::new(dec_bytes, k_dec)
            .decode_signed_array(block_count as usize);
        println!(
            "pm={} k_dec={} decisions={:?} len={}",
            pm,
            k_dec,
            decisions,
            decisions.len()
        );
        let decoded =
            crate::crf::decoder::intrabc::decode_intrabc_payload(&payload, w, h, 1).unwrap();
        for i in 0..px.len() {
            if px[i] != decoded[i] {
                let (x, y) = (i % w, i / w);
                let b = (y / 8) * 6 + (x / 8);
                panic!(
                    "首个失配 idx={i} ({x},{y}) 块#{b}({}) 期望 {} 得 {}",
                    decisions.get(b).unwrap_or(&-9),
                    px[i],
                    decoded[i]
                );
            }
        }
    }
}
