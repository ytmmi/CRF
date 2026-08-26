//! config —— 请求、预设、解析和有效配置
//!
//! 规划文档 §3.2。包含：
//! - request.rs：EncodeRequest / DecodeRequest
//! - preset.rs：质量预设与 revision
//! - lossy.rs：精细有损参数
//! - capability.rs：CPU/GPU 能力
//! - resolve.rs：继承、Auto、冲突与 resolved config
//! - report.rs：warnings/fallback/fingerprint
//!
//! 配置层禁止执行像素循环、分配编码 buffer、读写文件或选择具体 frame type。
//! 它只输出不可变 `ResolvedConfig`（定义在 [`crate::crf::core::contract`]）。
//!
//! **P0 状态**：空骨架。`ResolvedConfig` 类型已定义在 `core/contract.rs`。
//! 现有 `format/types.rs` 的 `EncodeParams` 与 `format/quant.rs` 的 `LossyTuning`
//! 将在 P3 迁移到本模块。

#![allow(dead_code)]

// P3 阶段迁入子模块（当前为空声明）
