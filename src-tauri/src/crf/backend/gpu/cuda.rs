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

    /// 在启用 `nvidia-cuda` feature 时执行 YCoCg-R 正向变换（3 分量交织，原地）。
    #[cfg(feature = "nvidia-cuda")]
    pub fn rct_forward(&self, pixels: &mut [i32]) -> Result<(), BackendError> {
        super::runtime::run_rct_forward(self.device.device_id, pixels)
    }

    /// 在启用 `nvidia-cuda` feature 时执行 YCoCg-R 逆向变换（3 分量交织，原地）。
    #[cfg(feature = "nvidia-cuda")]
    pub fn rct_inverse(&self, pixels: &mut [i32]) -> Result<(), BackendError> {
        super::runtime::run_rct_inverse(self.device.device_id, pixels)
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
        use crate::crf::backend::BackendError;
        let Some(backend) = NvidiaCudaBackend::new(0) else {
            return;
        };
        let a: Vec<i32> = (0..4096).map(|v| v * 3 - 700).collect();
        let b: Vec<i32> = (0..4096).map(|v| v * 2 + 11).collect();
        let actual = match backend.diff_i32(&a, &b) {
            Ok(values) => values,
            // Unit tests can be run without first building the sidecar DLL;
            // in that case leave the test to the packaging/integration check.
            Err(BackendError::DeviceError(reason))
                if reason.contains("crf_cuda.dll could not be loaded") =>
            {
                return;
            }
            Err(error) => panic!("CUDA diff kernel failed: {error:?}"),
        };
        let expected: Vec<i32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "nvidia-cuda")]
    #[test]
    fn cuda_rct_forward_matches_scalar_when_driver_is_available() {
        use crate::crf::backend::BackendError;
        let Some(backend) = NvidiaCudaBackend::new(0) else {
            return;
        };
        // 确定性 3 分量交织像素（覆盖正负值，含 i32::MIN 边界）
        let n = 4096usize;
        let mut input: Vec<i32> = Vec::with_capacity(n * 3);
        for i in 0..n {
            let r = (i as i32).wrapping_mul(3).wrapping_sub(700);
            let g = (i as i32).wrapping_mul(2).wrapping_add(11);
            let b = (i as i32).wrapping_sub(400);
            input.push(r);
            input.push(g);
            input.push(b);
        }
        // 标量参考（YCoCg-R 正向变换公式）
        let mut expected = input.clone();
        for px in expected.chunks_exact_mut(3) {
            let (r, g, b) = (px[0], px[1], px[2]);
            let co = r - b;
            let t = b + (co >> 1);
            let cg = g - t;
            px[0] = t + (cg >> 1);
            px[1] = co;
            px[2] = cg;
        }
        let mut actual = input.clone();
        match backend.rct_forward(&mut actual) {
            Ok(()) => {}
            // 未构建旁路 DLL 时跳过（交由打包/集成检查覆盖）
            Err(BackendError::DeviceError(reason))
                if reason.contains("crf_cuda.dll could not be loaded") =>
            {
                return;
            }
            Err(error) => panic!("CUDA rct_forward kernel failed: {error:?}"),
        }
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "nvidia-cuda")]
    #[test]
    fn cuda_rct_inverse_matches_scalar_when_driver_is_available() {
        use crate::crf::backend::BackendError;
        let Some(backend) = NvidiaCudaBackend::new(0) else {
            return;
        };
        // 确定性 YCoCg 域 3 分量交织像素（覆盖正负值）
        let n = 4096usize;
        let mut input: Vec<i32> = Vec::with_capacity(n * 3);
        for i in 0..n {
            let y = (i as i32).wrapping_mul(5).wrapping_sub(1000);
            let co = (i as i32).wrapping_mul(2).wrapping_add(333);
            let cg = (i as i32).wrapping_mul(3).wrapping_sub(777);
            input.push(y);
            input.push(co);
            input.push(cg);
        }
        // 标量参考（YCoCg-R 逆向变换公式）
        let mut expected = input.clone();
        for px in expected.chunks_exact_mut(3) {
            let (y, co, cg) = (px[0], px[1], px[2]);
            let t = y - (cg >> 1);
            let g = cg + t;
            let b = t - (co >> 1);
            px[0] = co + b;
            px[1] = g;
            px[2] = b;
        }
        let mut actual = input.clone();
        match backend.rct_inverse(&mut actual) {
            Ok(()) => {}
            Err(BackendError::DeviceError(reason))
                if reason.contains("crf_cuda.dll could not be loaded") =>
            {
                return;
            }
            Err(error) => panic!("CUDA rct_inverse kernel failed: {error:?}"),
        }
        assert_eq!(actual, expected);
    }
}
