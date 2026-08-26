//! Zigzag 扫描与符号编码 —— 已迁移到 `core::entropy::scan`
//!
//! **迁移说明（P4）**：本文件内容已迁移到 [`crate::crf::core::entropy::scan`]。
//! 保留为 `pub use` 转发层，维持 `format::zigzag::*` 旧路径兼容。
//! 新代码请直接使用 `core::entropy::scan`。

pub use crate::crf::core::entropy::scan::*;