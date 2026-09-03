//! transform —— 变换、量化和重建
//!
//! 规划文档 §3.6。包含：
//! - dct4.rs：4×4 lifting DCT
//! - dct8.rs：8×8 lifting DCT
//! - rect.rs：4×8/8×4 等矩形变换
//! - quant.rs：标量/矩阵/定点量化（P4 迁入）
//! - rdoq.rs：Trellis/RDOQ（P4 迁入）
//! - reconstruct.rs：逆量化/逆变换/块重建
//!
//! 编码和解码共享的是数学契约与逆变换语义，不是"decoder 调用 encoder 私有模块"。
//!
//! **迁移状态（P4）**：`dct4`/`dct8`/`rect`（原顶层 `crate::crf::transform`）
//! 与 `reconstruct`（原 `encoder/dct_path` 逆变换部分）已迁入，
//! 消除 core 对旧顶层 transform 的依赖。旧顶层 `transform/` 目录已删除。
//! `quant`（原 `format/quant.rs`）与 `closed_loop`（原 `format/closed_loop.rs`）已迁入。

#![allow(dead_code)]

/// dct4：4×4 lifting DCT（原顶层 transform/dct4.rs）
pub mod dct4;
/// dct8：8×8 lifting DCT（原顶层 transform/dct8.rs）
pub mod dct8;
/// rect：4×8/8×4 等矩形变换（原顶层 transform/rect.rs）
pub mod rect;

/// reconstruct：逆变换与块重建（原 encoder/dct_path 逆变换部分）
pub mod reconstruct;

/// quant：残差死区标量量化（原 format/quant.rs）
pub mod quant;
/// closed_loop：闭环预测 + 死区量化（原 format/closed_loop.rs）
pub mod closed_loop;

// 与旧顶层 transform 一致的公共 re-export（兼容路径）
pub use dct4::{dct4x4_forward, dct4x4_inverse};
pub(crate) use dct4::{dct4x4_forward_into, dct4x4_inverse_into};
#[allow(unused_imports)] // 兼容 API；生产热路径使用无分配的 *_into 版本
pub use dct8::{dct8x8_forward, dct8x8_inverse};
pub(crate) use dct8::{dct8x8_forward_into, dct8x8_inverse_into};
#[allow(unused_imports)] // 兼容 API；生产热路径使用无分配的 *_into 版本
pub use rect::{dct_rect_forward, dct_rect_inverse, is_valid_rect};
pub(crate) use rect::{dct_rect_forward_into, dct_rect_inverse_into};
