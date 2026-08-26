//! transform —— 变换、量化和重建
//!
//! 规划文档 §3.6。包含：
//! - transform.rs：TransformKind/TransformSize
//! - dct4.rs：4×4 lifting DCT
//! - dct8.rs：8×8 lifting DCT
//! - rect.rs：4×8/8×4 等矩形变换
//! - quant.rs：标量/矩阵/定点量化
//! - rdoq.rs：Trellis/RDOQ
//! - reconstruct.rs：逆量化/逆变换/块重建
//!
//! 编码和解码共享的是数学契约与逆变换语义，不是"decoder 调用 encoder 私有模块"。
//! 当前 `encoder/dct_path` 中的公共逆变换已迁移到
//! [`reconstruct`]（P1），解除 decoder → encoder 反向依赖。
//!
//! **迁移状态（P1）**：`reconstruct` 模块已迁入（原 `encoder/dct_path` 的
//! 逆变换函数）。`encoder/dct_path/mod.rs` 保留正变换和量化函数，
//! 逆变换改为 `pub use` 转发到本模块。
//! `encoder/rdoq.rs` 将在 P4 迁移到 `core/transform/rdoq.rs`。

#![allow(dead_code)]

/// reconstruct：逆变换与块重建（原 encoder/dct_path 逆变换部分）
pub mod reconstruct;
