//! GPU 后端 —— CUDA/HIP/Vulkan/wgpu（可选 feature）
//!
//! GPU 只能通过 [`crate::crf::backend::BackendKernel`] trait 进入，
//! 不能修改 sequence/decoder 容器层（规划文档 §2 依赖方向）。
//! 新增 GPU 后端必须独立文件/feature，不得把厂商 API 写入公共层。
//!
//! **P0 状态**：空骨架。P5 阶段按性能优化规划接入。

#![allow(dead_code)]
