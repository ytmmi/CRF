//! 分批加载的序列编码（超大组并行，路径 G 语义逐字节一致）
//!
//! 与 `sequence::encode_sequence_resolved` 路径 G（`input_original_frames`）语义
//! 一致：首帧闭环重建 `G_hat`，其余差分帧相对 `G_hat` 独立编码。区别仅在帧来源
//! ——本模块通过 `load(i)` 惰性加载，按 `batch_frames`（内存预算/单帧大小）
//! 分批并行，批完成后释放帧像素，内存 O(golden + batch_frames×单帧 + 码流)，
//! 支持 30-3-81（81 帧 3826×5412）等超大组。
//!
//! 仅支持原始帧序列（路径 G）；预差分序列（路径 C）请用批量接口。

use rayon::prelude::*;

use crate::crf::core::bitstream::constants::{
    FRAME_HEADER_SIZE, HEADER_SIZE, LIC_A_NUM_OFFSET, LIC_B_OFFSET,
};
use crate::crf::core::contract::ResolvedConfig;
use crate::crf::core::domain::ImageData;
use crate::crf::error::{CrfError, CrfResult};
use crate::crf::performance::telemetry::Span;

use super::frame::candidate::encode_frame_adaptive;
use super::frame::{encode_frame, FrameQuant};
use super::session::batch::{assemble_crf_output, fq_for_index};

