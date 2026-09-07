//! 逐帧自适应预测模式决策与候选竞争（frame_type 多路仲裁核心）
//!
//! 决策流水线：
//! 1. 4×4 Hadamard SATD 排序全部候选模式；
//! 2. top-2 候选真实熵编码取最小（帧级路径）；
//! 3. 无损模式下与条带(2)/三平面(3)/调色板(4)竞争；
//! 4. CABAC 算术编码候选(5)对最优模式重新编码。
//!
//! 所有候选按"字节最小者胜出"，保证相对任何单一路径单调不劣化。

use rayon::prelude::*;

use crate::crf::core::bitstream::constants::{BAND_HEIGHT, FRAME_HEADER_SIZE};
use crate::crf::core::domain::{CompressionType, ImageData, PredictionMode};
use crate::crf::core::prediction::cost::residual_activity_for_mode_sampled;
use crate::crf::core::prediction::cost::satd_for_mode_sampled;
use crate::crf::core::prediction::intra::apply_prediction_into;
use crate::crf::core::transform::closed_loop::closed_loop_predict_quant_banded_into;
use crate::crf::encoder::banded::encode_banded_payload;
use crate::crf::encoder::intra_transform::encode_intra_transform_payload;
use crate::crf::encoder::planar::encode_planar_payload_limited;
use crate::crf::error::{CrfError, CrfResult};

use super::super::rle_cabac;
use super::super::scratch::FrameScratch;
use super::BandSteps;
use super::{assemble_frame, FrameQuant};
use crate::crf::core::transform::rdoq::trellis_quantize_interleaved;

use crate::crf::performance::telemetry::Span;

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
/// 覆盖二次元插画高频出现的 ±45° 线条走向；SATD 排序自动纳入竞争。
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

