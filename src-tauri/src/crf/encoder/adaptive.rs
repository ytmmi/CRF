//! 逐帧自适应预测模式决策与候选竞争（frame_type 多路仲裁核心）
//!
//! 决策流水线：
//! 1. 行采样 SAD 排序全部候选模式；
//! 2. top-2 候选真实熵编码取最小（帧级路径）；
//! 3. 无损模式下与条带(2)/三平面(3)/调色板(4)竞争；
//! 4. CABAC 算术编码候选(5)对最优模式重新编码。
//!
//! 所有候选按"字节最小者胜出"，保证相对任何单一路径单调不劣化。

use rayon::prelude::*;

use crate::crf::encoder::banded::encode_banded_payload;
use crate::crf::encoder::intra_transform::encode_intra_transform_payload;
use crate::crf::encoder::planar::encode_planar_payload;
use crate::crf::error::{CrfError, CrfResult};
use crate::crf::format::{
    apply_prediction, sad_for_mode_sampled, CompressionType, ImageData, PredictionMode,
    BAND_HEIGHT, FRAME_HEADER_SIZE,
};

use super::frame::BandSteps;
use super::rdoq::trellis_quantize_interleaved;
use super::{assemble_frame, encode_frame_inner_limited, rle_cabac, FrameQuant};

/// 自适应编码结果：帧数据 + 帧级可识别的预测模式
///
/// pred_mode 在帧级(1/5)或 CABAC 路径胜出时为 Some，
/// 条带(2)/三平面(3)/调色板(4) 胜出时为 None（无单一帧级模式）。
pub struct AdaptiveOutput {
    pub data: Vec<u8>,
    pub pred_mode: Option<PredictionMode>,
}

/// 逐帧自适应预测的候选模式
///
/// 排除 None：差分/残差数据上无预测几乎不可能最优，
/// 且省去一次完整编码可显著降低决策开销。
/// v1.9 新增 TopRight(45°)/Diagonal(135°) 斜向模式（AV1 D45/D135 因果简化版），
/// 覆盖二次元插画高频出现的 ±45° 线条走向；SAD 排序自动纳入竞争。
pub(crate) const ADAPTIVE_CANDIDATES: [PredictionMode; 8] = [
    PredictionMode::Horizontal,
    PredictionMode::Vertical,
    PredictionMode::Average,
    PredictionMode::DC,
    PredictionMode::Med,
    PredictionMode::Paeth,
    PredictionMode::TopRight,
    PredictionMode::Diagonal,
];

