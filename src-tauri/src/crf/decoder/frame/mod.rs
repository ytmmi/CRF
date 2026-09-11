//! 帧分派层 —— frame_type 路由、包封装、payload handler 注册
//!
//! 规划文档 §5.3。职责：
//! - [`dispatcher`]：frame_type + compression_type 路由；
//! - [`packet`]：FramePacket / FrameDecodeContext；
//! - 版本化 payload handler 注册（P4 细化）。
//!
//! 每个 frame type 通过独立 handler 实现（位于 `decoder/payload/` 或现有子模块）。
//! handler 只负责 payload token → 变换前/预测前的中间数据；
//! 不负责读取文件头、不负责序列参考、不负责最终导出。
//!
//! **迁移状态（P2）**：定义 `FramePacket` 类型和 `decode_frame` 转发层。
//! 当前 `decoder/mod.rs` 的 `decode_frame` 函数（帧分派 + 重建混合）仍为生产路径，
//! `dispatcher::dispatch_frame` 作为其包装器，P4 逐步拆分为纯分派+独立重建。

pub mod dispatcher;
pub mod packet;