/// 分批加载并编码原始帧序列（路径 G）。
///
/// `load(i)` 惰性加载第 i 帧（i ∈ [0, frame_count)）；同一帧最多被请求一次。
pub fn encode_sequence_batched<F>(
    frame_count: usize,
    mut load: F,
    resolved: &ResolvedConfig,
) -> CrfResult<Vec<u8>>
where
    F: FnMut(usize) -> CrfResult<ImageData>,
{
    if frame_count < 2 {
        return Err(CrfError::FrameCountOutOfRange(frame_count as u16));
    }
    let params = &resolved.params;
    let mut header = resolved.header_template.clone();
    // 惰性加载路径的帧数由调用方提供（resolve 时可能只见过首帧）
    header.frame_count = frame_count as u16;
    let tuning = resolved.kernel.clone();
    let use_rct = resolved.use_rct;
    let compression_type = header.compression_type;
    let lossy_quant_step = tuning.enabled.then_some(tuning.global_step);

    let first = load(0)?;
    let components = first.color_format.component_count();
    let per_frame_bytes = first.width as usize * first.height as usize * components * 4;
    let batch_mem_limit: usize = std::env::var("CRF_BATCH_MEM_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000_000_000);
    // 帧级并行度：内存预算/单帧，并受核数上限约束（帧级并行比帧内并行更有效，
    // 实测 batch_frames 越大越快；超过核数无益）。
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    // 批内并行度：默认按内存预算自动切分；CRF_BATCH_FRAMES 可显式覆盖（调优/诊断）。
    let batch_frames = std::env::var("CRF_BATCH_FRAMES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(|| (batch_mem_limit / per_frame_bytes.max(1)).clamp(1, frame_count))
        .min(frame_count)
        .min(cores);

    let interval = tuning.anchor_interval.max(1) as usize;
    let base_bias = tuning.deadzone_bias;
    let q95 = tuning.q95_perceptual;
    let fq_for = |i: usize| -> FrameQuant {
        fq_for_index(i, lossy_quant_step, &tuning, base_bias, interval, q95)
    };

    let noise_on = lossy_quant_step.is_some() && tuning.noise_adaptive && components == 3;
    let q95_soft = q95 && lossy_quant_step.is_some();
    let soft1 = |buf: &mut [i32]| {
        for v in buf.iter_mut() {
            if *v == 1 || *v == -1 {
                *v = 0;
            }
        }
    };

    // ===== 阶段 1：首帧编码 + 闭环重建 G_hat =====
    let first_span = Span::begin("encode.first_frame");
    let mut first_diff_rgb = first.pixels.clone();
    if q95_soft {
        soft1(&mut first_diff_rgb);
    }
    let first_bypass_rgb = if use_rct {
        Some(first_diff_rgb.clone())
    } else {
        None
    };
    crate::crf::core::color::rct::rct_forward_in_place(&mut first_diff_rgb, components)?;
    let first_eff_frame = ImageData {
        width: first.width,
        height: first.height,
        bit_depth: first.bit_depth,
        color_format: first.color_format,
        pixels: first_diff_rgb,
    };
    let fq_first = fq_for(0);
    let mut data_first = if params.adaptive_prediction {
        encode_frame_adaptive(
            &first_eff_frame,
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
    let mut g_hat_img = crate::crf::decoder::reconstruct::reconstruct_frame(&data_first, &header)?;
    if header.flags.has_rct() && !header.flags.first_frame_no_rct() {
        g_hat_img.pixels = crate::crf::core::color::rct::rct_inverse(&g_hat_img.pixels, components)?;
    }
    let g_hat = g_hat_img.pixels; // RGB 域重建首帧
    drop(first_span);

    // ===== 阶段 2：差分帧分批加载 + 并行编码（相对 G_hat）=====
    let rest_span = Span::begin("encode.rest_frames");
    let mut rest_results: Vec<(Vec<u8>, Option<u8>, bool)> = Vec::with_capacity(frame_count - 1);
    for start in (1..frame_count).step_by(batch_frames) {
        let end = (start + batch_frames).min(frame_count);
        let batch: Vec<ImageData> = (start..end).map(|i| load(i)).collect::<CrfResult<_>>()?;        let batch_results: Vec<(Vec<u8>, Option<u8>, bool)> = batch
            .par_iter()
            .enumerate()
            .map(|(j, frame)| -> CrfResult<(Vec<u8>, Option<u8>, bool)> {
                let i = start + j;
                if frame.width != first.width || frame.height != first.height {
                    return Err(CrfError::ImageDimensionsMismatch {
                        expected: (first.width, first.height),
                        actual: (frame.width, frame.height),
                    });
                }
                let fq = fq_for(i);
                let lic_on = super::sequence_tools::lic_globally_enabled();
                let fused = !q95_soft && !noise_on;
                let encode_diff = |diff_rgb: Vec<i32>, already_rct: bool| -> CrfResult<Vec<u8>> {
                    let mut diff_rgb = diff_rgb;
                    if !already_rct {
                    if q95_soft {
                        soft1(&mut diff_rgb);
                    }
                    if noise_on {
                        use crate::crf::core::perceptual::noise::{
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
                    crate::crf::core::color::rct::rct_forward_in_place(&mut diff_rgb, components)?;
                    }
                    let eff_frame = ImageData {
                        width: frame.width,
                        height: frame.height,
                        bit_depth: frame.bit_depth,
                        color_format: frame.color_format,
                        pixels: diff_rgb,
                    };
                    let activity_on = tuning.activity_masking_x100 != 100
                        || tuning.flat_area_protection_x100 != 100
                        || tuning.edge_protection_x100 != 100;
                    let band_steps: Vec<u8> = if noise_on || activity_on {
                        if activity_on {
                            use crate::crf::core::perceptual::noise::estimate_band_activity_steps;
                            estimate_band_activity_steps(
                                &eff_frame.pixels,
                                eff_frame.width as usize,
                                eff_frame.height as usize,
                                components,
                                fq.step,
                                tuning.activity_masking_x100,
                                tuning.flat_area_protection_x100,
                                tuning.edge_protection_x100,
                            )
                        } else {
                            use crate::crf::core::perceptual::noise::estimate_band_quant_steps;
                            estimate_band_quant_steps(
                                &eff_frame.pixels,
                                eff_frame.width as usize,
                                eff_frame.height as usize,
                                components,
                                fq.step,
                                tuning.noise_tau_x100,
                            )
                        }
                    } else {
                        Vec::new()
                    };
                    let band_ref: super::frame::BandSteps<'_> =
                        if noise_on || activity_on { Some(&band_steps) } else { None };
                    if params.adaptive_prediction {
                        Ok(encode_frame_adaptive(
                            &eff_frame,
                            compression_type,
                            header.block_size,
                            false,
                            fq,
                            None,
                            band_ref,
                        )?
                        .data)
                    } else {
                        Ok(encode_frame(
                            &eff_frame,
                            compression_type,
                            header.block_size,
                            header.prediction_mode,
                            false,
                            fq,
                            None,
                        )?)
                    }
                };

                // golden 差分候选
                let mut diff_golden = vec![0i32; frame.pixels.len()];
                if fused {
                    crate::crf::backend::ops::sub_rct_forward(
                        &frame.pixels,
                        &g_hat,
                        &mut diff_golden,
                    );
                } else {
                    crate::crf::backend::ops::sub_i32(&frame.pixels, &g_hat, &mut diff_golden);
                }
                let mut data = encode_diff(diff_golden, fused)?;
                // LIC 加权 golden 候选（单调不劣化）
                let mut lic_field: Option<(u8, u8)> = None;
                if lic_on {
                    if let Some(fit) =
                        crate::crf::core::illumination::search_lic(&g_hat, &frame.pixels)
                    {
                        if fit.worthwhile() {
                            let mut lic_ref = vec![0i32; g_hat.len()];
                            crate::crf::core::illumination::fit_into(
                                &g_hat,
                                fit.a_num,
                                fit.b,
                                &mut lic_ref,
                            );
                            let mut diff_lic = vec![0i32; frame.pixels.len()];
                            if fused {
                                crate::crf::backend::ops::sub_rct_forward(
                                    &frame.pixels,
                                    &lic_ref,
                                    &mut diff_lic,
                                );
                            } else {
                                crate::crf::backend::ops::sub_i32(
                                    &frame.pixels,
                                    &lic_ref,
                                    &mut diff_lic,
                                );
                            }
                            let data_lic = encode_diff(diff_lic, fused)?;
                            if data_lic.len() < data.len() {
                                data = data_lic;
                                lic_field = Some((fit.a_num as u8, fit.b as i8 as u8));
                            }
                        }
                    }
                }
                if let Some((a_num, b)) = lic_field {
                    if data.len() > FRAME_HEADER_SIZE {
                        data[LIC_A_NUM_OFFSET] = a_num;
                        data[LIC_B_OFFSET] = b;
                    }
                }
                Ok((data, None, true))
            })
            .collect::<CrfResult<Vec<_>>>()?;
        rest_results.extend(batch_results);
        eprintln!(
            "  差分批 {}/{}（帧 {}..{}）完成",
            (start - 1) / batch_frames + 1,
            (frame_count - 1).div_ceil(batch_frames),
            start,
            end
        );
    }
    drop(rest_span);

    // ===== 阶段 3：组装 =====
    let mut all_results: Vec<(Vec<u8>, Option<u8>, bool)> = Vec::with_capacity(frame_count);
    all_results.push((data_first, None, false));
    all_results.extend(rest_results);

    let frames_start = HEADER_SIZE + frame_count * 8;
    let mut current_offset = frames_start as u32;
    let mut encoded_frames = Vec::with_capacity(frame_count);
    for (frame_data, _, golden) in all_results {
        let frame_size = frame_data.len() as u32;
        encoded_frames.push((frame_data, current_offset, frame_size, golden));
        current_offset += frame_size;
    }
    let assemble_span = Span::begin("encode.assemble");
    let output = assemble_crf_output(&header, frames_start, &encoded_frames)?;
    drop(assemble_span);
    Ok(output)
}
