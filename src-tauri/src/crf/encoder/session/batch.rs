//! 批量编码辅助函数（提取自 `sequence.rs`，P3 架构迁移）
//!
//! 包含：
//! - [`fq_for_index`]：路径 G 的逐帧量化配置；
//! - [`fq_for_chain_index`]：路径 C 的逐帧量化配置；
//! - [`assemble_crf_output`]：文件头/帧索引/帧数据/CRC 组装。

use crate::crf::checksum::crc32;
use crate::crf::core::bitstream::constants::{FOOTER_MAGIC, FOOTER_SIZE, HEADER_SIZE};
use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::config::lossy_v2::KernelLossyConfig;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::error::CrfResult;

/// 路径 G 的逐帧量化配置
///
/// 从 `encode_sequence` 提取（P3 架构迁移）。golden 首帧（i==0）强制无损——
/// 它是全部差分帧的还原基准，其量化误差会传导进每一帧的重建结果。
/// 间隔锚点帧按 anchor_quality_percent 折算步长。
pub fn fq_for_index(
    i: usize,
    lossy_quant_step: Option<u8>,
    tuning: &KernelLossyConfig,
    base_bias: i8,
    interval: usize,
    q95: bool,
) -> FrameQuant {
    let global_q = lossy_quant_step.unwrap_or(0);
    if global_q == 0 {
        return FrameQuant::lossless();
    }
    let is_anchor = i.is_multiple_of(interval);
    let step = if i == 0 {
        tuning.first_frame_step
    } else if is_anchor {
        tuning.anchor_step
    } else {
        global_q
    };
    if step == 0 {
        return FrameQuant::lossless();
    }
    FrameQuant {
        step,
        bias: base_bias,
        chroma_step: tuning.chroma_step(step),
        chroma_bias: tuning.chroma_deadzone_bias,
        chroma_half_res: tuning.chroma_half_res,
        q1_matrix_scale: q95,
    }
}

/// 路径 C（链式差分）的逐帧量化配置
///
/// 关键帧间隔边界走无损刷新（阻断误差累积），其余帧用全局档位。
/// 色度半分辨率与亮度步长解耦（P1a 遗留项）：与路径 G 的 [`fq_for_index`]
/// 一致，直接由 `tuning.chroma_half_res` 决定，不再受 `gq > 1` 隐式阻断。
pub fn fq_for_chain_index(
    i: usize,
    lossy_quant_step: Option<u8>,
    tuning: &KernelLossyConfig,
    base_bias: i8,
) -> FrameQuant {
    let gq = lossy_quant_step.unwrap_or(0);
    if gq == 0 || i.is_multiple_of(tuning.anchor_interval.max(1) as usize) {
        return FrameQuant::lossless();
    }
    FrameQuant {
        step: gq,
        bias: base_bias,
        chroma_step: tuning.chroma_step(gq),
        chroma_bias: tuning.chroma_deadzone_bias,
        chroma_half_res: tuning.chroma_half_res,
        q1_matrix_scale: false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺陷回归：路径 C 的色度半分辨率曾被 `gq > 1` 隐式阻断，
    /// 与路径 G（P1a 已解耦）不一致。q95（gq=1）下半分辨率应同样
    /// 由 `tuning.chroma_half_res` 决定。
    #[test]
    fn fq_for_chain_index_chroma_half_res_decoupled_from_step() {
        let mut tuning = KernelLossyConfig::lossless();
        tuning.enabled = true;
        tuning.global_step = 1;
        tuning.anchor_interval = 5; // 避免 max(1) 把全部帧当锚点边界
        tuning.chroma_half_res = true;

        // q95（gq=1）：修复前 `gq > 1 &&` 使其为 false，修复后为 true
        let fq_q95 = fq_for_chain_index(1, Some(1), &tuning, 0);
        assert!(
            fq_q95.chroma_half_res,
            "gq=1（q95）路径 C 半分辨率应跟随 tuning.chroma_half_res（不再被 gq>1 阻断）"
        );

        // q90（gq=2）：保持 true
        let fq_q90 = fq_for_chain_index(1, Some(2), &tuning, 0);
        assert!(fq_q90.chroma_half_res, "gq=2 路径 C 半分辨率应开启");

        // 显式关闭
        tuning.chroma_half_res = false;
        let fq_off = fq_for_chain_index(1, Some(2), &tuning, 0);
        assert!(!fq_off.chroma_half_res, "chroma_half_res=false 时应关闭");
    }

    /// 路径 C 锚点边界（interval 倍数帧）强制无损刷新
    #[test]
    fn fq_for_chain_index_anchor_boundary_lossless() {
        let mut tuning = KernelLossyConfig::lossless();
        tuning.enabled = true;
        tuning.global_step = 2;
        tuning.anchor_interval = 5;

        let fq_anchor = fq_for_chain_index(5, Some(2), &tuning, 0);
        assert_eq!(fq_anchor.step, 0, "锚点边界帧应为无损刷新");
        let fq_normal = fq_for_chain_index(3, Some(2), &tuning, 0);
        assert_eq!(fq_normal.step, 2, "非边界帧使用全局档位");
    }
}
