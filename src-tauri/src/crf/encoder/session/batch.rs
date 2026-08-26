//! 批量编码辅助函数（提取自 `sequence.rs`，P3 架构迁移）
//!
//! 包含：
//! - [`fq_for_index`]：路径 G 的逐帧量化配置；
//! - [`fq_for_chain_index`]：路径 C 的逐帧量化配置；
//! - [`assemble_crf_output`]：文件头/帧索引/帧数据/CRC 组装。

use crate::crf::checksum::crc32;
use crate::crf::error::CrfResult;
use crate::crf::format::{CrfHeader, LossyTuning, FOOTER_MAGIC, FOOTER_SIZE, HEADER_SIZE};
use crate::crf::encoder::frame::FrameQuant;

/// 路径 G 的逐帧量化配置
///
/// 从 `encode_sequence` 提取（P3 架构迁移）。golden 首帧（i==0）强制无损——
/// 它是全部差分帧的还原基准，其量化误差会传导进每一帧的重建结果。
/// 间隔锚点帧按 anchor_quality_percent 折算步长。
pub fn fq_for_index(
    i: usize,
    lossy_quant_step: Option<u8>,
    tuning: &LossyTuning,
    base_bias: i8,
    interval: usize,
    q95: bool,
) -> FrameQuant {
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
        chroma_bias: tuning.chroma_deadzone_bias.unwrap_or(base_bias),
        chroma_half_res: tuning.chroma_half_res,
        q1_matrix_scale: q95,
    }
}

/// 路径 C（链式差分）的逐帧量化配置
///
/// 关键帧间隔边界走无损刷新（阻断误差累积），其余帧用全局档位。
pub fn fq_for_chain_index(
    i: usize,
    lossy_quant_step: Option<u8>,
    tuning: &LossyTuning,
    base_bias: i8,
) -> FrameQuant {
    let gq = lossy_quant_step.unwrap_or(0);
    if gq == 0 || i.is_multiple_of(tuning.keyframe_interval.max(1) as usize) {
        return FrameQuant::lossless();
    }
    FrameQuant {
        step: gq,
        bias: base_bias,
        chroma_step: tuning.chroma_step(gq),
        chroma_bias: tuning.chroma_deadzone_bias.unwrap_or(base_bias),
        chroma_half_res: gq > 1 && tuning.chroma_half_res,
        q1_matrix_scale: false,
    }
}

/// 由步长反推质量档位（仅用于 q95 判定）
pub fn quality_of_step(_step: u8) -> u8 {
    0
}

/// 组装 CRF 输出文件
///
/// 从 `encode_sequence` 提取（P3 架构迁移）。
/// 写入顺序：文件头 → 帧索引 → 编码帧数据 → CRC32 → 文件尾魔数。
pub fn assemble_crf_output(
    header: &CrfHeader,
    frames_start: usize,
    encoded_frames: &[(Vec<u8>, u32, u32, bool)],
) -> CrfResult<Vec<u8>> {
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
    for (_, offset, size, _) in encoded_frames {
        output.extend_from_slice(&offset.to_le_bytes());
        output.extend_from_slice(&size.to_le_bytes());
    }

    // 写入编码后的帧数据
    for (frame_data, _, _, _) in encoded_frames {
        output.extend_from_slice(frame_data);
    }

    // 计算 CRC32（不含文件尾）
    let crc = crc32(&output);

    // 写入文件尾
    output.extend_from_slice(&crc.to_le_bytes());
    output.extend_from_slice(&FOOTER_MAGIC);

    Ok(output)
}