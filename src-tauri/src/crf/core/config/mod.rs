//! config —— 请求、预设、解析和有效配置
//!
//! 规划文档 §3.2。包含：
//! - lossy.rs：精细有损参数 LossyTuning（原 format/quant.rs，P4 迁入）
//! - request.rs：EncodeRequest / DecodeRequest
//! - preset.rs：质量预设与 revision
//! - capability.rs：CPU/GPU 能力
//! - resolve.rs：继承、Auto、冲突与 resolved config
//! - report.rs：warnings/fallback/fingerprint
//!
//! 配置层禁止执行像素循环、分配编码 buffer、读写文件或选择具体 frame type。
//! 它只输出不可变 `ResolvedConfig`（定义在 [`crate::crf::core::contract`]）。
//!
//! **迁移状态（P4）**：`lossy` 模块已迁入（原 format/quant.rs 的 LossyTuning）。
//! `ResolvedConfig` 类型已定义在 `core/contract.rs`。

/// lossy：真有损精细调参（P4 迁入，原 format/quant.rs 的 LossyTuning）
pub mod lossy;
