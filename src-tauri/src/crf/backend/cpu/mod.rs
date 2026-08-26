//! CPU SIMD/threads 后端 —— AVX2/AVX-512/NEON/Rayon
//!
//! 现有 `format/simd.rs` 的 RCT/差分/软阈值 SIMD 实现将迁移到本模块。
//! 运行时通过 `is_x86_feature_detected!` 分派，与 scalar 实现逐位一致。
//!
//! **P0 状态**：空骨架。P5 阶段接入性能后端时实现具体 kernel。

#![allow(dead_code)]
