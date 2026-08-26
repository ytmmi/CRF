//! FramePacket / FrameDecodeContext —— 帧包与解码上下文
//!
//! 规划文档 §7.2。编码器输出 `EncodedFrame { header, payload, reconstructed }`，
//! 容器层只接收前两项；解码器输入 `FramePacket`，输出 `DecodedFrame`。
//! `reconstructed` 不得序列化进隐藏字段，必须明确由 session 保存或释放。

use crate::crf::format::FrameHeader;

/// 帧包：容器层提供给分派层的统一内部帧表示
///
/// 容器层只读取字节和边界，payload 分派层据此选择 handler。
/// `reconstructed` 不由本结构携带（见规划文档 §7.2）。
#[derive(Debug)]
pub struct FramePacket<'a> {
    /// 帧头
    pub header: FrameHeader,
    /// payload 字节（不含帧头）
    pub payload: &'a [u8],
    /// 帧头 + payload 在文件中的字节范围
    pub range: crate::crf::core::contract::ByteRange,
}

impl<'a> FramePacket<'a> {
    /// 创建新的帧包
    pub fn new(header: FrameHeader, payload: &'a [u8], file_offset: usize) -> Self {
        let total_len = crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE + payload.len();
        FramePacket {
            header,
            payload,
            range: crate::crf::core::contract::ByteRange::new(file_offset, total_len),
        }
    }
}