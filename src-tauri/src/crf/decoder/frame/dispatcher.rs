//! 帧分派器 —— frame_type + compression_type 路由
//!
//! 规划文档 §5.3。当前作为 `decoder/mod.rs::decode_frame` 的包装器，
//! P4 逐步拆分为纯分派层（只选择 handler）+ 独立重建层。

use crate::crf::error::CrfResult;
use crate::crf::format::{CrfHeader, ImageData};

/// 分派帧解码
///
/// 当前直接转发到 `decoder/mod.rs::decode_frame`。P4 细化后本函数
/// 将只负责选择 handler，不执行重建。
///
/// # 参数
/// - `data`：帧头 + payload 完整字节
/// - `header`：文件头
pub fn dispatch_frame(data: &[u8], header: &CrfHeader) -> CrfResult<ImageData> {
    // P2 阶段：转发到现有的 decode_frame 实现
    // P4 阶段：拆分为 handler 选择 + 独立重建
    super::super::decode_frame(data, header)
}