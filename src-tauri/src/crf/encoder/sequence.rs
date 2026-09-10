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

use crate::crf::core::bitstream::constants::{
    FRAME_HEADER_SIZE, HEADER_SIZE, LIC_A_NUM_OFFSET, LIC_B_OFFSET, REFERENCE_TYPE_OFFSET,
};
use crate::crf::core::domain::{EncodeParams, ImageData};
use crate::crf::error::{CrfError, CrfResult};

use super::frame::candidate::encode_frame_adaptive;
use super::frame::{encode_frame, FrameQuant};

use crate::crf::performance::telemetry::Span;

/// 编码完整的 CRF 文件（公共入口）
///
/// 内部先经 [`crate::crf::core::contract::ResolvedConfig::resolve`] 解析配置一次，
/// 再委托 [`encode_sequence_resolved`] 消费，避免 compression_type/header/RCT/量化
/// 的重复解析（P3.b 配置解析收敛）。
/// 有界并发信号量：限制**同时编码的帧数**，控制并行中间量驻留内存
/// （非分批——帧完成即释放许可，保持连续流水，无批间空闲）。
struct FrameSemaphore {
    available: std::sync::Mutex<usize>,
    cv: std::sync::Condvar,
}

impl FrameSemaphore {
    fn new(n: usize) -> Self {
        Self {
            available: std::sync::Mutex::new(n.max(1)),
            cv: std::sync::Condvar::new(),
        }
    }
    /// 获取一个许可（RAII：`FramePermit` drop 时自动释放）。
    fn acquire(&self) -> FramePermit<'_> {
        let mut a = self.available.lock().unwrap_or_else(|e| e.into_inner());
        while *a == 0 {
            a = self.cv.wait(a).unwrap_or_else(|e| e.into_inner());
        }
        *a -= 1;
        FramePermit { sem: self }
    }
    fn release(&self) {
        let mut a = self.available.lock().unwrap_or_else(|e| e.into_inner());
        *a += 1;
        self.cv.notify_one();
    }
}

struct FramePermit<'a> {
    sem: &'a FrameSemaphore,
}
impl Drop for FramePermit<'_> {
    fn drop(&mut self) {
        self.sem.release();
    }
}

pub fn encode_sequence(frames: &[ImageData], params: &EncodeParams) -> CrfResult<Vec<u8>> {
    let resolved = crate::crf::core::contract::ResolvedConfig::resolve(params, frames)?;
    encode_sequence_resolved(frames, &resolved)
}

