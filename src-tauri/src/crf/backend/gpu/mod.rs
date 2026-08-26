//! GPU 后端入口。
//!
//! NVIDIA 的首期实现只负责能力发现、后端决策和失败回退。CUDA driver
//! API/kernel 尚未进入默认构建，因此 CPU-only 构建不需要 CUDA SDK。后续
//! kernel 必须通过 [`crate::crf::backend::BackendKernel`] 接入，不能把厂商
//! API 泄漏到 encoder/decoder。
#![allow(dead_code)]

pub mod capability;
pub mod cuda;
pub mod memory;
#[cfg(feature = "nvidia-cuda")]
mod runtime;

pub use capability::{probe_nvidia, NvidiaDeviceInfo};
pub use cuda::{resolve_backend, BackendRequest, BackendSelection, NvidiaCudaBackend};
pub use memory::{estimate_i32_batch, GpuMemoryEstimate, TransferMode};
