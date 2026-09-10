//! 流式编码 API：支持 >50 帧与超大分辨率序列
//!
//! [`encode_sequence`](super::sequence::encode_sequence) 要求全部帧驻留
//! 内存且上限 50 帧；本模块提供**逐帧推送**的流式接口：
//!
//! - **内存 O(golden + 单帧 + 码流)**：每帧独立参考 golden 首帧编码，
//!   处理完立即释放差分/变换中间量——总内存不随帧数线性增长；
//! - **解除 50 帧上限**：frame_count 为 u16，流式模式支持至多
//!   65535 帧（索引项 8B/帧，65535 帧索引 ≈ 512KB）；
//! - **格式完全兼容**：输出文件结构与 encode_sequence 一致
//!   （header + index + frames + CRC32 footer），解码端无感。
//!
//! 正确性保证：复用与 encode_sequence 路径 G 完全相同的单帧编码管线
//! （golden 差分 → 噪声感知两级滤波 → RCT → 自适应竞争），同参数下
//! 每帧码流与一次性编码逐字节一致。

use crate::crf::checksum::crc32;
use crate::crf::core::bitstream::constants::{
    FOOTER_MAGIC, FOOTER_SIZE, FRAME_HEADER_SIZE, HEADER_SIZE, LIC_A_NUM_OFFSET, LIC_B_OFFSET,
    REFERENCE_TYPE_OFFSET,
};
use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::{CompressionType, EncodeParams, ImageData};
use crate::crf::error::{CrfError, CrfResult};

use super::frame::candidate::encode_frame_adaptive;
use super::FrameQuant;
use crate::crf::core::perceptual::noise::estimate_band_quant_steps;

/// 流式帧数上限（u16 索引容量）
pub const STREAMING_MAX_FRAMES: usize = 65535;

/// 流式编码器
///
/// 用法：
/// ```ignore
/// let mut enc = StreamingEncoder::new(&params)?;
/// enc.push_frame(&first)?;   // 首帧 = golden 基准
/// for f in rest { enc.push_frame(&f)?; }
/// let file_bytes = enc.finish()?;
/// ```
pub struct StreamingEncoder {
    params: EncodeParams,
    compression_type: CompressionType,
    header: CrfHeader,
    /// P3.b：有损内核配置；首帧 push 时用真实 components 解析（new() 时为 None）
    tuning: Option<crate::crf::core::config::lossy_v2::KernelLossyConfig>,
    lossy_quant_step: Option<u8>,
    interval: usize,

    /// golden 首帧（**RGB 域**）——常驻差分基准，有损模式下逐位精确
    golden_rgb: Option<ImageData>,
    /// P5.1 previous 参考帧（**RGB 域**）——链式参考使用，用于 previous/hybrid 模式
    previous_rgb: Option<ImageData>,
    /// v1.15 prev2 参考帧（**RGB 域**）——前前帧重建，用于 hybrid 模式（周期动作）
    prev2_rgb: Option<ImageData>,
    /// 已编码帧体（各帧 [flags][(tree)][stream] 拼接）
    body: Vec<u8>,
    /// 每帧在 body 内的相对偏移与大小（finish 时换算为绝对偏移）
    frame_layout: Vec<(usize, usize)>,
    /// 每帧的 golden 标志（reference_type==0；v1.15 前为 coding_params.bit7）
    frame_golden_flags: Vec<bool>,
    /// v1.15 每帧的 prev2 标志（reference_type==2）
    frame_prev2_flags: Vec<bool>,
    frames_written: usize,
    finished: bool,
}

