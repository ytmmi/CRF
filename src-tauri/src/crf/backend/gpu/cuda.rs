//! NVIDIA CUDA 后端适配边界。
//!
//! 当前阶段不链接 CUDA SDK，也不宣称已经执行 GPU kernel；这里只提供可
//! 测试的后端选择和明确失败语义。启用真实 CUDA 时，应在本文件或其子模块
//! 内实现 kernel，并保持公共 `BackendKernel` 契约不变。

use super::capability::{probe_nvidia, NvidiaDeviceInfo};
use crate::crf::backend::{BackendError, BackendKernel, KernelInput, KernelOutput};

const DEFAULT_THRESHOLD_PIXELS: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRequest {
    Auto,
    Cpu,
    GpuAuto,
    NvidiaCuda,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSelection {
    Cpu {
        reason: String,
    },
    NvidiaCuda {
        device: NvidiaDeviceInfo,
    },
    Unavailable {
        request: BackendRequest,
        reason: String,
    },
}

pub fn resolve_backend(
    request: BackendRequest,
    width: u32,
    height: u32,
    memory_budget_bytes: Option<u64>,
) -> BackendSelection {
    if request == BackendRequest::Cpu {
        return BackendSelection::Cpu {
            reason: "requested CPU backend".into(),
        };
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels < DEFAULT_THRESHOLD_PIXELS {
        let reason = format!("input below GPU threshold ({pixels} < {DEFAULT_THRESHOLD_PIXELS})");
        return if request == BackendRequest::NvidiaCuda {
            BackendSelection::Unavailable { request, reason }
        } else {
            BackendSelection::Cpu { reason }
        };
    }
    let device = match probe_nvidia(0) {
        Some(device) => device,
        None => {
            let reason = "NVIDIA driver or nvidia-smi unavailable".to_owned();
            return if request == BackendRequest::NvidiaCuda {
                BackendSelection::Unavailable { request, reason }
            } else {
                BackendSelection::Cpu { reason }
            };
        }
    };
    if let Some(budget) = memory_budget_bytes {
        let estimate = pixels.saturating_mul(4).saturating_mul(3);
        if estimate > budget {
            let reason = format!("estimated GPU memory {estimate} exceeds budget {budget}");
            return if request == BackendRequest::NvidiaCuda {
                BackendSelection::Unavailable { request, reason }
            } else {
                BackendSelection::Cpu { reason }
            };
        }
    }
    BackendSelection::NvidiaCuda { device }
}

#[derive(Debug, Clone)]
pub struct NvidiaCudaBackend {
    pub device: NvidiaDeviceInfo,
}

impl NvidiaCudaBackend {
    pub fn new(device_id: u32) -> Option<Self> {
        Some(Self {
            device: probe_nvidia(device_id)?,
        })
    }

    /// 在启用 `nvidia-cuda` feature 时执行批量 i32 差分 kernel。
    #[cfg(feature = "nvidia-cuda")]
    pub fn diff_i32(&self, a: &[i32], b: &[i32]) -> Result<Vec<i32>, BackendError> {
        if a.len() != b.len() {
            return Err(BackendError::Unsupported(
                "diff inputs have different lengths",
            ));
        }
        super::runtime::run_diff_i32(self.device.device_id, a, b)
    }
}

impl BackendKernel for NvidiaCudaBackend {
    fn execute(&self, _input: &KernelInput) -> Result<KernelOutput, BackendError> {
        Err(BackendError::Unsupported(
            "CUDA kernels are not compiled in this CPU-safe build",
        ))
    }

    fn backend_id(&self) -> &'static str {
        "nvidia-cuda"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_request_never_probes_device() {
        assert!(matches!(
            resolve_backend(BackendRequest::Cpu, 4096, 4096, None),
            BackendSelection::Cpu { .. }
        ));
    }

    #[test]
    fn small_inputs_fall_back_for_auto() {
        assert!(matches!(
            resolve_backend(BackendRequest::Auto, 64, 64, None),
            BackendSelection::Cpu { .. }
        ));
    }

    #[cfg(feature = "nvidia-cuda")]
    #[test]
    fn cuda_diff_matches_scalar_when_driver_is_available() {
        let Some(backend) = NvidiaCudaBackend::new(0) else {
            return;
        };
        let a: Vec<i32> = (0..4096).map(|v| v * 3 - 700).collect();
        let b: Vec<i32> = (0..4096).map(|v| v * 2 + 11).collect();
        let actual = backend.diff_i32(&a, &b).expect("CUDA diff kernel failed");
        let expected: Vec<i32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        assert_eq!(actual, expected);
    }
}