/// 计算指定预测模式下残差的 SAD（绝对值和）
///
/// 作为编码比特数的快速代理指标：残差能量越小，Golomb/RLE 编码输出越短。
#[allow(dead_code)]
fn sad_for_mode(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> u64 {
    apply_prediction(pixels, width, height, components, mode)
        .iter()
        .map(|&v| v.unsigned_abs() as u64)
        .sum()
}

/// 编码单帧（逐帧自适应预测模式选择）
///
/// 两阶段决策：
/// 1. 对全部候选模式计算残差 SAD（O(N) 纯整数运算），按代价排序；
/// 2. 仅对 SAD 最小的 2 个候选执行真实熵编码，采用字节数最小者。
///
/// 相比全候选试编码（5 次完整编码），开销约为 5 次 SAD + 2 次编码，
/// 而决策质量接近穷举。帧头记录实际选用的预测模式。
///
/// GolombRice 类型下额外与"条带级自适应"（frame_type=2）竞争，取更小者：
/// 条带化允许帧内不同区域使用不同预测模式，适应局部内容差异。
///
/// `band_steps`：逐条带自适应步长表（噪声归一化），透传至闭环量化
/// 候选（帧级/CABAC/planar-Y）；None = 全帧统一步长。
pub fn encode_frame_adaptive(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    is_first_frame: bool,
    fq: FrameQuant,
    preferred_mode: Option<PredictionMode>,
    band_steps: BandSteps<'_>,
) -> CrfResult<AdaptiveOutput> {
    // 第一阶段：行采样 SAD 快速评估（约 1/4 开销，仅用于排序）
    // v1.11：8 候选模式 rayon 并行求值（纯函数无共享状态），
    // 多核下决策耗时近似减半以上。
    let width = image.width as usize;
    let height = image.height as usize;
    let components = image.color_format.component_count();

    let mut ranked: Vec<(u64, PredictionMode)> = ADAPTIVE_CANDIDATES
        .par_iter()
        .map(|&m| {
            (
                sad_for_mode_sampled(&image.pixels, width, height, components, m),
                m,
            )
        })
        .collect();
    ranked.sort_by_key(|&(sad, _)| sad);

    // 历史引导：前一帧胜出的预测模式优先进入试编码集
    if let Some(pm) = preferred_mode {
        if let Some(pos) = ranked.iter().position(|&(_, m)| m == pm) {
            let e = ranked.remove(pos);
            ranked.insert(0, e);
        }
    }

    // 第二阶段：top-2 真实编码，取字节数最小者
    // (字节数, 数据, 帧级可识别的预测模式——条带/三平面/调色板胜出时为 None)
    //
    // v1.11 Fast-Fail：第二个候选用第一名的总字节数作上限短路编码——
    // 熵码流字节单调递增，超限即必败，产物不进入最终码流。
    // 第二阶段：top-2 真实编码，取字节数最小者
    // (字节数, 数据, 帧级可识别的预测模式——条带/三平面/调色板胜出时为 None)
    //
    // v1.11 Fast-Fail：第二个候选用第一名的总字节数作上限短路编码——
    // 熵码流字节单调递增，超限即必败，产物不进入最终码流。
    let mut best: Option<(usize, Vec<u8>, Option<PredictionMode>)> = None;
    for (attempt, &(_, mode)) in ranked.iter().take(2).enumerate() {
        // 第二名候选的上限 = 第一名总长（含帧头）：熵码流字节单调递增，
        // 超限即必败，被淘汰候选的产物本就不会进入最终码流
        let limit = if attempt == 0 {
            usize::MAX
        } else {
            best.as_ref().map_or(usize::MAX, |(sz, ..)| *sz)
        };
        match encode_frame_inner_limited(
            image,
            compression_type,
            block_size,
            mode,
            is_first_frame,
            fq,
            band_steps,
            limit,
        )? {
            Some(data) => {
                if best.as_ref().is_none_or(|(sz, ..)| data.len() < *sz) {
                    best = Some((data.len(), data, Some(mode)));
                }
            }
            None => continue, // Fast-Fail
        }
    }

    // P6 候选顺序优化：第三阶段前移平面化编码竞争（高频胜出 13/14 帧前置，
    // 使后续候选 Fast-Fail 上限更紧）。
    if compression_type == CompressionType::GolombRice && components == 3 {
        let payload = encode_planar_payload(image, compression_type, block_size, fq, band_steps)?;
        let planar = assemble_frame(&payload, image, 0, 3)?;
        if best.as_ref().is_none_or(|(sz, ..)| planar.len() < *sz) {
            best = Some((planar.len(), planar, None));
        }
    }

    // planar 与 CABAC 均可闭环，有损下照常参与竞争；palette 仅限无损低色数场景。
    if !fq.is_lossy() && compression_type == CompressionType::GolombRice {
        // 第三阶段（仅 GolombRice）：条带级自适应预测竞争
        //
        // v1.9 条带高度自适应：32 行恒参与；大图（≥128 行）额外以
        // 64 行条带竞争——平坦插画可摊薄条带头开销并增长共享行程。
        // 胜出高度写入帧头 coding_params（32/64 均 <0x80，不触碰 golden 位）。
        const BAND_HEIGHT_ALT: usize = 64;
        // v1.11：帧级 SAD 最优模式作为条带试编码顺序偏好（Fast-Fail 协同）
        let band_preferred = ranked
            .first()
            .map(|&(_, m)| m)
            .unwrap_or(PredictionMode::Med);
        let mut banded_best: Option<(usize, Vec<u8>, u8)> = None;
        for bh in [BAND_HEIGHT, BAND_HEIGHT_ALT] {
            if bh != BAND_HEIGHT && height < 128 {
                continue; // 小图 64 行条带退化为单条带，无竞争意义
            }
            let payload = encode_banded_payload(image, bh, band_preferred)?;
            let len = payload.len();
            if banded_best.as_ref().is_none_or(|(sz, ..)| len < *sz) {
                banded_best = Some((len, payload, bh as u8));
            }
        }
        if let Some((_, payload, bh)) = banded_best {
            let banded = assemble_frame(&payload, image, bh, 2)?;
            if best.as_ref().is_none_or(|(sz, ..)| banded.len() < *sz) {
                best = Some((banded.len(), banded, None));
            }
        }

        // 第五阶段：调色板候选（frame_type=4）
        //
        // 二次元插画纯色块/低色数数据特化；planar 子平面经递归同样受益。
        // v2 载荷启用 copy-above token 化（coding_params.bit0=1 标记）。
        if palette_plausible(&image.pixels) {
            match encode_palette_payload(&image.pixels, width) {
                Some(Ok(payload)) => {
                    let pal = assemble_frame(&payload, image, 0x01, 4)?;
                    if best.as_ref().is_none_or(|(sz, ..)| pal.len() < *sz) {
                        best = Some((pal.len(), pal, None));
                    }
                }
                Some(Err(e)) => return Err(e),
                None => {} // 色数超限，放弃候选
            }
        }

        // 第八阶段：帧内块复制候选（frame_type=7，v1.11）
        //
        // 首帧自然图像与差分帧的重复纹理特化（服装花纹/背景图案的精确
        // 8×8 重复，hash 链搜索 + LZ 式贪心命中）；全 PRED 退化路径的开销
        // 由字节竞争兜底淘汰，单调不劣化保持。
        // v1.11 差分帧启用：因果完备性修复（PRED 候选剔除 DC/TopRight、
        // 残差流一次性解码）后，差分帧的块级 COPY 同样安全。
        let enable_itbc = std::env::var("CRF_ITBC").map(|v| v == "1").unwrap_or(true);
        if enable_itbc {
            let payload_itbc =
                super::intrabc::encode_intrabc_payload(&image.pixels, width, height, components)?;
            let itbc = assemble_frame(&payload_itbc, image, 0, 7)?;
            if best.as_ref().is_none_or(|(sz, ..)| itbc.len() < *sz) {
                best = Some((itbc.len(), itbc, None));
            }
        }
    }

    // 第六阶段：CABAC 熵编码候选（frame_type=5）
    //
    // 取采样 SAD 最优的预测模式，残差位流改由自适应算术编码承载。
    // escape/商前缀等偏斜分布通常再省 5%~10%。
    if compression_type == CompressionType::GolombRice {
        if let Some(&(_, best_mode)) = ranked.first() {
            // CABAC 候选同样走闭环（有损）或开环（无损），与帧级路径一致。
            // v2 梯度分级上下文：空间域残差流传入 stride 启用因果梯度分级
            let predicted = if fq.is_lossy() {
                crate::crf::format::closed_loop_predict_quant_banded(
                    &image.pixels,
                    width,
                    height,
                    components,
                    best_mode,
                    fq.step,
                    fq.bias,
                    band_steps,
                )
                .0
            } else {
                apply_prediction(&image.pixels, width, height, components, best_mode)
            };
            let stride = width * components;
            // 载荷 v3：[k u8][flags][(ma_tree 头)][cabac 码流]
            // Fast-Fail：当前最佳总长作上限（载荷预算 = 上限 − 帧头 − k 字节）；
            // 超限返回 None → CABAC 必败，静默跳过本候选
            let cabac_limit = best.as_ref().map_or(usize::MAX, |(sz, ..)| {
                sz.saturating_sub(FRAME_HEADER_SIZE + 1)
            });
            let cabac_payload = rle_cabac::encode_frame_rle_cabac_adaptive_limited(
                &predicted,
                Some(stride),
                cabac_limit,
            )?;
            if let Some((payload, k)) = cabac_payload {
                let mut full_payload = Vec::with_capacity(payload.len() + 1);
                full_payload.push(k);
                full_payload.extend_from_slice(&payload);
                let mut cab = assemble_frame(&full_payload, image, k, 5)?;
                // 帧头 pred_mode 记录实际使用的空间预测模式
                let pm_off = FRAME_HEADER_SIZE - 1;
                cab[pm_off] = best_mode as u8;
                if best.as_ref().is_none_or(|(sz, ..)| cab.len() < *sz) {
                    best = Some((cab.len(), cab, Some(best_mode)));
                }
            }
        }
    }

    // 第七阶段：DCT 变换域量化候选（frame_type=6）
    //
    // 对标 JPEG/AVIF 变换编码管线：eff 平面经分块 lifting DCT 变换到
    // 频域后死区标量量化，高频系数量化归零率远高于空间域标量量化。
    // 不做空间预测（DCT 本身即去相关手段），保持路径独立性以便公平竞争。
    // 分量感知：交织数据按分量解包后逐平面变换（解码端对称，见
    // dct_dequantize_inverse_interleaved_bs），杜绝跨分量混叠采样。
    //
    // v1.10 有损四版竞争：{4×4, 8×8} × {flat, 感知矩阵} + Trellis 再竞争。
    // v1.11 无损放开：lifting DCT 严格可逆、Q=1 时量化恒等（逐位无损，
    // 回归锚点 test_dct_quant_roundtrip_q1），故 Q=1 的 DCT+CABAC 是合法
    // 无损候选——探针实测斜线/随机纹理内容较 Med+RLE 小 16%~31%。
    // 无损仅跑 {4×4, 8×8} flat 两版（Q=1 下矩阵/Trellis 均无效化）；
    // 载荷首字节标志位（v1.12 矩形泛化）：
    //   bit5 = 宽度 8 标记、bit7 = 高度 8 标记（组合出 {4×4,8×8,8×4,4×8}）、
    //   bit6 = 感知矩阵、bit4 = Trellis（仅有损）。
    //   bit7 在 v1.11 及更早文件中恒为 0，向后兼容。
    //
    // P6 速度优化（§5-P6）：DCT 候选预筛——残差能量低于 step 的 10%
    // 时跳过全部 DCT 候选。约束由"逐字节一致"放宽为"质量不劣化"：
    // 跳过 DCT 可能改变 Fast-Fail 上限链使产物变化，只要解码质量
    // 不劣化即为有效收益（速度+可能的体积双赢）。纹理/噪声内容
    //（残差能量高）照常竞争。
    let pixel_count = width * height * components;
    let avg_abs_res = ranked
        .first()
        .map(|&(sad, _)| sad as f64 / pixel_count as f64)
        .unwrap_or(0.0);
    let dct_threshold = (fq.step.max(1) as f64) * 0.1;
    let dct_worth = avg_abs_res >= dct_threshold;

    if compression_type == CompressionType::GolombRice && dct_worth {
        const TRELLIS_FLAG_BIT: u8 = 0x10;
        const QM_FLAG_BIT: u8 = 0x40;
        const BW8_FLAG_BIT: u8 = 0x20; // 原 BS8：宽度=8
        const BH8_FLAG_BIT: u8 = 0x80; // v1.12 新启用：高度=8

        let variants: &[(usize, usize, bool)] = if fq.is_lossy() {
            if fq.q1_matrix_scale {
                // v1.12 q95 视觉无损档：Q=1 + 矩阵缩放（高频 Q=2 轻滤），
                // 全部变体强制矩阵（flat+Q1 即恒等无意义）
                &[(4, 4, true), (8, 8, true), (8, 4, true), (4, 8, true)]
            } else {
                // 常规有损：方形四版 + 矩形感知矩阵两版
                &[
                    (4, 4, false),
                    (8, 8, false),
                    (4, 4, true),
                    (8, 8, true),
                    (8, 4, true),
                    (4, 8, true),
                ]
            }
        } else {
            // 无损：Q=1 恒等语义下矩阵/Trellis 无效化，但矩形形状仍有效
            //（v1.13 矩形补齐：{8×4, 4×8} flat 两版加入竞争；
            //  载荷 bit5/bit7 形状码与有损共用，解码端已对称支持）
            &[(4, 4, false), (8, 8, false), (8, 4, false), (4, 8, false)]
        };

        let mut best_dct: Option<(usize, Vec<u8>, usize, usize, bool)> = None;
        for &(block_w, block_h, use_qm) in variants {
            let q_step = if fq.is_lossy() { fq.step.max(1) } else { 1 };
            let q_coeff = super::dct_path::dct_quantize_interleaved_bs(
                &image.pixels,
                width,
                height,
                components,
                q_step,
                block_w,
                block_h,
                use_qm,
                fq.q1_matrix_scale && use_qm,
            );
            // DCT 系数流为非空间域数据，不启用空间域分类器（stride=None）
            // 载荷 v3：[k|形状/矩阵标志 u8][flags=Uniform][cabac 码流]
            let (payload, k) = rle_cabac::encode_frame_rle_cabac_adaptive(&q_coeff, None)?;
            let mut flag_byte = k;
            if block_w == 8 {
                flag_byte |= BW8_FLAG_BIT;
            }
            if block_h == 8 {
                flag_byte |= BH8_FLAG_BIT;
            }
            if use_qm {
                flag_byte |= QM_FLAG_BIT;
            }
            let mut full_payload = Vec::with_capacity(payload.len() + 1);
            full_payload.push(flag_byte);
            full_payload.extend_from_slice(&payload);
            let len = FRAME_HEADER_SIZE + full_payload.len();
            if best_dct.as_ref().is_none_or(|(sz, ..)| len < *sz) {
                best_dct = Some((len, full_payload, block_w, block_h, use_qm));
            }
        }

        // Trellis 再竞争：仅有损且非 q95 档（Q=1 时 Trellis 短路无意义）
        if fq.is_lossy() && !fq.q1_matrix_scale {
            if let Some((len, payload_bs, block_w, block_h, use_qm)) = best_dct {
                let mut final_payload = payload_bs;
                if use_qm {
                    let table: &[u32] = match (block_w, block_h) {
                        (8, 8) => &super::dct_path::qm::DCT_PERCEPTUAL_QM8,
                        (8, 4) => &super::dct_path::qm::DCT_PERCEPTUAL_QM_WIDE,
                        (4, 8) => &super::dct_path::qm::DCT_PERCEPTUAL_QM_TALL,
                        _ => &super::dct_path::qm::DCT_PERCEPTUAL_QM,
                    };
                    let t_coeff = trellis_quantize_interleaved(
                        &image.pixels,
                        width,
                        height,
                        components,
                        fq.step,
                        block_w,
                        block_h,
                        table,
                    );
                    let (payload, k) = rle_cabac::encode_frame_rle_cabac_adaptive(&t_coeff, None)?;
                    let flag_byte = k
                        | TRELLIS_FLAG_BIT
                        | QM_FLAG_BIT
                        | if block_w == 8 { BW8_FLAG_BIT } else { 0 }
                        | if block_h == 8 { BH8_FLAG_BIT } else { 0 };
                    let mut full_payload = Vec::with_capacity(payload.len() + 1);
                    full_payload.push(flag_byte);
                    full_payload.extend_from_slice(&payload);
                    if FRAME_HEADER_SIZE + full_payload.len() < len {
                        final_payload = full_payload;
                    }
                }

                let k = final_payload[0] & 0x0F;
                let dct_f = assemble_frame(&final_payload, image, k, 6)?;
                if best.as_ref().is_none_or(|(sz, ..)| dct_f.len() < *sz) {
                    best = Some((dct_f.len(), dct_f, None));
                }
            }
        } else if let Some((_, final_payload, _, _, _)) = best_dct {
            // 无损路径：无 Trellis 轮，直接采用竞争胜者
            let k = final_payload[0] & 0x0F;
            let dct_f = assemble_frame(&final_payload, image, k, 6)?;
            if best.as_ref().is_none_or(|(sz, ..)| dct_f.len() < *sz) {
                best = Some((dct_f.len(), dct_f, None));
            }
        }
        // 第八阶段：预测后变换 + CABAC 系数编码（frame_type=8，v1.14）
        //
        // DC 预测模式 → transform skip（直通量化，避免 DCT 能量扩散）；
        // H/V/MED 模式 → 8×8 lifting DCT → 量化 → zigzag。
        // 系数流走 CABAC run-level 编码（3 上下文 + 直通 sign/余数）。
        // 仅 3 分量时参与（单分量 Gray 用前序帧级候选）。
        if compression_type == CompressionType::GolombRice && components == 3 {
            let payload_tf8 = encode_intra_transform_payload(
                image,
                compression_type,
                fq.step,
                fq.bias,
                fq.chroma_step,
                fq.chroma_bias,
            )?;
            let tf8 = assemble_frame(&payload_tf8, image, 0, 8)?;
            if best.as_ref().is_none_or(|(sz, ..)| tf8.len() < *sz) {
                best = Some((tf8.len(), tf8, None));
            }
        }
    }

    best.map(|(_, data, mode)| AdaptiveOutput {
        data,
        pred_mode: mode,
    })
    .ok_or_else(|| CrfError::InvalidCodingParams("no adaptive prediction candidate".to_string()))
}

/// 调色板最大颜色数
const PALETTE_MAX_COLORS: usize = 256;

/// 抽样快速判断数据是否可能为低色数（粗筛；误放行由精确阶段兜底）
fn palette_plausible(pixels: &[i32]) -> bool {
    use std::collections::HashSet;
    let mut set: HashSet<i32> = HashSet::with_capacity(300);
    for v in pixels.iter().step_by(7) {
        if set.insert(*v) && set.len() > PALETTE_MAX_COLORS {
            return false;
        }
    }
    true
}

/// 调色板编码（frame_type=4）：二次元插画低色数数据特化
///
/// 数据唯一值 > PALETTE_MAX_COLORS 时返回 None（放弃该候选）。
/// 载荷布局：
/// [palette_count u16 LE][k_index u8][pal_len u32 LE]
/// [palette 值位流 pal_len 字节（exp-Golomb zigzag）]
/// [索引位流（剩余字节，RLE+Golomb 自适应 k）]
fn encode_palette_payload(pixels: &[i32], width: usize) -> Option<CrfResult<Vec<u8>>> {
    use std::collections::HashMap;

    // 精确统计唯一值并按首现顺序构建调色板与索引流
    let mut map: HashMap<i32, u16> = HashMap::with_capacity(512);
    let mut palette_order: Vec<i32> = Vec::new();
    let mut indices: Vec<i32> = Vec::with_capacity(pixels.len());
    for &v in pixels {
        let next = map.len() as u16;
        match map.get(&v) {
            Some(&ix) => indices.push(ix as i32),
            None => {
                if next as usize >= PALETTE_MAX_COLORS {
                    return None; // 超出色数上限，放弃候选
                }
                map.insert(v, next);
                palette_order.push(v);
                indices.push(next as i32);
            }
        }
    }

    #[allow(clippy::redundant_closure_call)] // IIFE 为复用 ? 早退语义的最小作用域包装
    Some((|| {
        // copy-above token 化（AV1 palette 思路适配）：水平恒定区域 →
        // token 0 长行程，RLE 行程机制直接复用；非零为显式索引+1（零开销）。
        let use_copy_above = width > 0 && indices.len() > width;
        let payload_indices: Vec<i32> = if use_copy_above {
            indices
                .iter()
                .enumerate()
                .map(|(i, &ix)| {
                    if i >= width && indices[i - width] == ix {
                        0
                    } else {
                        ix + 1
                    }
                })
                .collect()
        } else {
            indices.clone()
        };

        // token/索引流先行构建以获取 k（头部需要）
        let mut probe = super::rle_golomb::RleGolombEncoder::adaptive(&payload_indices);
        let k = probe.k;
        probe.encode_signed_array(&payload_indices);
        let idx_bytes = probe.finish();

        // palette 值流：exp-Golomb(zigzag)，无需额外参数
        let mut pe = super::exp_golomb::ExpGolombEncoder::new();
        for &v in &palette_order {
            pe.encode_signed(v);
        }
        let pal_bytes = pe.finish();

        // 布局 v2（与 decoder/palette.rs 对称）：
        // [count u16 LE][flags u8][k u8][pal_len u32 LE][pal_bytes][idx_bytes]
        // flags.bit0 = copy-above 模式启用；帧头 coding_params.bit0 同步标记
        let flags: u8 = if use_copy_above { 0x01 } else { 0x00 };
        let mut out = Vec::with_capacity(8 + pal_bytes.len() + idx_bytes.len());
        out.extend_from_slice(&(palette_order.len() as u16).to_le_bytes());
        out.push(flags);
        out.push(k);
        out.extend_from_slice(&(pal_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&pal_bytes);
        out.extend_from_slice(&idx_bytes);
        Ok(out)
    })())
}

/// 测试钩子（仅 cfg(test) 可见）：暴露调色板载荷编码供集成测试对比
#[cfg(test)]
pub(crate) mod test_hooks {
    /// palette v2 载荷编码（copy-above token 化）
    pub(crate) fn encode_palette_payload_for_test(
        pixels: &[i32],
        width: usize,
    ) -> Option<crate::crf::error::CrfResult<Vec<u8>>> {
        super::encode_palette_payload(pixels, width)
    }

    /// 基线：原始索引流直接 RLE+Golomb（无 copy-above）的位流大小
    pub(crate) fn palette_index_baseline_size(indices: &[i32]) -> usize {
        let mut probe = crate::crf::encoder::rle_golomb::RleGolombEncoder::adaptive(indices);
        probe.encode_signed_array(indices);
        probe.finish().len()
    }
}
