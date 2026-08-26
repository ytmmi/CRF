//! FramePacket / FrameDecodeContext —— 帧包与解码上下文
//!
//! 规划文档 §7.2。统一帧包契约定义在 [`crate::crf::core::contract::FramePacket`]，
//! 本模块重新导出供解码侧使用。编码器输出
//! `EncodedFrame { header, payload, reconstructed }`，容器层只接收前两项；
//! 解码器输入 `FramePacket`，输出 `DecodedFrame`。`reconstructed` 不得序列化进
//! 隐藏字段，必须明确由 session 保存或释放。

pub use crate::crf::core::contract::FramePacket;
