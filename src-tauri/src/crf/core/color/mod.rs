//! color —— 色彩和采样
//!
//! 规划文档 §3.4。包含：
//! - rct.rs：YCoCg-R 纯数学正逆变换
//! - layout.rs：RGB/plane/interleaved layout
//! - resample.rs：4:4:4/4:2:2/4:2:0 抽样与重建
//! - range.rs：位深、范围、饱和与色彩边界
//!
//! `rct.rs` 不应继续放在既包含 SIMD 又包含公开 `format` 类型的混合文件中。
//! CPU/GPU 实现通过 backend kernel 提供，公共 `color` 只定义参考语义、布局和测试向量。
//!
//! **P0 状态**：空骨架。现有 `format/rct.rs` 的 `rct_forward`/`rct_inverse`/
//! `rct_applicable` 与 `format/simd.rs` 的 RCT SIMD 实现将在 P4 迁移到本模块
//! 与 `backend/cpu`。

#![allow(dead_code)]

// P4 阶段迁入子模块（当前为空声明）
