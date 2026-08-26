//! 编码 session 层 —— 序列生命周期与帧级调度
//!
//! 规划文档 §4.2。职责：
//! - [`batch`]：批量输入与帧循环（提取自 `sequence.rs` 的辅助函数）；
//! - streaming：streaming push/finish 外壳（P3.b 细化）；
//! - reference：golden/previous/anchor reconstructed refs；
//! - scheduler：帧级并行、阶段依赖、backend 调度。
//!
//! **迁移状态（P3）**：`batch` 模块已提取辅助函数和输出组装逻辑。
//! `encode_sequence` 仍保留在 `sequence.rs` 作为 facade，内部委托到本模块。
//! streaming 复用同一 `FrameEncoder`。
//!
//! 参考：[编解码器分层重构规划](../../../../docs/codec-architecture-refactor-plan.md) §4.2

pub mod batch;
pub mod reference;