/// 编码单帧（逐帧自适应预测模式选择）
///
/// 两阶段决策：
/// 1. 对全部候选模式计算采样 4×4 Hadamard SATD，按代价排序；
/// 2. 仅对 SATD 最小的 2 个候选执行真实熵编码，采用字节数最小者。
///
/// 相比全候选试编码，开销约为 8 次采样 SATD + 2 次编码，
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
    // 第一阶段：4×4 Hadamard SATD 预筛（仅用于排序）。
    // v1.11：8 候选模式 rayon 并行求值（纯函数无共享状态），
    // 多核下决策耗时近似减半以上。
    let width = image.width as usize;
    let height = image.height as usize;
    let components = image.color_format.component_count();

    let satd_span = Span::begin("encode.adaptive.satd");
    let mut ranked: Vec<(u64, PredictionMode)> = ADAPTIVE_CANDIDATES
        .par_iter()
        .map(|&m| {
            (
                satd_for_mode_sampled(&image.pixels, width, height, components, m),
                m,
            )
        })
        .collect();
    ranked.sort_by_key(|&(satd, _)| satd);
    drop(satd_span);

    // 历史引导：前一帧胜出的预测模式优先进入试编码集
    if let Some(pm) = preferred_mode {
        if let Some(pos) = ranked.iter().position(|&(_, m)| m == pm) {
            let e = ranked.remove(pos);
            ranked.insert(0, e);
        }
    }

    // SATD 只负责排序；候选启用阈值继续使用像素域平均绝对残差，
    // 避免把 Hadamard 频域增益误当成像素幅度。
    let avg_abs_res = ranked
        .first()
        .map(|&(_, mode)| {
            residual_activity_for_mode_sampled(&image.pixels, width, height, components, mode)
        })
        .unwrap_or(0.0);
    let dct_threshold = (fq.step.max(1) as f64) * 0.1;
    let dct_worth = avg_abs_res >= dct_threshold;

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
    let mut frame_scratch = FrameScratch::default();
    let trial_span = Span::begin("encode.adaptive.trial_encode");
    for (attempt, &(_, mode)) in ranked.iter().take(2).enumerate() {
        // 第二名候选的上限 = 第一名总长（含帧头）：熵码流字节单调递增，
        // 超限即必败，被淘汰候选的产物本就不会进入最终码流
        let limit = if attempt == 0 {
            usize::MAX
        } else {
            best.as_ref().map_or(usize::MAX, |(sz, ..)| *sz)
        };
        match super::encode_frame_inner_limited_with_scratch(
            image,
            compression_type,
            block_size,
            mode,
            is_first_frame,
            fq,
            band_steps,
            limit,
            &mut frame_scratch,
        )? {
            Some(data) => {
                if best.as_ref().is_none_or(|(sz, ..)| data.len() < *sz) {
                    best = Some((data.len(), data, Some(mode)));
                }
            }
            None => continue, // Fast-Fail
        }
    }
    drop(trial_span);

    // P6 候选顺序优化：第三阶段前移平面化编码竞争（高频胜出 13/14 帧前置，
    // 使后续候选 Fast-Fail 上限更紧）。
    if compression_type == CompressionType::GolombRice && components == 3 {
        let planar_span = Span::begin("encode.adaptive.planar");
        // Fast-Fail 预算：planar 仅需击败当前最佳总长；超预算即必败。
        let planar_limit = best.as_ref().map_or(usize::MAX, |(sz, ..)| *sz);
        if let Some(payload) = encode_planar_payload_limited(
            image,
            compression_type,
            block_size,
            fq,
            band_steps,
            planar_limit,
        )? {
            let planar = assemble_frame(&payload, image, 0, 3)?;
            if best.as_ref().is_none_or(|(sz, ..)| planar.len() < *sz) {
                best = Some((planar.len(), planar, None));
            }
        }
        drop(planar_span);
    }

    // planar 与 CABAC 均可闭环，有损下照常参与竞争；palette 仅限无损低色数场景。
    if !fq.is_lossy() && compression_type == CompressionType::GolombRice {
        // 第三阶段（仅 GolombRice）：条带级自适应预测竞争
        //
        // v1.9 条带高度自适应：32 行恒参与；大图（≥128 行）额外以
        // 64 行条带竞争——平坦插画可摊薄条带头开销并增长共享行程。
        // 胜出高度写入帧头 coding_params（32/64 均 <0x80，不触碰 golden 位）。
        const BAND_HEIGHT_ALT: usize = 64;
        // 帧级 SATD 最优模式作为条带试编码顺序偏好（Fast-Fail 协同）
        let band_preferred = ranked
            .first()
            .map(|&(_, m)| m)
            .unwrap_or(PredictionMode::Med);
        let banded_span = Span::begin("encode.adaptive.banded");
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
        drop(banded_span);

        // 第五阶段：调色板候选（frame_type=4）
        //
        // 二次元插画纯色块/低色数数据特化；planar 子平面经递归同样受益。
        // v2 载荷启用 copy-above token 化（coding_params.bit0=1 标记）。
        let palette_span = Span::begin("encode.adaptive.palette");
        if palette_plausible(&image.pixels, components) {
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
        drop(palette_span);

        // 第八阶段：帧内块复制候选（frame_type=7，v1.11）
        //
        // 首帧自然图像与差分帧的重复纹理特化（服装花纹/背景图案的精确
        // 8×8 重复，hash 链搜索 + LZ 式贪心命中）；全 PRED 退化路径的开销
        // 由字节竞争兜底淘汰，单调不劣化保持。
        // D3 差分帧剪枝（2026-09-08）：`--probe-rest-frames` 实测 44/44 帧
        // 差分帧（RCT 残差域）intrabc 胜出 0 次、176 次调用累计 49.3s——
        // 差分残差经 RCT 去相关后已无 8×8 精确重复纹理（同一纹理差分后
        // 归一为像素差，块签名不再命中），IntraBC 从不 `len < best`。
        // 跳过不改变任何候选的 `best`、不影响后续候选 Fast-Fail 上限链，
        // 字节逐字节一致（探针保留为回归锚点）。首帧（原始插画、重复花纹
        // 密集）保留——那是 IntraBC 的专属场景。
        let enable_itbc = std::env::var("CRF_ITBC").map(|v| v == "1").unwrap_or(true);
        let itbc_span = Span::begin("encode.adaptive.intrabc");
        // P1b 子平面剪枝：单分量（planar 子平面，生产路径唯一 components==1
        // 场景）经 RCT+CfL 去相关后已无 8×8 精确重复纹理，IntraBC 的块复制
        // 命中率为零。探针实测全部测试组子平面 intrabc 胜出 0 次——跳过不
        // 改变任何候选的 `best`（它从不 set best），字节透明。
        if enable_itbc && is_first_frame && components > 1 {
            let payload_itbc =
                super::intrabc::encode_intrabc_payload(&image.pixels, width, height, components)?;
            let itbc = assemble_frame(&payload_itbc, image, 0, 7)?;
            if best.as_ref().is_none_or(|(sz, ..)| itbc.len() < *sz) {
                best = Some((itbc.len(), itbc, None));
            }
        }
        drop(itbc_span);
    }

    // 第六阶段：CABAC 熵编码候选（frame_type=5）
    //
    // 取采样 SATD 最优的预测模式，残差位流改由自适应算术编码承载。
    // escape/商前缀等偏斜分布通常再省 5%~10%。
    // P6 预筛（§5-P6）：残差能量极低时 CABAC 无法改善平面化候选
    //（RLE 对零行程已最优），跳过闭环预测以节省时间。
    if compression_type == CompressionType::GolombRice {
        let cabac_span = Span::begin("encode.adaptive.cabac");
        if let Some(&(_, best_mode)) = ranked.first().filter(|_| avg_abs_res >= 0.5) {
            // CABAC 候选同样走闭环（有损）或开环（无损），与帧级路径一致。
            // v2 梯度分级上下文：空间域残差流传入 stride 启用因果梯度分级
            let predicted: &[i32] = if fq.is_lossy() {
                let (residuals, reconstruction) = frame_scratch.closed_loop(image.pixels.len());
                closed_loop_predict_quant_banded_into(
                    &image.pixels,
                    residuals,
                    reconstruction,
                    width,
                    height,
                    components,
                    best_mode,
                    fq.step,
                    fq.bias,
                    band_steps,
                );
                residuals
            } else {
                let residuals = frame_scratch.residuals(image.pixels.len());
                apply_prediction_into(
                    &image.pixels,
                    residuals,
                    width,
                    height,
                    components,
                    best_mode,
                );
                residuals
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
                // 帧头 pred_mode 记录实际使用的空间预测模式。
                // v1.15 帧头 12B：pred_mode 位于 data[10]（FRAME_HEADER_SIZE-2），
                // data[11] 为 reference_type——旧写法 FRAME_HEADER_SIZE-1 会
                // 误写 reference_type 且使 pred_mode 恒为 PRED_MODE_UNSET，
                // 导致解码端预测撤销失效（编解码不对称）。
                let pm_off = FRAME_HEADER_SIZE - 2;
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
    //
    // P1b 子平面剪枝（lossless 单分量）：planar 子平面经 RCT+CfL 去相关
    // 后已无频域能量可聚集，探针实测全部测试组 lossless 子平面 DCT 胜出
    // 0 次（cabac 对残差流已最优）。跳过不改变 `best`（DCT 从不 set best），
    // 字节透明。有损单分量（色度 chroma_step 量化）仍保留 DCT——量化下
    // 变换域可能真实胜出，无探针证据，不剪。
    //
    // ⚠ D4 已回退（2026-09-08）：曾尝试对无损差分帧剪 dct——`--probe-rest-frames`
    // 每组仅采样前 2 差分帧，dct 表面 0 胜出；但 2-12-4 后续帧 dct 真实胜出，
    // 剪枝后该组字节 +2.2%。探针采样不完整，dct 不满足「从不 set best」前提，
    // 维持原状（首帧+差分帧均参与竞争，单调不劣化由竞争兜底）。
    let skip_dct_subplane = components == 1 && !fq.is_lossy();
    if compression_type == CompressionType::GolombRice && dct_worth && !skip_dct_subplane {
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
        let dct_span = Span::begin("encode.adaptive.dct");
        let q_step = if fq.is_lossy() { fq.step.max(1) } else { 1 };
        // P1 首帧加速：DCT 各变体（lossless 4 版 / 常规有损 6 版）互相独立——
        // 各自算自己的系数流与 CABAC 载荷，`best_dct` 取最小、无跨变体
        // Fast-Fail、无字节依赖。首帧为串行主瓶颈（探针实测 lossless dct
        // 611ms），并行化使 dct 阶段随线程数近线性缩放。rayon collect 保序 +
        // 后续同序严格 `<` 比较，tie-break 与串行版逐字节一致。
        let dct_results: Vec<(usize, Vec<u8>, usize, usize, bool)> = variants
            .par_iter()
            .map(
                |&(block_w, block_h, use_qm)| -> CrfResult<(usize, Vec<u8>, usize, usize, bool)> {
                    let q_coeff = super::super::dct_path::dct_quantize_interleaved_bs(
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
                    Ok((len, full_payload, block_w, block_h, use_qm))
                },
            )
            .collect::<CrfResult<Vec<_>>>()?;
        for (len, full_payload, block_w, block_h, use_qm) in dct_results {
            if best_dct.as_ref().is_none_or(|(sz, ..)| len < *sz) {
                best_dct = Some((len, full_payload, block_w, block_h, use_qm));
            }
        }
        drop(dct_span);

        // Trellis 再竞争：仅有损且非 q95 档（Q=1 时 Trellis 短路无意义）
        if fq.is_lossy() && !fq.q1_matrix_scale {
            if let Some((len, payload_bs, block_w, block_h, use_qm)) = best_dct {
                let mut final_payload = payload_bs;
                if use_qm {
                    let table: &[u32] = match (block_w, block_h) {
                        (8, 8) => &crate::crf::core::transform::qm::DCT_PERCEPTUAL_QM8,
                        (8, 4) => &crate::crf::core::transform::qm::DCT_PERCEPTUAL_QM_WIDE,
                        (4, 8) => &crate::crf::core::transform::qm::DCT_PERCEPTUAL_QM_TALL,
                        _ => &crate::crf::core::transform::qm::DCT_PERCEPTUAL_QM,
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
                        crate::crf::core::transform::rdoq::DEFAULT_LAMBDA_NUM,
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
        // P6 预筛：同 DCT 指标——平坦内容时跳过（DCT 无收益则 frame_type=8 也无）。
        if compression_type == CompressionType::GolombRice && components == 3 && dct_worth {
            let itrans_span = Span::begin("encode.adaptive.intra_transform");
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
            drop(itrans_span);
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

/// 像素级抽样唯一色数上限：palette 的竞争力来自「低色数 + 平坦区 RLE 长行程」。
/// 当像素级(三元组)唯一色数超过此宽松上界时，索引流需要 log2(N) 位/像素、
/// 且表体积随 N 增长，在真实插画（数万~百万色）中必然劣于预测+熵编码候选。
/// 抽样阶段超过即快速放弃（探针 K1：真实差分帧分量级唯一 320~506 全部 >256，
/// 首帧恰 =256 使旧实现每帧完整执行全帧 HashMap + 索引流编码，而 palette 在
/// 这些数据上从不参与竞争）。阈值取宽松上界、仅剪「从分量级与像素级角度均
/// 无可能胜出」的帧，误放行由精确阶段兜底，产物字节由端到端锚点锁定。
const PALETTE_SAMPLE_PIXEL_LIMIT: usize = 4096;

/// 抽样快速判断数据是否可能为低色数（粗筛；误放行由精确阶段兜底）
fn palette_plausible(pixels: &[i32], components: usize) -> bool {
    use std::collections::HashSet;
    let mut set: HashSet<i32> = HashSet::with_capacity(300);
    // 像素级抽样三元组集合（仅 components>1 时启用）
    let mut px_set: HashSet<u64> = HashSet::with_capacity(256);
    let px_stride = components.max(1);
    let mut i = 0usize;
    while i < pixels.len() {
        let v = pixels[i];
        if set.insert(v) && set.len() > PALETTE_MAX_COLORS {
            return false;
        }
        if px_stride > 1 && i % px_stride == 0 {
            // 当前像素起点：取 px_stride 个分量编码为 u64 键（低 16 位线性组合）
            let mut key = 0u64;
            for &c in pixels[i..].iter().take(px_stride).take(4) {
                key = key
                    .wrapping_mul(0x1_0000_01B3)
                    .wrapping_add(c as u64 & 0xFFFF);
            }
            if px_set.insert(key) && px_set.len() > PALETTE_SAMPLE_PIXEL_LIMIT {
                return false;
            }
        }
        i += 7;
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
        let mut probe = super::super::rle_golomb::RleGolombEncoder::adaptive(&payload_indices);
        let k = probe.k;
        probe.encode_signed_array(&payload_indices);
        let idx_bytes = probe.finish();

        // palette 值流：exp-Golomb(zigzag)，无需额外参数
        let mut pe = super::super::exp_golomb::ExpGolombEncoder::new();
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
