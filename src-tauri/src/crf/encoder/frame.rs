//! 单帧编码层：帧级 API 与帧头装配
//!
//! - [`encode_frame`] / [`encode_frame_inner`]：按固定预测模式编码单帧
//!   （有损时走闭环预测+量化，杜绝误差漂移）；
//! - [`assemble_frame`]：将已编码载荷装配为带帧头的完整帧缓冲；
//! - [`FrameQuant`]：单帧量化配置（步长/死区偏置/色度参数）。

use crate::crf::error::CrfResult;
use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::domain::{CompressionType, FrameHeader, ImageData, PredictionMode};
use crate::crf::core::prediction::intra::apply_prediction_into;
use crate::crf::format::closed_loop::closed_loop_predict_quant_banded_into;

use super::scratch::FrameScratch;

/// 单帧量化配置（真有损）
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameQuant {
    /// 量化步长：0 = 无损
    pub step: u8,
    /// 死区偏置（i8）
    pub bias: i8,
    /// 色度平面步长（planar 的 Co/Cg 用；0=与 step 相同）
    pub chroma_step: u8,
    /// 色度死区偏置（P1 色度精细化）：planar 的 Co/Cg 子平面使用；
    /// 由 V2 `quant.deadzoneChromaX256` 在构造点解析。
    pub chroma_bias: i8,
    /// 色度半分辨率标志（planar 的 Co/Cg 用 2×2 均值下采样，对标 yuv420p）
    pub chroma_half_res: bool,
    /// v1.12 q95 视觉无损档：Q=1 下许可感知矩阵缩放（高频 Q_pos=2）。
    /// 仅影响 DCT 候选；空间域候选在 Q=1 下恒为无损体积，竞争自动让位。
    pub q1_matrix_scale: bool,
}

impl FrameQuant {
    pub(crate) fn lossless() -> Self {
        FrameQuant {
            step: 0,
            bias: 0,
            chroma_step: 0,
            chroma_bias: 0,
            chroma_half_res: false,
            q1_matrix_scale: false,
        }
    }
    pub(crate) fn is_lossy(&self) -> bool {
        self.step > 0 || self.q1_matrix_scale
    }
}

/// 逐条带自适应步长表：每 BAND_HEIGHT 行一个有效量化步长，
/// 由噪声感知估计器生成（见 noise::estimate_band_quant_steps）。
/// 以独立参数沿调用链传递，保持 FrameQuant 的 Copy 语义。
pub(crate) type BandSteps<'a> = Option<&'a [u8]>;

/// 编码单帧（固定预测模式）
///
/// 根据压缩类型选择编码方式，返回帧头 + 编码数据。
/// 帧头 pred_mode 记录本帧实际使用的预测模式，文件完全自描述。
/// is_first_frame: 是否为第一帧（原始帧），用于选择不同的编码策略
/// lossy_quant: 有损量化步长（0=无损）
/// band_steps: 逐条带自适应步长表（噪声归一化；None=全帧统一）
pub fn encode_frame(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    prediction_mode: PredictionMode,
    is_first_frame: bool,
    fq: FrameQuant,
    band_steps: crate::crf::encoder::frame::BandSteps<'_>,
) -> CrfResult<Vec<u8>> {
    encode_frame_inner(
        image,
        compression_type,
        block_size,
        prediction_mode,
        is_first_frame,
        fq,
        band_steps,
    )
}

/// 编码单帧内部实现（按给定模式执行预测与熵编码）
///
/// `byte_limit`：Fast-Fail 上限（v1.11）——熵编码产物超过该字节数即终止，
/// 返回 None（该候选必败）。`usize::MAX` 表示无限制（行为与旧版一致）。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub(crate) fn encode_frame_inner_limited(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    prediction_mode: PredictionMode,
    is_first_frame: bool,
    fq: FrameQuant,
    band_steps: BandSteps<'_>,
    byte_limit: usize,
) -> CrfResult<Option<Vec<u8>>> {
    let mut scratch = FrameScratch::default();
    encode_frame_inner_limited_with_scratch(
        image,
        compression_type,
        block_size,
        prediction_mode,
        is_first_frame,
        fq,
        band_steps,
        byte_limit,
        &mut scratch,
    )
}

