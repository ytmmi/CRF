//! CRF 格式处理模块
//!
//! 本模块实现了 CRF (Compressing Residual Frames) 格式的编解码功能。
//! CRF 是一种轻量级的无损压缩格式，专为差分图片序列设计。

pub mod checksum;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod format;
pub mod transform;

// 重新导出常用类型
// （部分项在非 test 编译单元中无直接调用者，属公共 API 面，供集成与测试路径使用）
#[allow(unused_imports)]
pub use decoder::{decode_from_bytes, decode_from_file};
#[allow(unused_imports)]
pub use encoder::{encode_frame, encode_sequence};
pub use error::CrfResult;
#[allow(unused_imports)]
pub use format::{
    apply_prediction, ColorFormat, DecodeResult, EncodeParams, ImageData, LossyTuning,
    PredictionMode,
};

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
/// 将图像序列编码为 CRF 格式
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
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: false,
        user_metadata: user_metadata.map(|s| s.to_string()),
    };
    encode_sequence(frames, &params)
}

/// 快捷解码函数
///
/// 将 CRF 数据解码为图像序列
#[allow(dead_code)] // 公共快捷入口，仅测试路径调用
pub fn decode(data: &[u8]) -> CrfResult<DecodeResult> {
    decode_from_bytes(data)
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
