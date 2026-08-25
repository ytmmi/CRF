//! 序列编码主流程：encode_sequence
//!
//! 职责：输入校验 → 文件头/索引布局 → RCT 色彩去相关 → 有损量化档位解析 →
//! 按输入语义分路径编码（G=原始帧 golden 差分 / C=预差分兼容）→ 文件组装。
//!
//! 路径 G（input_original_frames=true，推荐）：
//!   frames 即原始帧序列，全部差分至首帧（golden 参考）。
//!   量化误差不沿链累积；帧间零依赖 → 全并行；任意帧可独立解码。
//!
//! 路径 C（兼容）：frames[0]=首帧原图，frames[1..]=预差分残差帧。
//!   保持旧约定；有损时误差沿链累积（建议配合 keyframe_interval 缓解）。

use rayon::prelude::*;

use crate::crf::checksum::crc32;
use crate::crf::error::{CrfError, CrfResult};
use crate::crf::format::{
    CompressionType, CrfHeader, EncodeParams, Flags, ImageData, FOOTER_MAGIC, FOOTER_SIZE,
    FRAME_HEADER_SIZE, HEADER_SIZE,
};

use super::adaptive::encode_frame_adaptive;
use super::frame::{encode_frame, FrameQuant};

/// 编码完整的 CRF 文件
///
/// 输入：图像序列和编码参数
/// 输出：完整的 CRF 文件数据
pub fn encode_sequence(frames: &[ImageData], params: &EncodeParams) -> CrfResult<Vec<u8>> {
    // 验证输入
    if frames.is_empty() {
        return Err(CrfError::FrameCountOutOfRange(0));
    }

    let frame_count = frames.len() as u16;
    // 批量接口保持 50 帧内存上限（全帧驻留）；>50 帧请使用
    // streaming::StreamingEncoder（逐帧推送，内存 O(golden+单帧+码流)）
    if !(2..=50).contains(&frame_count) {
        return Err(CrfError::FrameCountOutOfRange(frame_count));
    }

    // 验证所有帧尺寸一致
    let first = &frames[0];
    for (i, frame) in frames.iter().enumerate().skip(1) {
        if frame.width != first.width || frame.height != first.height {
            return Err(CrfError::ImageDimensionsMismatch {
                expected: (first.width, first.height),
                actual: (frame.width, frame.height),
            });
        }
        if frame.bit_depth != first.bit_depth {
            return Err(CrfError::InvalidCodingParams(format!(
                "Frame {} bit depth mismatch",
                i
            )));
        }
    }

    // 确定压缩类型
    let compression_type = match params.compression_type.as_str() {
        "golomb-rice" | "golomb" => CompressionType::GolombRice,
        "exp-golomb" | "exp_golomb" | "egc" => CompressionType::ExpGolomb,
        "transform" | "dct" => CompressionType::Transform,
        _ => {
            return Err(CrfError::InvalidCodingParams(
                params.compression_type.clone(),
            ))
        }
    };

    // 构建文件头
    let mut header = CrfHeader::new(
        frame_count,
        first.width,
        first.height,
        first.bit_depth,
        first.color_format,
        compression_type,
    );
    header.block_size = params.block_size.unwrap_or(8) as u16;
    header.prediction_mode = params.prediction_mode;

    // 设置帧索引标志
    let mut flags = Flags::new();
    flags.set_has_index(true);
    header.flags = flags;

    // 可逆色彩变换（YCoCg-R）：参考 AV1 / HEVC RExt / JPEG-XL 无损模式的做法。
    // 对 3 分量格式在空间预测之前先去除 RGB 通道相关性，
    // 使残差能量集中于亮度通道；纯整数运算、严格可逆。
    let components = first.color_format.component_count();
    let use_rct = crate::crf::format::rct_applicable(components);
    let encode_frames: Vec<ImageData> = if use_rct {
        frames
            .iter()
            .map(|f| {
                let transformed = crate::crf::format::rct_forward(&f.pixels, components)?;
                Ok(ImageData {
                    width: f.width,
                    height: f.height,
                    bit_depth: f.bit_depth,
                    color_format: f.color_format,
                    pixels: transformed,
                })
            })
            .collect::<CrfResult<Vec<_>>>()?
    } else {
        frames.to_vec()
    };
    header.flags.set_has_rct(use_rct);

    // 设置用户数据
    if let Some(ref user_data) = params.user_metadata {
        header.user_data = user_data.clone();
    }

    // 验证文件头
    header.validate()?;

    // 真有损：质量档位映射为量化步长，写入文件头（flags.bit2 + byte21）
    let lossy_quant_step = params
        .lossy_quality
        .map(crate::crf::format::quant_step_from_quality);
    if let Some(q) = lossy_quant_step {
        header.lossy_quant = q;
        header.flags.set_has_lossy_quant(true);
    }

    // 真有损精细调参（None → 默认：色度×130%、关键帧间隔 10、无死区偏置）
    let tuning = crate::crf::format::LossyTuning::resolve(params.lossy_tuning.as_ref());
    let interval = tuning.keyframe_interval.max(1) as usize;
    let base_bias = tuning.deadzone_bias;

    // 计算帧索引偏移量
    let index_size = frame_count as usize * 8; // 每个索引项 8 字节
    let header_end = HEADER_SIZE;
    let frames_start = header_end + index_size;

    // 编码所有帧
    let mut encoded_frames = Vec::with_capacity(frames.len());
    let mut current_offset = frames_start as u32;

    // 按帧序号解析量化配置：
    // golden 首帧（i==0）强制无损——它是全部差分帧的还原基准，
    // 其量化误差会传导进每一帧的重建结果，必须像素级精确；
    // 间隔锚点帧按 anchor_quality_percent 折算步长（100=与普通帧同档，
    // 对标 AVIF 全帧统一 CRF；<100 锚点更高精度；0=锚点无损）。
    // v1.12：q95 视觉无损档（is_q95_perceptual）时差分帧携带矩阵缩放许可；
    // golden 首帧默认强制无损，golden_lossless=false 时按锚点档位量化。
    let q95 = params
        .lossy_quality
        .map(crate::crf::format::quant::is_q95_perceptual)
        .unwrap_or(false);
    let fq_for_index = |i: usize| -> FrameQuant {
        let global_q = lossy_quant_step.unwrap_or(0);
        if global_q == 0 {
            return FrameQuant::lossless();
        }
        if i == 0 && tuning.golden_lossless {
            return FrameQuant::lossless();
        }
        let is_anchor = i.is_multiple_of(interval);
        let step = if is_anchor {
            tuning.anchor_step(global_q)
        } else {
            global_q
        };
        FrameQuant {
            step,
            bias: base_bias,
            chroma_step: tuning.chroma_step(step),
            // P1 色度精细化：色度死区偏置独立通道（None → 继承全局偏置）
            chroma_bias: tuning.chroma_deadzone_bias.unwrap_or(base_bias),
            // P1 4:2:0 解耦（规划 §4.1）：色度半分辨率不再受 step>1 阻断，
            // 由 chroma_half_res 参数独立决定；planar 载荷内 ss_flags.bit0
            // 携带该标志，解码对称不受影响；无损锚点经 is_lossy() 自动关闭。
            chroma_half_res: tuning.chroma_half_res,
            q1_matrix_scale: q95,
        }
    };
    let all_results: Vec<(Vec<u8>, Option<u8>, bool)> = if params.input_original_frames {
        // ===== 路径 G（P0 两阶段闭环编码）=====
        // 规范 §5.3：有损 golden 时后续残差必须以文件实际可用的重建首帧
        // G_hat = rct⁻¹(decode_frame(encode(frame0))) 为参考，而不是原始
        // frame0——否则解码端 "G_hat + residual" 的还原式会把首帧量化
        // 误差传导进全部后续帧。两阶段结构：先串行编码 frame0 并本地
        // 解码重建 G_hat；再以 G_hat 为基准并行生成与编码其余残差帧，
        // 保持 golden 残差帧之间的并行性与随机访问能力。
        //
        // golden 差分在 RGB 域进行（与解码端 rct_inverse 出口同域，
        // 避免 RCT 移位舍入的非线性差异破坏逐位一致性）
        let noise_on = lossy_quant_step.is_some() && tuning.noise_adaptive && components == 3;
        // v1.12 q95 视觉无损档的空间域轻滤：|v| ≤ 1 的差分归零。
        // ±1 是插画差分最高频的小幅值（抗锯齿边缘/渐变过渡），
        // 归零后 RLE 零行程显著增长；视觉影响趋近于零（±1 灰度差
        // 低于人眼 JND）。golden_lossless=false 时首帧同样轻滤，
        // 保证超小序列（2 帧）下 q95 与无损可区分。
        let q95_soft = q95 && lossy_quant_step.is_some();
        let soft1 = |buf: &mut [i32]| {
            for v in buf.iter_mut() {
                if *v == 1 || *v == -1 {
                    *v = 0;
                }
            }
        };

        // ===== 阶段 1a：frame0 差分域输入构造（i==0 时差分即原帧本身；
        // q95_soft 轻滤语义保持不变——轻滤后的首帧就是文件中的真实内容）=====
        let mut first_diff_rgb = frames[0].pixels.clone();
        if q95_soft {
            soft1(&mut first_diff_rgb);
        }
        // v1.13 RCT 首帧自适应：保留轻滤后的 RGB 原域副本（bypass 候选），
        // 与 RCT 域版本做双路完整管线竞争。
        let first_bypass_rgb = if use_rct {
            Some(first_diff_rgb.clone())
        } else {
            None
        };
        let first_eff_pixels = crate::crf::format::rct_forward(&first_diff_rgb, components)?;
        let first_eff_frame = ImageData {
            width: frames[0].width,
            height: frames[0].height,
            bit_depth: frames[0].bit_depth,
            color_format: frames[0].color_format,
            pixels: first_eff_pixels,
        };
        drop(first_diff_rgb);

        // ===== 阶段 1b：frame0 编码（fq_for_index(0) 决定无损/有损档位；
        // 双路竞争字节最小者胜出，平局保守保持 RCT 版）=====
        let fq_first = fq_for_index(0);
        let mut data_first = if params.adaptive_prediction {
            encode_frame_adaptive(
                &first_eff_frame,
                compression_type,
                header.block_size,
                false, // 与既有路径 G 行为一致：批量接口不携带首帧语义
                fq_first,
                None,
                None,
            )?
            .data
        } else {
            encode_frame(
                &first_eff_frame,
                compression_type,
                header.block_size,
                header.prediction_mode,
                false,
                fq_first,
                None,
            )?
        };
        let mut first_no_rct = false;
        if let Some(bypass) = first_bypass_rgb {
            let bypass_img = ImageData {
                width: first_eff_frame.width,
                height: first_eff_frame.height,
                bit_depth: first_eff_frame.bit_depth,
                color_format: first_eff_frame.color_format,
                pixels: bypass,
            };
            let bypass_data = if params.adaptive_prediction {
                encode_frame_adaptive(
                    &bypass_img,
                    compression_type,
                    header.block_size,
                    false,
                    fq_first,
                    None,
                    None,
                )?
                .data
            } else {
                encode_frame(
                    &bypass_img,
                    compression_type,
                    header.block_size,
                    header.prediction_mode,
                    false,
                    fq_first,
                    None,
                )?
            };
            if bypass_data.len() < data_first.len() {
                data_first = bypass_data;
                first_no_rct = true;
            }
        }
        if first_no_rct {
            header.flags.set_first_frame_no_rct(true);
        }
        let first_result: (Vec<u8>, Option<u8>, bool) = (data_first.clone(), None, false);

        // ===== 阶段 1c：本地闭环重建 G_hat =====
        // 复用解码端同一 decode_frame 入口与最终文件头上下文（flags 含
        // has_rct / first_frame_no_rct / lossy_quant），保证编码端本地
        // 重建与文件自包含解码逐位一致——这是闭环语义的定义本身。
        let mut g_hat_img = crate::crf::decoder::decode_frame(&data_first, &header)?;
        if header.flags.has_rct() && !header.flags.first_frame_no_rct() {
            g_hat_img.pixels = crate::crf::format::rct_inverse(&g_hat_img.pixels, components)?;
        }
        let g_hat = g_hat_img.pixels; // RGB 域重建首帧

        // ===== 阶段 2：后续帧并行差分编码（残差 = 原始帧 − G_hat）=====
        // v1.13 RCT 首帧自适应的双路竞争仅属于首帧；差分帧逻辑保持原样。
        let rest_results: Vec<(Vec<u8>, Option<u8>, bool)> = frames
            .par_iter()
            .enumerate()
            .skip(1)
            .map(|(i, frame)| -> CrfResult<(Vec<u8>, Option<u8>, bool)> {
                // P0 闭环核心：差分基准为本地重建的 G_hat（而非 frames[0]）。
                // 无损 golden 时 G_hat == frames[0]（decode 精确还原），产物
                // 与旧实现逐字节一致；有损 golden 时误差不再向后续帧传导。
                let mut diff_rgb = vec![0i32; frame.pixels.len()];
                crate::crf::format::simd::sub_i32(&frame.pixels, &g_hat, &mut diff_rgb);
                if q95_soft {
                    soft1(&mut diff_rgb);
                }
                // 噪声感知软阈值预处理（仅差分帧、零中心性门控内生效）
                if noise_on {
                    use super::noise::{
                        estimate_interleaved_band_thresholds, soft_threshold_interleaved,
                    };
                    let thresholds = estimate_interleaved_band_thresholds(
                        &diff_rgb,
                        frame.width as usize,
                        frame.height as usize,
                        components,
                        tuning.noise_tau_x100,
                    );
                    soft_threshold_interleaved(
                        &mut diff_rgb,
                        frame.width as usize,
                        frame.height as usize,
                        components,
                        &thresholds,
                    );
                }
                let eff_pixels = crate::crf::format::rct_forward(&diff_rgb, components)?;
                let eff_frame = ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: eff_pixels,
                };
                let fq = fq_for_index(i);
                // 闭环 per-band 自适应步长（噪声归一化）：失真稠密的条带
                // 死区加宽，静止为主的条带保持基础步长精细度。
                let band_steps: Vec<u8> = if noise_on {
                    use super::noise::estimate_band_quant_steps;
                    estimate_band_quant_steps(
                        &eff_frame.pixels,
                        eff_frame.width as usize,
                        eff_frame.height as usize,
                        components,
                        fq.step,
                        tuning.noise_tau_x100,
                    )
                } else {
                    Vec::new()
                };
                let band_ref: super::frame::BandSteps<'_> =
                    if noise_on { Some(&band_steps) } else { None };
                let mut data = if params.adaptive_prediction {
                    encode_frame_adaptive(
                        &eff_frame,
                        compression_type,
                        header.block_size,
                        false, // 差分帧无首帧语义
                        fq,
                        None,
                        band_ref,
                    )?
                    .data
                } else {
                    encode_frame(
                        &eff_frame,
                        compression_type,
                        header.block_size,
                        header.prediction_mode,
                        false,
                        fq,
                        None,
                    )?
                };
                // golden 参考标志（coding_params.bit7）：全 golden 架构下所有
                // 差分帧均参考首帧
                if data.len() > FRAME_HEADER_SIZE {
                    data[9] |= 0x80;
                }
                Ok((data, None, true))
            })
            .collect::<Result<Vec<_>, CrfError>>()?;

        let mut all_results = Vec::with_capacity(frames.len());
        all_results.push(first_result);
        all_results.extend(rest_results);
        all_results
    } else {
        // ===== 路径 C：兼容（预差分序列 + 模式继承 + 关键帧间隔）=====
        let first_fq = FrameQuant::lossless();
        let (mut first_data, mut first_pm) = if params.adaptive_prediction {
            let out = encode_frame_adaptive(
                &encode_frames[0],
                compression_type,
                header.block_size,
                true,
                first_fq,
                None,
                None,
            )?;
            (out.data, out.pred_mode.map(|m| m as u8))
        } else {
            let data = encode_frame(
                &encode_frames[0],
                compression_type,
                header.block_size,
                header.prediction_mode,
                true,
                FrameQuant::lossless(),
                None,
            )?;
            (data, None)
        };
        // v1.13 RCT 首帧自适应：路径 C 首帧同样与 RGB 直通版本竞争
        //（frames[0] 为原始 RGB 原域；路径 C 首帧恒无损，无轻滤语义）。
        // 直通胜出时 pred_mode 继承源同步切换为直通版的帧级模式。
        if use_rct {
            let (bypass_data, bypass_pm) = if params.adaptive_prediction {
                let out = encode_frame_adaptive(
                    &frames[0],
                    compression_type,
                    header.block_size,
                    true,
                    first_fq,
                    None,
                    None,
                )?;
                (out.data, out.pred_mode.map(|m| m as u8))
            } else {
                let data = encode_frame(
                    &frames[0],
                    compression_type,
                    header.block_size,
                    header.prediction_mode,
                    true,
                    FrameQuant::lossless(),
                    None,
                )?;
                (data, None)
            };
            if bypass_data.len() < first_data.len() {
                first_data = bypass_data;
                first_pm = bypass_pm;
                header.flags.set_first_frame_no_rct(true);
            }
        }
        let preferred = first_pm.map(crate::crf::format::PredictionMode::from_u8);

        let mut rest: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
        if params.adaptive_prediction {
            let outs: Vec<CrfResult<super::adaptive::AdaptiveOutput>> = encode_frames
                .par_iter()
                .enumerate()
                .skip(1)
                .map(|(i, frame)| {
                    let fq = fq_for_chain_index(i, lossy_quant_step, &tuning, base_bias);
                    encode_frame_adaptive(
                        frame,
                        compression_type,
                        header.block_size,
                        false,
                        fq,
                        preferred,
                        None,
                    )
                })
                .collect();
            for out in outs {
                let o = out?;
                rest.push((o.data, o.pred_mode.map(|m| m as u8)));
            }
        } else {
            let outs: Vec<CrfResult<Vec<u8>>> = encode_frames
                .par_iter()
                .enumerate()
                .skip(1)
                .map(|(i, frame)| {
                    let fq = fq_for_chain_index(i, lossy_quant_step, &tuning, base_bias);
                    encode_frame(
                        frame,
                        compression_type,
                        header.block_size,
                        header.prediction_mode,
                        false,
                        fq,
                        None,
                    )
                })
                .collect();
            for out in outs {
                rest.push((out?, None));
            }
        }

        let mut all: Vec<(Vec<u8>, Option<u8>, bool)> = vec![(first_data, first_pm, false)];
        all.extend(rest.into_iter().map(|(d, pm)| (d, pm, false)));
        all
    };

    for (frame_data, _, golden) in all_results.into_iter() {
        let frame_size = frame_data.len() as u32;
        encoded_frames.push((frame_data, current_offset, frame_size, golden));
        current_offset += frame_size;
    }

    // 组装文件
    let total_size = frames_start
        + encoded_frames
            .iter()
            .map(|(_, _, size, _)| *size as usize)
            .sum::<usize>()
        + FOOTER_SIZE;
    let mut output = Vec::with_capacity(total_size);

    // 写入文件头
    header.write_bytes(&mut output)?;

    // 写入帧索引
    for (_, offset, size, _) in &encoded_frames {
        output.extend_from_slice(&offset.to_le_bytes());
        output.extend_from_slice(&size.to_le_bytes());
    }

    // 写入编码后的帧数据
    for (frame_data, _, _, _) in &encoded_frames {
        output.extend_from_slice(frame_data);
    }

    // 计算 CRC32（不含文件尾）
    let crc = crc32(&output);

    // 写入文件尾
    output.extend_from_slice(&crc.to_le_bytes());
    output.extend_from_slice(&FOOTER_MAGIC);

    Ok(output)
}

