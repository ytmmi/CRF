//! 帧分派器 —— frame_type + compression_type 路由
//!
//! 规划文档 §5.3。P2 状态：`dispatch_frame` 选择并委托重建层
//! （[`super::super::reconstruct::reconstruct_frame`]）。
//! P4 将细化为纯分派层（只选择 handler）+ 独立重建层。

use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::ImageData;
use crate::crf::error::CrfResult;

/// 分派帧重建
///
/// 按 `frame_header.frame_type + compression_type` 路由到重建层。
/// P4 细化后本函数将只负责选择 handler，不执行重建本身。
///
/// # 参数
/// - `data`：帧头 + payload 完整字节
/// - `header`：文件头
pub fn dispatch_frame(data: &[u8], header: &CrfHeader) -> CrfResult<ImageData> {
    // P2 阶段：委托重建层（共享单帧重建契约）
    // P4 阶段：拆分为 handler 选择 + 独立重建
    super::super::reconstruct::reconstruct_frame(data, header)
}
