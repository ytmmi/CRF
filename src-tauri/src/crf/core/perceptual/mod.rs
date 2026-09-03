//! perceptual —— 噪声感知估计与软阈值（规划文档 §3 工具层）
//!
//! - noise.rs：JPEG/WebP 有损源差分残差的噪声归一化——零中心性门控、
//!   逐条带自适应量化步长、软阈值滤波。纯 encoder 侧算法，无 decoder 对称端；
//!   解码端零感知，格式零改动。
//!
//! **迁移状态（P4）**：`noise` 模块已从 `encoder/noise.rs` 迁入（原
//! `encoder/noise.rs` 不再保留）。

#![allow(dead_code)]

/// noise：噪声感知估计与软阈值（P4 迁入，原 encoder/noise.rs）
pub mod noise;
