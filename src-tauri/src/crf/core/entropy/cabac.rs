//! CABAC range coder 共享语法常量（规划文档 §3.7 cabac.rs）
//!
//! 这些常量定义简化 CABAC（LZMA 式 range coder）的**概率模型边界**——
//! 概率定点精度、更新率、归一化阈值与上下文初始概率。编解码两端必须
//! 严格一致，任何单边改动都会破坏无损往返，因此统一定义在 core 层，
//! 由 `encoder/rle_cabac.rs` 与 `decoder/rle_cabac.rs` 的状态机共同消费。

/// 概率定点精度：P(bit=1) ∈ [0, 4096]
pub const RC_BITS: u32 = 12;
/// 概率更新率 1/32
pub const RC_MOVE: u32 = 5;
/// 归一化阈值：range < RC_TOP 时左移 8 位并输出一个字节
pub const RC_TOP: u32 = 1 << 24;
/// CABAC 上下文初始概率（2048 = 0.5）
pub const INIT_PROB: u16 = 2048;