/// 消费已解析配置的序列编码主流程（P3.b）
///
/// 输入校验与编码编排保留在此；配置解析（压缩类型映射、文件头构建、RCT 判定、
/// 有损内核配置）由 [`crate::crf::core::contract::ResolvedConfig`] 一次性提供，
/// 批量和 streaming 不得分别解析。
pub(crate) fn encode_sequence_resolved(
    frames: &[ImageData],
    resolved: &crate::crf::core::contract::ResolvedConfig,
) -> CrfResult<Vec<u8>> {
    // 验证输入
    if frames.is_empty() {
        return Err(CrfError::FrameCountOutOfRange(0));
    }

    let frame_count = frames.len() as u16;
    if frame_count < 2 {
        return Err(CrfError::FrameCountOutOfRange(frame_count));
    }
    // 帧数上限由 bitstream u16 决定（65535）；批内并行度由内存预算**自动切分**
    // （见下方 batch_frames），不再硬限 50 帧。

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

    // 消费已解析配置：压缩类型、文件头、RCT、有损内核均来自 ResolvedConfig
    let params = &resolved.params;
    let compression_type = resolved.header_template.compression_type;
    let mut header = resolved.header_template.clone();
    let tuning = resolved.kernel.clone();
    let use_rct = resolved.use_rct;
    let lossy_quant_step = tuning.enabled.then_some(tuning.global_step);

    // INF.1 内存预检：batch 接口全帧驻留（RCT 后全帧 + G_hat + 编码产物 +
    // planar 中间量），大图组会触发 allocator fail-fast panic。返回结构化错误
    // 引导用户用 streaming 路径（CRF_STREAMING=1，内存 O(golden+单帧+码流)）。
    // §13 发现组10（8500×5816×4帧 ~2.4GB）batch panic；1000 组（~310MB）正常。
    let components = first.color_format.component_count();
    let per_frame_bytes = first.width as usize * first.height as usize * components * 4;
    // batch 内存阈值：默认 1.5 GB 保守值，CRF_BATCH_MEM_LIMIT 环境变量可放宽。
    // 根据内存预算与单帧大小**自动切分**批内并行度（每批驻留像素 ≤ 预算），
    // 不再硬限 50 帧；输入帧仍由调用方驻留（超大组请用 streaming 路径）。
    let batch_mem_limit: usize = std::env::var("CRF_BATCH_MEM_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_500_000_000);
    let batch_frames = (batch_mem_limit / per_frame_bytes.max(1)).clamp(1, frames.len());

    let interval = tuning.anchor_interval.max(1) as usize;
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
    let q95 = tuning.q95_perceptual;
    // 使用 session::batch::fq_for_index（P3 架构迁移）
    let fq_for_index = |i: usize| -> FrameQuant {
        super::session::batch::fq_for_index(i, lossy_quant_step, &tuning, base_bias, interval, q95)
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
        let first_span = Span::begin("encode.first_frame");
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
        // P1：原地 RCT——bypass 副本已在上一行克隆，first_diff_rgb 可直接
        // 原地改写为 RCT 域，省去 rct_forward 内部 to_vec 全帧克隆，逐位一致。
        crate::crf::core::color::rct::rct_forward_in_place(&mut first_diff_rgb, components)?;
        let first_eff_frame = ImageData {
            width: frames[0].width,
            height: frames[0].height,
            bit_depth: frames[0].bit_depth,
            color_format: frames[0].color_format,
            pixels: first_diff_rgb,
        };

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
        // 复用公共重建层（decoder/reconstruct，规划文档 §5.4）同一入口与
        // 最终文件头上下文（flags 含 has_rct / first_frame_no_rct /
        // lossy_quant），保证编码端本地重建与文件自包含解码逐位一致——
        // 这是闭环语义的定义本身。不直接调用 decoder 容器/session 层。
        let mut g_hat_img =
            crate::crf::decoder::reconstruct::reconstruct_frame(&data_first, &header)?;
        if header.flags.has_rct() && !header.flags.first_frame_no_rct() {
            g_hat_img.pixels =
                crate::crf::core::color::rct::rct_inverse(&g_hat_img.pixels, components)?;
        }
        let g_hat = g_hat_img.pixels; // RGB 域重建首帧
        drop(first_span);

        // ===== 阶段 2：后续帧并行差分编码（残差 = 原始帧 − G_hat）=====
        // v1.13 RCT 首帧自适应的双路竞争仅属于首帧；差分帧逻辑保持原样。
        let rest_span = Span::begin("encode.rest_frames");
        // 有界并发：信号量限制**同时编码的帧数** ≤ batch_frames（按内存预算自动
        // 切分），控制并行中间量驻留内存；非分批——帧完成即释放许可（连续流水，
        // 无批间空闲）。结果按帧序 collect，产物与全并行逐字节一致。
        let frame_sem = FrameSemaphore::new(batch_frames);
        let rest_results: Vec<(Vec<u8>, Option<u8>, bool)> = frames[1..]
            .par_iter()
            .enumerate()
            .map(|(j, frame)| -> CrfResult<(Vec<u8>, Option<u8>, bool)> {
                let i = j + 1;
                let _permit = frame_sem.acquire();
                // P0 闭环核心：差分基准为本地重建的 G_hat（而非 frames[0]）。
                // 无损 golden 时 G_hat == frames[0]（decode 精确还原），产物
                // 与旧实现逐字节一致；有损 golden 时误差不再向后续帧传导。
                let fq = fq_for_index(i);
                // v1.16 LIC 全局开关（A/B 验证与逃生门，batch/streaming 共用）
                let lic_on = super::sequence_tools::lic_globally_enabled();
                // 差分帧编码管线（golden / LIC 共用）：入参 RGB 域差分
                // （未后处理），内部执行 q95 轻滤 → 噪声软阈值 → RCT →
                // band 步长 → 熵编码。golden 与 LIC 两路逐参对齐，保证
                // batch/streaming 对称与字节竞争公平。
                let encode_diff = |diff_rgb: Vec<i32>| -> CrfResult<Vec<u8>> {
                    let mut diff_rgb = diff_rgb;
                    if q95_soft {
                        soft1(&mut diff_rgb);
                    }
                    // 噪声感知软阈值预处理（仅差分帧、零中心性门控内生效）
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
                    // P1：原地 RCT——diff_rgb 已是独占缓冲，直接改写省去 rct_forward
                    // 内部的 to_vec 全帧克隆，逐位一致。
                    crate::crf::core::color::rct::rct_forward_in_place(
                        &mut diff_rgb,
                        components,
                    )?;
                    let eff_frame = ImageData {
                        width: frame.width,
                        height: frame.height,
                        bit_depth: frame.bit_depth,
                        color_format: frame.color_format,
                        pixels: diff_rgb,
                    };
                    // 闭环 per-band 自适应步长：噪声归一化（amp25 失真感知）或
                    // activity masking（空间梯度感知）二选一，由 V2 perceptual 字段决定。
                    let activity_on = tuning.activity_masking_x100 != 100
                        || tuning.flat_area_protection_x100 != 100
                        || tuning.edge_protection_x100 != 100;
                    let band_steps: Vec<u8> = if noise_on || activity_on {
                        if activity_on {
                            // P4.2/P4.3/P4.4 activity masking：纹理增步长 + 平坦/边缘减步长
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
                    let band_ref: super::frame::BandSteps<'_> = if noise_on || activity_on {
                        Some(&band_steps)
                    } else {
                        None
                    };
                    if params.adaptive_prediction {
                        Ok(encode_frame_adaptive(
                            &eff_frame,
                            compression_type,
                            header.block_size,
                            false, // 差分帧无首帧语义
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

                // golden 差分候选（帧头 reference_type=0）
                let mut diff_golden = vec![0i32; frame.pixels.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, &g_hat, &mut diff_golden);
                let data_golden = encode_diff(diff_golden)?;

                // LIC 加权 golden 差分候选（v1.16）：采样扫描 → 预筛 →
                // 同管线编码 → 字节竞争（单调不劣化，仅在更小时采用）。
                // 光照渐变（闪光/阴影/时间渐变）内容 LIC 残差显著更小；
                // 无收益帧预筛直接跳过，零额外编码成本。
                let mut data = data_golden;
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
                            crate::crf::backend::ops::sub_i32(
                                &frame.pixels,
                                &lic_ref,
                                &mut diff_lic,
                            );
                            let data_lic = encode_diff(diff_lic)?;
                            if data_lic.len() < data.len() {
                                data = data_lic;
                                lic_field = Some((fit.a_num as u8, fit.b as i8 as u8));
                            }
                        }
                    }
                }
                // 写入帧头 LIC 信令（仅启用时；默认 (0,0) 由 assemble_frame 写出）
                if let Some((a_num, b)) = lic_field {
                    if data.len() > FRAME_HEADER_SIZE {
                        data[LIC_A_NUM_OFFSET] = a_num;
                        data[LIC_B_OFFSET] = b;
                    }
                }
                // v1.15：golden 参考由帧头 reference_type=0 表达（assemble_frame
                // 默认值），不再写 coding_params.bit7——差分帧恒参考首帧。
                Ok((data, None, true))
            })
            .collect::<Result<Vec<_>, CrfError>>()?;
        drop(rest_span);

        let mut all_results = Vec::with_capacity(frames.len());
        all_results.push(first_result);
        all_results.extend(rest_results);
        all_results
    } else {
        // ===== 路径 C：兼容（预差分序列 + 模式继承 + 关键帧间隔）=====
        // 全帧 RCT 前置副本仅在路径 C 需要（首帧 + 预差分残差帧均以 RCT 域编码）。
        let rct_span = Span::begin("encode.rct");
        let encode_frames: Vec<ImageData> = if use_rct {
            frames
                .iter()
                .map(|f| {
                    let transformed =
                        crate::crf::core::color::rct::rct_forward(&f.pixels, components)?;
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
        drop(rct_span);
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
        let preferred = first_pm.map(crate::crf::core::domain::PredictionMode::from_u8);

        let mut rest: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
        if params.adaptive_prediction {
            let outs: Vec<CrfResult<super::frame::candidate::AdaptiveOutput>> = encode_frames
                .par_iter()
                .enumerate()
                .skip(1)
                .map(|(i, frame)| {
                    let fq = super::session::batch::fq_for_chain_index(
                        i,
                        lossy_quant_step,
                        &tuning,
                        base_bias,
                    );
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
                    let fq = super::session::batch::fq_for_chain_index(
                        i,
                        lossy_quant_step,
                        &tuning,
                        base_bias,
                    );
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
        // 路径 C 为预差分序列：帧 i 存储的即差分数据，解码端以链式（previous）
        // 还原 F[i] = F[i-1] + 帧 i。v1.15：帧头 reference_type 显式标记为 1。
        all.extend(rest.into_iter().map(|(mut d, pm)| {
            if d.len() > FRAME_HEADER_SIZE {
                d[REFERENCE_TYPE_OFFSET] = 1; // previous 链式参考
            }
            (d, pm, false)
        }));
        all
    };

    // P5.1/P5.2/P5.5/P5.7: 可选的重建参考竞争（golden / previous / prev2，v1.15）。
    // 默认保持 Golden 旧语义；显式选择 Previous/Hybrid 时逐帧比较码字长度，
    // 并仅使用已重建帧作为参考。Hybrid 在场景切换时回到 golden（新 anchor
    // 的码流语义与 golden 相同）。
    //
    // v1.15 多参考帧（prev2）：Hybrid 模式在 previous 之外增加前前帧（prev2）
    // 候选，应对周期动作/遮挡重现（帧 N 与 N−2 更相似）。信令为帧头
    // reference_type：0=golden / 1=previous / 2=prev2。Previous 模式保持纯
    // previous 链（不引入 prev2，避免改变其误差累积设计语义）。
    let all_results = if params.input_original_frames
        && !matches!(
            tuning.reference_mode,
            crate::crf::core::config::lossy_v2::ReferenceModeV2::Golden
        )
        && !all_results.is_empty()
    {
        let mut out = all_results;
        let first_recon = crate::crf::decoder::reconstruct::reconstruct_frame(&out[0].0, &header)?;
        let first_in_rct = use_rct && !header.flags.first_frame_no_rct();
        // golden_rgb：首帧重建（RGB 域），previous/prev2 链的初始基准
        let golden_rgb = if first_in_rct {
            crate::crf::core::color::rct::rct_inverse(&first_recon.pixels, components)?
        } else {
            first_recon.pixels
        };
        // previous = recon[i-1]（RGB 域）；prev2 = recon[i-2]（RGB 域，可选）
        let mut previous = golden_rgb.clone();
        let mut prev2: Option<Vec<i32>> = None;
        let force_previous = matches!(
            tuning.reference_mode,
            crate::crf::core::config::lossy_v2::ReferenceModeV2::Previous
        );
        for i in 1..out.len() {
            let frame = &frames[i];
            let fq = fq_for_index(i);
            // 场景切换检测基于 previous 差分（与旧逻辑一致）：
            // 残差均值超过阈值时视为新 anchor，跳过 prev/prev2 候选，
            // 使用 golden 参考保持随机访问与误差隔离。
            let mut diff_prev = vec![0i32; frame.pixels.len()];
            crate::crf::backend::ops::sub_i32(&frame.pixels, &previous, &mut diff_prev);
            let periodic_anchor =
                tuning.anchor_interval > 0 && i.is_multiple_of(tuning.anchor_interval as usize);
            let scene_cut = periodic_anchor
                || (!matches!(
                    tuning.scene_cut,
                    crate::crf::core::config::lossy_v2::SceneCutModeV2::Off
                ) && (diff_prev
                    .iter()
                    .map(|v| v.unsigned_abs() as u64)
                    .sum::<u64>()
                    / diff_prev.len().max(1) as u64)
                    > ((tuning.scene_cut_threshold_x1000 as u64 * 255) / 1000));

            // 参考候选编码（闭包）：diff vs 参考 → 稀疏变化 mask → RCT → 编码。
            // 与路径 G 阶段 2 的差分帧处理逐参对齐（change_mask / band 语义）。
            let encode_ref = |ref_pixels: &[i32]| -> crate::crf::error::CrfResult<Vec<u8>> {
                let mut diff = vec![0i32; frame.pixels.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, ref_pixels, &mut diff);
                if !matches!(
                    tuning.change_mask,
                    crate::crf::core::config::lossy_v2::ToolMode::Off
                ) {
                    let ts = header.block_size.max(4) as usize;
                    let mask = super::sequence_tools::change_mask(
                        &frame.pixels,
                        ref_pixels,
                        frame.width as usize,
                        frame.height as usize,
                        components,
                        ts,
                        if fq.step > 0 { (fq.step / 2) as i32 } else { 0 },
                    );
                    super::sequence_tools::apply_change_mask(
                        &mut diff,
                        &mask,
                        frame.width as usize,
                        frame.height as usize,
                        components,
                        ts,
                    );
                }
                let eff = crate::crf::core::color::rct::rct_forward(&diff, components)?;
                let eff_frame = ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: eff,
                };
                if params.adaptive_prediction {
                    Ok(encode_frame_adaptive(
                        &eff_frame,
                        compression_type,
                        header.block_size,
                        false,
                        fq,
                        None,
                        None,
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

            // 三候选竞争：golden（out[i] 已生成，reference_type=0）为初始最优。
            let mut best_len = out[i].0.len();
            let mut adopted: Option<(Vec<u8>, u8)> = None; // (data, reference_type)
            if !scene_cut {
                // previous 候选（reference_type=1）
                let cand_prev = encode_ref(&previous)?;
                if !cand_prev.is_empty() && (force_previous || cand_prev.len() < best_len) {
                    best_len = cand_prev.len();
                    adopted = Some((cand_prev, 1));
                }
                // prev2 候选（reference_type=2，仅 Hybrid——Previous 模式
                // 保持纯 previous 链的误差累积语义）
                if !force_previous {
                    if let Some(p2) = &prev2 {
                        let cand_p2 = encode_ref(p2)?;
                        if !cand_p2.is_empty() && cand_p2.len() < best_len {
                            best_len = cand_p2.len();
                            adopted = Some((cand_p2, 2));
                        }
                    }
                }
            }
            if let Some((data, ref_type)) = adopted {
                let mut d = data;
                if d.len() > FRAME_HEADER_SIZE {
                    d[REFERENCE_TYPE_OFFSET] = ref_type; // reference_type 字段
                }
                out[i] = (d, None, false);
            }

            // 更新重建链：以实际胜出帧重建为新的 previous；prev2 = 旧 previous。
            // 还原公式与解码端 restore_temporal 一致（参考基准 + 差分重建）。
            let recon = crate::crf::decoder::reconstruct::reconstruct_frame(&out[i].0, &header)?;
            let mut rgb = recon.pixels;
            if use_rct {
                rgb = crate::crf::core::color::rct::rct_inverse(&rgb, components)?;
            }
            let ref_type = if out[i].0.len() > FRAME_HEADER_SIZE {
                out[i].0[REFERENCE_TYPE_OFFSET]
            } else {
                0
            };
            let base: Option<Vec<i32>> = if ref_type == 2 {
                // prev2 参考：recon[i-2] + rgb
                prev2
                    .as_ref()
                    .map(|p2| p2.iter().zip(&rgb).map(|(a, b)| a + b).collect())
            } else if ref_type == 1 {
                // previous 参考：recon[i-1]（旧 previous）+ rgb
                Some(previous.iter().zip(&rgb).map(|(a, b)| a + b).collect())
            } else {
                // golden 参考：首帧重建 + rgb。v1.16：若本帧启用 LIC 加权
                // （帧头 lic_a_num != 0），参考先经乘加变换
                // LIC(golden_rgb) = (lic_a_num·golden)/100 + lic_b，
                // 与解码端 restore_referenced 严格对称。
                let lic_a = if out[i].0.len() > FRAME_HEADER_SIZE {
                    out[i].0[LIC_A_NUM_OFFSET]
                } else {
                    0
                };
                let lic_b = if out[i].0.len() > FRAME_HEADER_SIZE {
                    out[i].0[LIC_B_OFFSET]
                } else {
                    0
                };
                let golden_ref: Vec<i32> = if lic_a != 0 {
                    crate::crf::core::illumination::apply_lic_weighted(
                        &golden_rgb,
                        lic_a,
                        lic_b,
                    )
                } else {
                    golden_rgb.clone()
                };
                Some(golden_ref.iter().zip(&rgb).map(|(a, b)| a + b).collect())
            };
            prev2 = Some(previous);
            previous = base.unwrap_or_else(|| rgb.clone());
        }
        out
    } else {
        all_results
    };

    for (frame_data, _, golden) in all_results.into_iter() {
        let frame_size = frame_data.len() as u32;
        encoded_frames.push((frame_data, current_offset, frame_size, golden));
        current_offset += frame_size;
    }

    // 组装文件（使用 session::batch::assemble_crf_output，P3 架构迁移）
    let assemble_span = Span::begin("encode.assemble");
    let output =
        super::session::batch::assemble_crf_output(&header, frames_start, &encoded_frames)?;
    drop(assemble_span);

    // P5.6 码率护栏：目标由配置层以定点整数表达，编码结果不得静默突破硬上限。
    if tuning.enabled {
        let rate = &tuning.rate;
        let size = output.len() as u64;
        if let Some(max) = rate.max_bytes {
            if size > max {
                return Err(CrfError::InvalidCodingParams(format!(
                    "sequence exceeds max_bytes ({} > {})",
                    size, max
                )));
            }
        }
        if let Some(target) = rate.target_bytes {
            // 目标字节是软目标：报告/调用方可据此进行二次调参；仅在明显超出
            // （>125%）时返回错误，避免对旧调用造成意外失败。
            if size > target.saturating_mul(5) / 4 {
                return Err(CrfError::InvalidCodingParams(format!(
                    "sequence target_bytes infeasible ({} > {})",
                    size, target
                )));
            }
        }
    }

    Ok(output)
}
