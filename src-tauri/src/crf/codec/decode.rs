//! 解码 facade —— `DecodeRequest → DecodeResult`
//!
//! 职责（规划文档 §5.1）：
/// - 建立 `DecodeSession`（P2 接入）；
/// - 错误映射；
/// - 取消与结果出口。
///
/// 禁止出现 Golomb、CABAC、DCT 或 RGB 变换数学实现。
///
/// **迁移状态（P0）**：仅定义请求类型。生产路径仍为
/// `crate::crf::decoder::decode_from_bytes` / `decode_from_file`。
/// P2 完成后将在此文件实现 facade 转发。

use crate::crf::format::DecodeResult;

/// 解码请求
#[derive(Debug, Clone)]
pub struct DecodeRequest {
    /// CRF 码流字节（从内存解码）
    pub bytes: Vec<u8>,
}

/// 解码 facade 入口（P0 占位）
///
/// **当前未实现**。生产路径请使用 `crate::crf::decoder::decode_from_bytes`。
/// P2 阶段容器层拆分完成后，本函数将转为正式入口。
#[allow(dead_code)]
pub fn decode_from_bytes(_request: DecodeRequest) -> Result<DecodeResult, super::CodecError> {
    Err(super::CodecError::NotImplemented("codec::decode_from_bytes facade (P2)"))
}

/// 从 reader 解码（P0 占位）
#[allow(dead_code)]
pub fn decode_from_reader<R: std::io::Read + std::io::Seek>(
    _reader: &mut R,
) -> Result<DecodeResult, super::CodecError> {
    Err(super::CodecError::NotImplemented("codec::decode_from_reader facade (P2)"))
}
