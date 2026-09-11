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
use crate::crf::core::domain::DecodeResult;

/// 解码请求
#[derive(Debug, Clone)]
pub struct DecodeRequest {
    /// CRF 码流字节（从内存解码）
    pub bytes: Vec<u8>,
}

/// 解码 facade 入口
///
/// 转发到 `crate::crf::decoder::decode_from_bytes`（迁移期旧入口保持生产）。
/// P2 之后旧入口将反转为转发到本函数。
pub fn decode_from_bytes(request: DecodeRequest) -> Result<DecodeResult, super::CodecError> {
    crate::crf::decoder::decode_from_bytes(&request.bytes).map_err(super::CodecError::from)
}

/// 从 reader 解码：读出全部字节后走内存解码路径。
/// 迁移期实现；P2 容器层完成后将支持真正的流式 bounded reader。
#[allow(dead_code)] // codec facade 预留流式入口，待 P2 接线
pub fn decode_from_reader<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
) -> Result<DecodeResult, super::CodecError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))?;
    decode_from_bytes(DecodeRequest { bytes })
}
