//! CPU SIMD/threads 后端 —— AVX2/AVX-512/NEON/Rayon
//!
//! 现有 `format/simd.rs` 的 RCT/差分/软阈值 SIMD 实现已迁移到本模块（P5）。
//! 运行时通过 `is_x86_feature_detected!` 分派，与 scalar 实现逐位一致。
//!
//! **迁移状态（P5）**：simd 模块已迁入（原 format/simd.rs）。
//! dispatch 模块提供 BackendKernel trait 的具体实现。
//! format/simd.rs 保留为 pub use 转发层。

/// dispatch：BackendKernel trait 调度器
pub mod dispatch;
/// simd：CPU SIMD 向量化 kernel（P5 迁入，原 format/simd.rs）
pub mod simd;
/// simd_predict：平面空间预测 SIMD（components==1，planar 子平面/灰度帧）
pub mod simd_predict;
