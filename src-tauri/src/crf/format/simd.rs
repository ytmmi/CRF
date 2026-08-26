//! SIMD 向量化残差计算 —— 已迁移到 `backend::cpu::simd`
//!
//! **迁移说明（P5）**：本文件内容已迁移到 [`crate::crf::backend::cpu::simd`]。
//! 保留为 `pub use` 转发层，维持 `format::simd::*` 旧路径兼容。
//! 新代码请直接使用 `backend::cpu::simd`。

pub use crate::crf::backend::cpu::simd::*;