impl StreamingEncoder {
    /// 创建流式编码器
    pub fn new(params: &EncodeParams) -> CrfResult<Self> {
        if params.compression_type.is_empty() {
            return Err(CrfError::InvalidCodingParams(
                params.compression_type.clone(),
            ));
        }
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

        // 占位头（finish 时以真实尺寸/帧数重建）
        let mut header = CrfHeader::new(0, 0, 0, 8, params_color(params), compression_type);
        header.block_size = params.block_size.unwrap_or(8) as u16;
        header.prediction_mode = params.prediction_mode;

        // P3.b：有损内核配置延迟到首帧 push 时解析（streaming 在 new() 时
        // 尚无帧信息，components/frame_count 未知，无法正确解析 chroma 采样）。
        Ok(StreamingEncoder {
            params: params.clone(),
            compression_type,
            header,
            tuning: None,
            lossy_quant_step: None,
            interval: 0,
            golden_rgb: None,
            previous_rgb: None,
            prev2_rgb: None,
            body: Vec::new(),
            frame_layout: Vec::new(),
            frame_golden_flags: Vec::new(),
            frame_prev2_flags: Vec::new(),
            frames_written: 0,
            finished: false,
        })
    }

    /// 推送一帧（首帧自动成为 golden 无损基准；后续帧为差分帧）
    ///
    /// 内存峰值 ≈ 单帧像素 + 编码缓冲——差分/变换中间量在返回前释放，
    /// 总内存不随已推送帧数增长。
    pub fn push_frame(&mut self, frame: &ImageData) -> CrfResult<()> {
        if self.finished {
            return Err(CrfError::InvalidCodingParams(
                "encoder already finished".into(),
            ));
        }
        if self.frames_written >= STREAMING_MAX_FRAMES {
            return Err(CrfError::FrameCountOutOfRange(STREAMING_MAX_FRAMES as u16));
        }
        let components = frame.color_format.component_count();
        let width = frame.width as usize;
        let height = frame.height as usize;
        // P3.b：首帧 push 时用真实 components 解析有损内核配置（frame_count 在
        // streaming 中未知保持 None；components 决定 chroma 采样自动解析与校验）。
        if self.tuning.is_none() {
            let tuning = crate::crf::core::config::lossy_v2::KernelLossyConfig::from_options(
                self.params.lossy.as_ref(),
                crate::crf::core::config::lossy_v2::ResolveContext {
                    components: Some(components),
                    frame_count: None,
                },
            )
            .map_err(|e| CrfError::InvalidCodingParams(e.to_string()))?;
            self.interval = tuning.anchor_interval.max(1) as usize;
            self.lossy_quant_step = tuning.enabled.then_some(tuning.global_step);
            self.tuning = Some(tuning);
        }
        let fq_base = self.frame_quant(self.frames_written);

        match &self.golden_rgb {
            None => {
                // ===== 首帧：编码 + P0 闭环重建 G_hat =====
                // 回填真实尺寸/位深/色彩格式至文件头（finish 校验用）
                self.header.width = frame.width;
                self.header.height = frame.height;
                self.header.bit_depth = frame.bit_depth;
                self.header.color_format = frame.color_format;
                // RCT 标志与批量路径对齐（3 分量即启用 YCoCg-R）
                let use_rct = components == 3;
                if use_rct {
                    let mut f = crate::crf::core::domain::Flags::new();
                    f.set_has_index(true);
                    f.set_has_rct(true);
                    self.header.flags = f;
                }
                // v1.13 RCT 首帧自适应：RCT 域与 RGB 直通各走完整管线，
                // 字节最小者胜出（平局保守保持 RCT）；直通胜出时置位
                // flags.bit3。两路 Fast-Fail 上限独立计算，不跨路传递。
                // 非 3 分量（Gray）无通道相关性可去除，直接直通编码
                // （与批量路径 use_rct=false 行为对齐；历史版本此处
                // 无条件 rct_forward 会令 Gray 序列报错）。
                let img_eff = if use_rct {
                    ImageData {
                        width: frame.width,
                        height: frame.height,
                        bit_depth: frame.bit_depth,
                        color_format: frame.color_format,
                        pixels: crate::crf::core::color::rct::rct_forward(
                            &frame.pixels,
                            components,
                        )?,
                    }
                } else {
                    ImageData {
                        width: frame.width,
                        height: frame.height,
                        bit_depth: frame.bit_depth,
                        color_format: frame.color_format,
                        pixels: frame.pixels.clone(),
                    }
                };
                let data_eff = encode_first_frame_bytes(&img_eff, self)?;
                let data = if use_rct {
                    let data_bypass = encode_first_frame_bytes(frame, self)?;
                    if data_bypass.len() < data_eff.len() {
                        self.header.flags.set_first_frame_no_rct(true);
                        data_bypass
                    } else {
                        data_eff
                    }
                } else {
                    data_eff
                };
                // P0 闭环：有损 golden 时后续差分必须以文件实际可用的重建首帧
                // G_hat = rct⁻¹(reconstruct_frame(encode(frame0))) 为参考，而不是
                // 原始 frame0——否则解码端 "G_hat + residual" 的还原式会把首帧
                // 量化误差传导进全部后续帧（与批量路径阶段 1c 同一契约）。
                // 重建依赖 header.lossy_quant（frame_type=8 反量化用），故首帧
                // push 时提前冻结（finish 时重复设置幂等，与批量 resolve 期
                // 冻结文件头语义一致）。
                if let Some(q) = self.lossy_quant_step {
                    self.header.lossy_quant = q;
                    self.header.flags.set_has_lossy_quant(true);
                }
                let mut g_hat_img =
                    crate::crf::decoder::reconstruct::reconstruct_frame(&data, &self.header)?;
                if self.header.flags.has_rct() && !self.header.flags.first_frame_no_rct() {
                    g_hat_img.pixels =
                        crate::crf::core::color::rct::rct_inverse(&g_hat_img.pixels, components)?;
                }
                // 保存 **RGB 域重建首帧 G_hat** 作为后续差分基准（历史缺陷曾存
                // 原始帧导致有损 golden 漂移；更早版本曾存 RCT 域导致跨域相减）
                self.golden_rgb = Some(g_hat_img.clone());
                // P5.1：初始化 previous 为 G_hat（与 golden 相同），用于 previous/hybrid 模式
                self.previous_rgb = Some(g_hat_img);
                self.frame_golden_flags.push(false); // 首帧不是 golden 差分
                self.frame_prev2_flags.push(false);
                self.append_frame(data);
            }
            Some(golden) => {
                // ===== 差分帧：SIMD 差分 → RCT → 编码 =====
                if golden.pixels.len() != frame.pixels.len() {
                    return Err(CrfError::ImageDimensionsMismatch {
                        expected: (golden.width, golden.height),
                        actual: (frame.width, frame.height),
                    });
                }
                let reference_mode = self
                    .tuning
                    .as_ref()
                    .map(|t| t.reference_mode)
                    .unwrap_or(crate::crf::core::config::lossy_v2::ReferenceModeV2::Golden);
                let use_previous = matches!(
                    reference_mode,
                    crate::crf::core::config::lossy_v2::ReferenceModeV2::Previous
                        | crate::crf::core::config::lossy_v2::ReferenceModeV2::Hybrid
                );

                // 差分帧编码管线（golden / LIC 共用，与 streaming 既有 golden
                // 路径逐参对齐）：RCT → band 步长 → 熵编码。入参为 RGB 域差分。
                let encode_diff =
                    |diff_rgb: Vec<i32>| -> crate::crf::error::CrfResult<Vec<u8>> {
                        let mut diff = diff_rgb;
                        crate::crf::core::color::rct::rct_forward_in_place(
                            &mut diff,
                            components,
                        )?;
                        let fq_band: Vec<u8> = if self.noise_on() || self.activity_on() {
                            fq_band_steps(
                                fq_base.step,
                                &diff,
                                width,
                                height,
                                components,
                                self.tuning
                                    .as_ref()
                                    .expect("lossy config resolved before band quantization"),
                            )
                        } else {
                            Vec::new()
                        };
                        let band_ref: super::frame::BandSteps<'_> =
                            if fq_band.is_empty() { None } else { Some(&fq_band) };
                        if self.params.adaptive_prediction {
                            Ok(encode_frame_adaptive(
                                &ImageData {
                                    width: frame.width,
                                    height: frame.height,
                                    bit_depth: frame.bit_depth,
                                    color_format: frame.color_format,
                                    pixels: diff,
                                },
                                self.compression_type,
                                self.header.block_size,
                                false,
                                fq_base,
                                None,
                                band_ref,
                            )?
                            .data)
                        } else {
                            Ok(super::encode_frame(
                                &ImageData {
                                    width: frame.width,
                                    height: frame.height,
                                    bit_depth: frame.bit_depth,
                                    color_format: frame.color_format,
                                    pixels: diff,
                                },
                                self.compression_type,
                                self.header.block_size,
                                self.header.prediction_mode,
                                false,
                                fq_base,
                                None,
                            )?)
                        }
                    };

                // golden 差分候选（帧头 reference_type=0）
                let mut diff_golden = vec![0i32; frame.pixels.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, &golden.pixels, &mut diff_golden);
                let mut final_data = encode_diff(diff_golden)?;
                let mut ref_type: u8 = 0; // 0=golden

                // v1.16 LIC 加权 golden 差分候选（与批量路径同决策逻辑）：
                // 采样扫描 → 预筛 → 同管线编码 → 字节竞争（单调不劣化）。
                // 仅在更小时采用；字段延迟到 reference 竞争结束后写入
                // （final_data 可能被 previous/prev2 覆盖）。
                let mut lic_adopt: Option<(u8, u8)> = None;
                if super::sequence_tools::lic_globally_enabled() {
                    if let Some(fit) =
                        crate::crf::core::illumination::search_lic(&golden.pixels, &frame.pixels)
                    {
                        if fit.worthwhile() {
                            let mut lic_ref = vec![0i32; golden.pixels.len()];
                            crate::crf::core::illumination::fit_into(
                                &golden.pixels,
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
                            if data_lic.len() < final_data.len() {
                                final_data = data_lic;
                                lic_adopt = Some((fit.a_num as u8, fit.b as i8 as u8));
                            }
                        }
                    }
                }
                if use_previous {
                    let previous = self
                        .previous_rgb
                        .as_ref()
                        .expect("previous_rgb must be set after first frame");
                    let mut diff_previous = vec![0i32; frame.pixels.len()];
                    crate::crf::backend::ops::sub_i32(
                        &frame.pixels,
                        &previous.pixels,
                        &mut diff_previous,
                    );

                    // P5.2 稀疏变化 mask（与批量路径对齐）：静止 tile 直接写零。
                    let tuning = self.tuning.as_ref().expect("tuning must be set");
                    if !matches!(
                        tuning.change_mask,
                        crate::crf::core::config::lossy_v2::ToolMode::Off
                    ) {
                        let ts = self.header.block_size.max(4) as usize;
                        let mask = super::sequence_tools::change_mask(
                            &frame.pixels,
                            &previous.pixels,
                            width,
                            height,
                            components,
                            ts,
                            if fq_base.step > 0 {
                                (fq_base.step / 2) as i32
                            } else {
                                0
                            },
                        );
                        super::sequence_tools::apply_change_mask(
                            &mut diff_previous,
                            &mask,
                            width,
                            height,
                            components,
                            ts,
                        );
                    }

                    // 场景切换检测（与批量路径对齐；self.frames_written 为当前帧
                    // 0-based 索引，批量路径 i 从 1 起 → frame_index = written + 1）
                    let frame_index = self.frames_written + 1;
                    let periodic_anchor = tuning.anchor_interval > 0
                        && frame_index.is_multiple_of(tuning.anchor_interval as usize);
                    let scene_cut = periodic_anchor
                        || (!matches!(
                            tuning.scene_cut,
                            crate::crf::core::config::lossy_v2::SceneCutModeV2::Off
                        ) && (diff_previous
                            .iter()
                            .map(|v| v.unsigned_abs() as u64)
                            .sum::<u64>()
                            / diff_previous.len().max(1) as u64)
                            > ((tuning.scene_cut_threshold_x1000 as u64 * 255) / 1000));
                    let force_previous = matches!(
                        reference_mode,
                        crate::crf::core::config::lossy_v2::ReferenceModeV2::Previous
                    );
                    let use_prev2 = matches!(
                        reference_mode,
                        crate::crf::core::config::lossy_v2::ReferenceModeV2::Hybrid
                    );

                    // 参考候选编码（闭包）：diff vs 参考 → change_mask → RCT → 编码。
                    let encode_ref = |ref_pixels: &[i32]| -> crate::crf::error::CrfResult<Vec<u8>> {
                        let mut diff = vec![0i32; frame.pixels.len()];
                        crate::crf::backend::ops::sub_i32(&frame.pixels, ref_pixels, &mut diff);
                        if !matches!(
                            tuning.change_mask,
                            crate::crf::core::config::lossy_v2::ToolMode::Off
                        ) {
                            let ts = self.header.block_size.max(4) as usize;
                            let mask = super::sequence_tools::change_mask(
                                &frame.pixels,
                                ref_pixels,
                                width,
                                height,
                                components,
                                ts,
                                if fq_base.step > 0 {
                                    (fq_base.step / 2) as i32
                                } else {
                                    0
                                },
                            );
                            super::sequence_tools::apply_change_mask(
                                &mut diff, &mask, width, height, components, ts,
                            );
                        }
                        crate::crf::core::color::rct::rct_forward_in_place(&mut diff, components)?;
                        let eff = ImageData {
                            width: frame.width,
                            height: frame.height,
                            bit_depth: frame.bit_depth,
                            color_format: frame.color_format,
                            pixels: diff,
                        };
                        if self.params.adaptive_prediction {
                            Ok(encode_frame_adaptive(
                                &eff,
                                self.compression_type,
                                self.header.block_size,
                                false,
                                fq_base,
                                None,
                                None,
                            )?
                            .data)
                        } else {
                            Ok(super::encode_frame(
                                &eff,
                                self.compression_type,
                                self.header.block_size,
                                self.header.prediction_mode,
                                false,
                                fq_base,
                                None,
                            )?)
                        }
                    };

                    if !scene_cut {
                        // previous 候选（reference_type=1）
                        let data_previous = encode_ref(&previous.pixels)?;
                        if !data_previous.is_empty()
                            && (force_previous || data_previous.len() < final_data.len())
                        {
                            final_data = data_previous;
                            ref_type = 1;
                        }
                        // prev2 候选（reference_type=2，仅 Hybrid；Previous 模式
                        // 保持纯 previous 链的误差累积语义）
                        if use_prev2 && ref_type != 1 {
                            if let Some(p2) = &self.prev2_rgb {
                                let data_p2 = encode_ref(&p2.pixels)?;
                                if !data_p2.is_empty() && data_p2.len() < final_data.len() {
                                    final_data = data_p2;
                                    ref_type = 2;
                                }
                            }
                        }
                    }
                }

                // 写入帧头 reference_type（v1.15；固定偏移，不随帧头尺寸变化）
                if final_data.len() > FRAME_HEADER_SIZE {
                    final_data[REFERENCE_TYPE_OFFSET] = ref_type;
                }
                // v1.16：写入 LIC 信令（仅最终参考仍为 golden 且 LIC 胜出时；
                // previous/prev2 候选为新编码帧、LIC 字段为默认 (0,0)，与
                // 批量路径对称）
                if ref_type == 0 {
                    if let Some((a_num, b)) = lic_adopt {
                        if final_data.len() > FRAME_HEADER_SIZE {
                            final_data[LIC_A_NUM_OFFSET] = a_num;
                            final_data[LIC_B_OFFSET] = b;
                        }
                    }
                }

                // 更新参考重建链（prev2 = 旧 previous；previous = 参考基准 + 当前差分重建）
                // 还原公式与解码端 restore_temporal 一致（基准 + rgb）。
                {
                    let recon = crate::crf::decoder::reconstruct::reconstruct_frame(
                        &final_data,
                        &self.header,
                    )?;
                    let mut rgb = recon.pixels;
                    if self.header.flags.has_rct() {
                        rgb = crate::crf::core::color::rct::rct_inverse(&rgb, components)?;
                    }
                    let base: Option<Vec<i32>> = if ref_type == 2 {
                        // prev2 参考：prev2 + rgb
                        self.prev2_rgb.as_ref().map(|p2| {
                            p2.pixels
                                .iter()
                                .zip(rgb.iter())
                                .map(|(a, b)| a + b)
                                .collect()
                        })
                    } else if ref_type == 1 {
                        // previous 参考：旧 previous + rgb
                        self.previous_rgb.as_ref().map(|p| {
                            p.pixels
                                .iter()
                                .zip(rgb.iter())
                                .map(|(a, b)| a + b)
                                .collect()
                        })
                    } else {
                        // golden 参考：golden + rgb。v1.16：本帧启用 LIC 时
                        // 参考先经乘加加权 LIC(golden)（与解码端
                        // restore_referenced 严格对称）。
                        self.golden_rgb.as_ref().map(|g| {
                            let lic_a = if final_data.len() > FRAME_HEADER_SIZE {
                                final_data[LIC_A_NUM_OFFSET]
                            } else {
                                0
                            };
                            let lic_b = if final_data.len() > FRAME_HEADER_SIZE {
                                final_data[LIC_B_OFFSET]
                            } else {
                                0
                            };
                            let weighted: Vec<i32> = if lic_a != 0 {
                                crate::crf::core::illumination::apply_lic_weighted(
                                    &g.pixels,
                                    lic_a,
                                    lic_b,
                                )
                            } else {
                                g.pixels.clone()
                            };
                            weighted
                                .iter()
                                .zip(rgb.iter())
                                .map(|(a, b)| a + b)
                                .collect()
                        })
                    };
                    self.prev2_rgb = self.previous_rgb.take();
                    if let Some(b) = base {
                        self.previous_rgb = Some(ImageData {
                            width: frame.width,
                            height: frame.height,
                            bit_depth: frame.bit_depth,
                            color_format: frame.color_format,
                            pixels: b,
                        });
                    }
                }

                self.frame_golden_flags.push(ref_type == 0);
                self.frame_prev2_flags.push(ref_type == 2);
                self.append_frame(final_data);
            }
        }
        Ok(())
    }

    /// 结束编码并输出完整 CRF 文件字节
    pub fn finish(mut self) -> CrfResult<Vec<u8>> {
        if self.frames_written < 2 {
            return Err(CrfError::FrameCountOutOfRange(self.frames_written as u16));
        }
        self.finished = true;

        let frame_count = self.frames_written as u16;

        // 重建最终文件头（真实尺寸/帧数/标志）
        let mut header = self.header.clone();
        header.frame_count = frame_count;
        // 在 push_frame 已设置的标志（has_index/has_rct）基础上追加有损标记
        if let Some(q) = self.lossy_quant_step {
            header.lossy_quant = q;
            header.flags.set_has_lossy_quant(true);
        }
        header.validate()?;

        let index_size = frame_count as usize * 8;
        let total_size = HEADER_SIZE + index_size + self.body.len() + FOOTER_SIZE;
        let mut output = Vec::with_capacity(total_size);

        // 完整文件头（含 magic，64B）
        header.write_bytes(&mut output)?;

        // 帧索引（offset 相对文件起始；首帧位于 header+index 之后）
        let frames_start = HEADER_SIZE + index_size;

        // 帧索引：相对偏移换算为绝对偏移（frames_start + body 内偏移）
        for (rel, size) in &self.frame_layout {
            let abs = (frames_start + rel) as u32;
            output.extend_from_slice(&abs.to_le_bytes());
            output.extend_from_slice(&(*size as u32).to_le_bytes());
        }

        // 帧数据
        output.extend_from_slice(&self.body);

        // CRC32（不含文件尾）
        let crc = crc32(&output);
        output.extend_from_slice(&crc.to_le_bytes());
        output.extend_from_slice(&FOOTER_MAGIC);

        Ok(output)
    }

    fn append_frame(&mut self, data: Vec<u8>) {
        // 记录帧在 body 内的相对布局（绝对偏移在 finish 时统一换算，
        // 因为 push 时总帧数未知 → 索引区大小未知）
        self.frame_layout.push((self.body.len(), data.len()));
        self.body.extend_from_slice(&data);
        self.frames_written += 1;
    }

    fn noise_on(&self) -> bool {
        let tuning = self
            .tuning
            .as_ref()
            .expect("lossy config resolved before noise check");
        self.lossy_quant_step.is_some()
            && tuning.noise_adaptive
            && self.header.color_format.component_count() == 3
    }

    fn activity_on(&self) -> bool {
        let tuning = self
            .tuning
            .as_ref()
            .expect("lossy config resolved before activity check");
        self.lossy_quant_step.is_some()
            && (tuning.activity_masking_x100 != 100
                || tuning.flat_area_protection_x100 != 100
                || tuning.edge_protection_x100 != 100)
            && self.header.color_format.component_count() == 3
    }

    fn frame_quant(&self, i: usize) -> FrameQuant {
        // 与批量路径共用同一逐帧量化配置（规划文档 §3.2：batch/streaming
        // 不得分别解析）。修复历史语义分叉：
        //  - 旧实现 `i == 0 → lossless()` 恒强制首帧无损，忽略
        //    golden_lossless=false（批量路径允许有损首帧）；
        //  - 旧实现无锚点帧间隔（keyframe_interval）步长折算。
        // 现统一委托 session::batch::fq_for_index，语义与 batch 完全一致。
        let tuning = self
            .tuning
            .as_ref()
            .expect("lossy config resolved before frame quantization");
        let q95 = tuning.q95_perceptual;
        super::session::batch::fq_for_index(
            i,
            self.lossy_quant_step,
            tuning,
            tuning.deadzone_bias,
            self.interval,
            q95,
        )
    }
}

fn params_color(_p: &EncodeParams) -> crate::crf::core::domain::ColorFormat {
    crate::crf::core::domain::ColorFormat::Rgb
}

/// 首帧编码统一入口（流式路径）：自适应/固定模式分流。
/// 首帧量化档位由 `frame_quant(0)` 决定（golden_lossless=true 时恒无损，
/// 与批量路径 fq_for_index 语义一致；false 时按锚点档位量化）。
/// 供 RCT 双路竞争的两条路复用。
fn encode_first_frame_bytes(img: &ImageData, enc: &StreamingEncoder) -> CrfResult<Vec<u8>> {
    let fq = enc.frame_quant(0);
    if enc.params.adaptive_prediction {
        Ok(encode_frame_adaptive(
            img,
            enc.compression_type,
            enc.header.block_size,
            true,
            fq,
            None,
            None,
        )?
        .data)
    } else {
        super::encode_frame(
            img,
            enc.compression_type,
            enc.header.block_size,
            enc.header.prediction_mode,
            true,
            fq,
            None,
        )
    }
}

#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
fn write_header_fields(header: &CrfHeader, out: &mut Vec<u8>) -> CrfResult<()> {
    header.write_bytes(out)
}

fn fq_band_steps(
    fq_step: u8,
    eff_pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    tuning: &crate::crf::core::config::lossy_v2::KernelLossyConfig,
) -> Vec<u8> {
    if tuning.activity_masking_x100 != 100
        || tuning.flat_area_protection_x100 != 100
        || tuning.edge_protection_x100 != 100
    {
        // P4.2/P4.3/P4.4 activity masking：空间梯度能量步长
        use crate::crf::core::perceptual::noise::estimate_band_activity_steps;
        estimate_band_activity_steps(
            eff_pixels,
            width,
            height,
            components,
            fq_step,
            tuning.activity_masking_x100,
            tuning.flat_area_protection_x100,
            tuning.edge_protection_x100,
        )
    } else {
        estimate_band_quant_steps(
            eff_pixels,
            width,
            height,
            components,
            fq_step,
            tuning.noise_tau_x100,
        )
    }
}

// FRAME_HEADER_SIZE 引用占位
#[allow(unused_imports)]
const _: usize = FRAME_HEADER_SIZE;
