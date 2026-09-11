//! CRF 格式处理模块
//!
//! 本模块实现了 CRF (Compressing Residual Frames) 格式的编解码功能。
//! CRF 是一种轻量级的无损压缩格式，专为差分图片序列设计。

pub mod backend;
pub mod checksum;
// ===== P0 架构迁移：新增分层骨架（规划文档 §2）=====
// codec: 对外 facade（P3 接入生产路径，当前为空骨架）
// core: 编解码器共享的纯契约/数学（P1~P4 逐步迁入）
// backend: 性能后端层（P5 接入 scalar/CPU SIMD/GPU）
// 以下三个模块声明仅增加，不删除现有模块。迁移期旧路径保持生产。
pub mod codec;
pub mod core;
pub mod decoder;
pub mod encoder;
pub mod error;
/// performance：性能可观测性层（阶段计时 telemetry + 端到端 bench，P0）
pub mod performance;

// 重新导出常用类型（公共 API 面，供集成与测试路径使用）
// P4 后类型统一自 core::domain / core::bitstream / core::config 提供
#[allow(unused_imports)]
pub use core::config::lossy_v2::{
    expert_panel_schema, ChromaSampling, ConfigError, LossyBase, LossyOptionsV2,
    LossyOptionsV2Builder, ResolvedLossyReport,
};
#[allow(unused_imports)]
pub use core::domain::{ColorFormat, DecodeResult, EncodeParams, ImageData, PredictionMode};
#[allow(unused_imports)]
pub use core::prediction::cost::satd_for_mode_sampled;
#[allow(unused_imports)]
pub use core::prediction::intra::apply_prediction;
#[allow(unused_imports)]
pub use core::transform::closed_loop::closed_loop_predict_quant_banded;
#[allow(unused_imports)]
pub use decoder::{decode_from_bytes, decode_from_file};
#[allow(unused_imports)]
pub use encoder::{encode_frame, encode_sequence};
pub use error::CrfResult;

/// CRF 格式版本信息
#[allow(dead_code)] // 公共版本标识，供外部集成与测试使用
pub const VERSION: &str = "1.0.0";

/// 获取 CRF 格式版本
#[allow(dead_code)] // 公共查询接口，测试路径使用
pub fn version() -> &'static str {
    VERSION
}

/// 快捷编码函数
///
/// 将图像序列编码为 CRF 格式。经 codec facade 转发（规划文档 §2：
/// 应用层只能依赖 facade）。
#[allow(dead_code)] // 公共快捷入口，仅测试路径调用
pub fn encode(
    frames: &[ImageData],
    compression_type: &str,
    user_metadata: Option<&str>,
) -> CrfResult<Vec<u8>> {
    let params = EncodeParams {
        compression_type: compression_type.to_string(),
        block_size: None,
        prediction_mode: PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
        input_original_frames: false,
        user_metadata: user_metadata.map(|s| s.to_string()),
    };
    codec::encode(codec::EncodeRequest {
        frames: frames.to_vec(),
        params,
    })
    .map(|report| report.bytes)
    .map_err(|e| match e {
        codec::CodecError::Internal(inner) => inner,
        codec::CodecError::NotImplemented(what) => {
            crate::crf::error::CrfError::InvalidCodingParams(what.to_string())
        }
        codec::CodecError::InvalidInput(msg) => {
            crate::crf::error::CrfError::InvalidCodingParams(msg)
        }
    })
}

/// 快捷解码函数
///
/// 将 CRF 数据解码为图像序列。经 codec facade 转发。
#[allow(dead_code)] // 公共快捷入口，仅测试路径调用
pub fn decode(data: &[u8]) -> CrfResult<DecodeResult> {
    codec::decode_from_bytes(codec::DecodeRequest {
        bytes: data.to_vec(),
    })
    .map_err(|e| match e {
        codec::CodecError::Internal(inner) => inner,
        codec::CodecError::NotImplemented(what) => {
            crate::crf::error::CrfError::InvalidCodingParams(what.to_string())
        }
        codec::CodecError::InvalidInput(msg) => {
            crate::crf::error::CrfError::InvalidCodingParams(msg)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_frames(count: usize, width: u16, height: u16) -> Vec<ImageData> {
        (0..count)
            .map(|i| {
                let pixels: Vec<i32> = (0..(width as usize * height as usize))
                    .map(|j| ((i * 10 + j) % 256) as i32 - 128)
                    .collect();
                ImageData {
                    width,
                    height,
                    bit_depth: 8,
                    color_format: ColorFormat::Gray,
                    pixels,
                }
            })
            .collect()
    }

    #[test]
    fn test_quick_encode_decode_golomb() {
        let frames = create_test_frames(5, 16, 16);
        let encoded = encode(&frames, "golomb-rice", Some("test")).unwrap();
        let result = decode(&encoded).unwrap();

        assert_eq!(result.frames.len(), frames.len());
        for (original, decoded) in frames.iter().zip(result.frames.iter()) {
            assert_eq!(original.pixels, decoded.pixels);
        }
    }

    #[test]
    fn test_quick_encode_decode_exp_golomb() {
        let frames = create_test_frames(5, 16, 16);
        let encoded = encode(&frames, "exp-golomb", None).unwrap();
        let result = decode(&encoded).unwrap();

        assert_eq!(result.frames.len(), frames.len());
        for (original, decoded) in frames.iter().zip(result.frames.iter()) {
            assert_eq!(original.pixels, decoded.pixels);
        }
    }

    #[test]
    fn test_version() {
        assert_eq!(version(), "1.0.0");
    }
}
