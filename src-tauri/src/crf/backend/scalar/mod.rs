//! scalar 参考实现 —— 唯一正确性基准
//!
//! 所有 CPU SIMD/GPU 实现必须与本模块逐位一致。
//! 性能后端通过 [`crate::crf::backend::BackendKernel`] trait 接入，
//! scalar 实现作为 fallback 和对拍基准始终可用。
//!
//! **P0 状态**：空骨架。P5 阶段接入性能后端时实现具体 kernel。

#![allow(dead_code)]
