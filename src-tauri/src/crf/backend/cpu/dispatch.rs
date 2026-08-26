//! BackendKernel 调度器 —— CPU SIMD/scalar 运行时分派
//!
//! 规划文档 §7.5。提供 `BackendKernel` trait 的具体实现，运行时
//! 自动选择 AVX2 或标量回退。每个 kernel 必须有 scalar 实现，
//! SIMD 实现与 scalar 逐位一致。

use crate::crf::backend::{BackendError, BackendKernel, KernelInput, KernelLayout, KernelOutput};

/// 差分 kernel（骨架，P5.b 细化多平面输入时扩展）
pub struct SubKernel;

impl BackendKernel for SubKernel {
    fn execute(&self, _input: &KernelInput) -> Result<KernelOutput, BackendError> {
        // 差分 kernel 需要两个输入平面，当前 KernelInput 只支持单平面
        Err(BackendError::Unsupported("SubKernel 需要两个输入平面"))
    }

    fn backend_id(&self) -> &'static str {
        "cpu.sub"
    }
}

/// RCT 正向变换 kernel（骨架，P5.b 细化参数传递时扩展）
pub struct RctForwardKernel;

impl BackendKernel for RctForwardKernel {
    fn execute(&self, _input: &KernelInput) -> Result<KernelOutput, BackendError> {
        Err(BackendError::Unsupported("RctForwardKernel 需要像素数据参数"))
    }

    fn backend_id(&self) -> &'static str {
        "cpu.rct_forward"
    }
}

/// RCT 逆变换 kernel（骨架，P5.b 细化参数传递时扩展）
pub struct RctInverseKernel;

impl BackendKernel for RctInverseKernel {
    fn execute(&self, _input: &KernelInput) -> Result<KernelOutput, BackendError> {
        Err(BackendError::Unsupported("RctInverseKernel 需要像素数据参数"))
    }

    fn backend_id(&self) -> &'static str {
        "cpu.rct_inverse"
    }
}

/// 软阈值 kernel（骨架，P5.b 细化参数传递时扩展）
pub struct SoftThresholdKernel;

impl BackendKernel for SoftThresholdKernel {
    fn execute(&self, _input: &KernelInput) -> Result<KernelOutput, BackendError> {
        Err(BackendError::Unsupported("SoftThresholdKernel 需要 t 参数"))
    }

    fn backend_id(&self) -> &'static str {
        "cpu.soft_threshold"
    }
}