/// 路径 C（链式差分）的逐帧量化配置：
/// 关键帧间隔边界走无损刷新（阻断误差累积），其余帧用全局档位。
fn fq_for_chain_index(
    i: usize,
    lossy_quant_step: Option<u8>,
    tuning: &crate::crf::format::LossyTuning,
    base_bias: i8,
) -> FrameQuant {
    let gq = lossy_quant_step.unwrap_or(0);
    if gq == 0 || i.is_multiple_of(tuning.keyframe_interval.max(1) as usize) {
        return FrameQuant::lossless();
    }
    let _q95 = crate::crf::format::quant::is_q95_perceptual(quality_of_step(gq));
    FrameQuant {
        step: gq,
        bias: base_bias,
        chroma_step: tuning.chroma_step(gq),
        chroma_bias: tuning.chroma_deadzone_bias.unwrap_or(base_bias),
        chroma_half_res: gq > 1 && tuning.chroma_half_res,
        // v1.12：显式路径为兼容保留，q95 矩阵缩放仅支撑推荐路径 G
        q1_matrix_scale: false,
    }
}

/// 由步长反推质量档位（仅用于 q95 判定；Q=1 时无法区分 q95 与 q96+，
/// 链式路径按保守语义不启用矩阵缩放——恒返回 None 档）
fn quality_of_step(_step: u8) -> u8 {
    0
}
