//! color —— 色彩和采样
//!
//! 规划文档 §3.4。包含：
//! - rct.rs：YCoCg-R 纯数学正逆变换（P4 迁入，原 format/rct.rs）
//! - layout.rs：RGB/plane/interleaved layout
//! - resample.rs：4:4:4/4:2:2/4:2:0 抽样与重建
//! - range.rs：位深、范围、饱和与色彩边界
//!
//! `rct.rs` 不应继续放在既包含 SIMD 又包含公开 `format` 类型的混合文件中。
//! CPU/GPU 实现通过 backend kernel 提供，公共 `color` 只定义参考语义、布局和测试向量。
//!
//! **迁移状态（P4）**：rct 模块已迁入（原 format/rct.rs）。
//! format/rct.rs 保留为 pub use 转发层。SIMD 实现仍在 format/simd.rs，
//! P5 迁移到 backend/cpu。

/// rct：可逆色彩空间变换（P4 迁入，原 format/rct.rs）
pub mod rct;