/// [`encode_frame_inner_limited`] 的 Scratch Buffer 复用版本。
///
/// 自适应仲裁在同一帧的多个候选间传入同一个 `scratch`，从而避免每次
/// 预测都重新申请整帧残差/重建缓冲；独立调用仍由兼容包装自动创建缓冲。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub(crate) fn encode_frame_inner_limited_with_scratch(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    prediction_mode: PredictionMode,
    is_first_frame: bool,
    fq: FrameQuant,
    band_steps: BandSteps<'_>,
    byte_limit: usize,
    scratch: &mut FrameScratch,
) -> CrfResult<Option<Vec<u8>>> {
    let width = image.width as usize;
    let height = image.height as usize;
    let components = image.color_format.component_count();

    // 应用帧内预测；真有损走闭环（预测邻居取自重建缓冲，杜绝误差漂移）。
    // band_steps 提供逐条带自适应步长（噪声归一化），闭环逐行查表生效；
    // 解码端无感（输出为各条带 Q_eff 倍数的自描述残差）。
    let pixels: &[i32] = if fq.is_lossy() {
        let (residuals, reconstruction) = scratch.closed_loop(image.pixels.len());
        closed_loop_predict_quant_banded_into(
            &image.pixels,
            residuals,
            reconstruction,
            width,
            height,
            components,
            prediction_mode,
            fq.step,
            fq.bias,
            band_steps,
        );
        residuals
    } else {
        let residuals = scratch.residuals(image.pixels.len());
        apply_prediction_into(
            &image.pixels,
            residuals,
            width,
            height,
            components,
            prediction_mode,
        );
        residuals
    };

    // 熵编码可用预算 = 总上限 − 帧头
    let payload_limit = byte_limit.saturating_sub(FRAME_HEADER_SIZE);

    // 根据压缩类型编码，同时确定 frame_type
    // 0=块级自适应k Golomb, 1=RLE+Golomb, 2=条带自适应（见 encode_frame_banded_adaptive）
    //
    // Fast-Fail（v1.11）：残差帧 RLE 路径支持字节上限短路——位流单调增长，
    // 超限即必败。首帧双路竞争与 ExpGolomb/Transform 路径保持无限制。
    let encoded: Option<(Vec<u8>, u8, u8)> = match compression_type {
        CompressionType::GolombRice => {
            if is_first_frame {
                // 首帧双路竞争：块级自适应k Golomb vs RLE+Golomb，取字节数最小者。
                // 自然图像经 MED/平均预测后残差零值占比高时 RLE 显著更优；
                // 高熵纹理则块级 k 的 Golomb 更稳。解码端按 frame_type 自动分流。
                let r_block = super::golomb::encode_frame_golomb_block_adaptive(
                    pixels, width, height, components,
                )?;
                let r_rle = super::rle_golomb::encode_frame_rle_golomb_adaptive(pixels)?;
                if r_rle.0.len() < r_block.0.len() {
                    Some((r_rle.0, r_rle.1, 1))
                } else {
                    Some((r_block.0, r_block.1, 0))
                }
            } else {
                // 残差帧使用 RLE+Golomb 混合编码（针对大面积零值优化）。
                // Fast-Fail：超预算即返回 None（该候选必败）。
                super::rle_golomb::encode_frame_rle_golomb_adaptive_limited(pixels, payload_limit)?
                    .map(|(data, k)| (data, k, 1))
            }
        }
        CompressionType::ExpGolomb => {
            let data = super::exp_golomb::encode_frame_exp_golomb(pixels)?;
            Some((data, 0, 0)) // 指数哥伦布不需要 k 参数
        }
        CompressionType::Transform => {
            let data = super::transform::encode_frame_transform(
                pixels,
                width,
                height,
                block_size as usize,
            )?;
            Some((data, 0, 0)) // k 值在块内编码
        }
    };

    let Some((encoded_data, coding_params, frame_type)) = encoded else {
        return Ok(None); // Fast-Fail 触发：该候选必败
    };

    // 构建帧头：data_len = 像素数 × 分量数
    let data_len = (width * height) as u32 * image.color_format.component_count() as u32;

    let mut frame_header = FrameHeader::with_type(
        encoded_data.len() as u32,
        data_len,
        coding_params,
        frame_type,
    );
    // 记录本帧实际使用的预测模式，解码端无需依赖文件头全局设置
    frame_header.pred_mode = prediction_mode as u8;

    // 写入帧头 + 编码数据
    let mut frame_buffer = Vec::with_capacity(FRAME_HEADER_SIZE + encoded_data.len());
    frame_header.write_bytes(&mut frame_buffer)?;
    frame_buffer.extend_from_slice(&encoded_data);

    Ok(Some(frame_buffer))
}

/// 兼容包装：无 Fast-Fail 限制的单帧编码
pub(crate) fn encode_frame_inner(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    prediction_mode: PredictionMode,
    is_first_frame: bool,
    fq: FrameQuant,
    band_steps: BandSteps<'_>,
) -> CrfResult<Vec<u8>> {
    encode_frame_inner_limited(
        image,
        compression_type,
        block_size,
        prediction_mode,
        is_first_frame,
        fq,
        band_steps,
        usize::MAX,
    )
    .map(|o| o.expect("无限制下必然产出"))
}

/// 将已编码载荷装配为带帧头的完整帧缓冲
///
/// 条带(2)/三平面(3)/调色板(4)等无单一帧级预测模式的路径使用本入口，
/// pred_mode 置为 UNSET（0xFF），由载荷自描述或解码端分流处理。
pub(crate) fn assemble_frame(
    encoded_data: &[u8],
    image: &ImageData,
    coding_params: u8,
    frame_type: u8,
) -> CrfResult<Vec<u8>> {
    let data_len = (image.width as usize
        * image.height as usize
        * image.color_format.component_count()) as u32;
    let mut header = FrameHeader::with_type(
        encoded_data.len() as u32,
        data_len,
        coding_params,
        frame_type,
    );
    header.pred_mode = PredictionMode::PRED_MODE_UNSET; // 条带模式逐条带指定，帧级字段置空
    let mut buffer = Vec::with_capacity(FRAME_HEADER_SIZE + encoded_data.len());
    header.write_bytes(&mut buffer)?;
    buffer.extend_from_slice(encoded_data);
    Ok(buffer)
}
