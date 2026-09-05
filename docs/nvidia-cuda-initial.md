# NVIDIA CUDA 初步接入

本文记录性能规划 P3/P4 的第一步：在不改变 CRF 码流语义的前提下，建立 NVIDIA 能力探针、后端选择和显存预算边界，并接入首个可执行 CUDA diff kernel。默认构建优先尝试 GPU，任何不可用或异常情况都会自动回退 CPU。

## 已实现

- `crf::backend::gpu::capability::probe_nvidia`：通过 `nvidia-smi` 读取设备名、驱动版本、显存和 compute capability。
- `crf::backend::gpu::resolve_backend`：按请求后端、像素阈值、驱动可用性和显存预算选择 CPU 或 NVIDIA CUDA；`Auto/GpuAuto` 安全回退，强制 `NvidiaCuda` 返回 `Unavailable`。
- `crf::backend::gpu::memory`：统一传输模式枚举和 i32 批处理显存估算，避免尺寸计算溢出。
- `NvidiaCudaBackend`：通过公共 `BackendKernel` 暴露窄适配层；启用 feature 后提供 `diff_i32` 与 `rct_forward`，通过旁路 `crf_cuda.dll` 调用 CUDA Driver API 并执行 PTX（diff + YCoCg-R 正变换两个 kernel，共享同一 module/context）。
- CUDA 运行时会缓存 Driver、context、module（kernel function 按名惰性获取）；每次调用仅分配/释放输入输出缓冲区，降低重复初始化开销，并在会话销毁时安全释放 CUDA 资源。

默认 feature 为 `nvidia-cuda`，统一 `backend::ops::sub_i32` 与 `backend::ops::rct_forward` 会在首次达到 1M 元素（rct 为 3 分量交织，即约 349K 像素）时检查设备并优先使用 GPU；小输入、无 NVIDIA 环境、驱动异常或 kernel 失败会自动切换到 CPU。GPU 失败状态会被记忆，避免每次调用重复探测。

使用 `--no-default-features` 可生成纯 CPU 构建；两种构建都不改变 CRF 码流语义。

正式分发产物由 `crf-viewer.exe` 与同目录的 `crf_cuda.dll` 组成；该 DLL 只依赖 NVIDIA 驱动提供的 `nvcuda.dll`，不依赖 CUDA Toolkit、`nvcc` 或 `cudart.dll`。Toolkit（例如 `D:\BianCen\cuda\cuda133`）仅用于本地开发和可选的工具链检查。

## 启用 NVIDIA 构建

Toolkit 可位于 `D:\BianCen\cuda\cuda133`，无需加入全局 PATH。当前 Driver API 路径不要求 `nvcc` 参与 Rust 编译：

```powershell
$env:CUDA_PATH = 'D:\BianCen\cuda\cuda133'
cargo check --manifest-path src-tauri/Cargo.toml --workspace --features nvidia-cuda
cargo build --manifest-path src-tauri/Cargo.toml --workspace --release --features nvidia-cuda
$env:CRF_CUDA_DLL = (Resolve-Path src-tauri/target/release/crf_cuda.dll)
cargo test --manifest-path src-tauri/Cargo.toml --features nvidia-cuda crf::backend::gpu::cuda::tests::cuda_diff_matches_scalar_when_driver_is_available -- --nocapture
```

该测试在可用 NVIDIA 设备上执行 4096 个 i32 差分并与标量结果逐值对拍；无设备时安全跳过。

## 下一步（真正 CUDA kernel 前）

1. 固定 diff/RCT/resample 的 scalar 测试向量和端到端基线。
2. RCT 正变换已接入默认编码路径（`backend::ops::rct_forward` → `crf_cuda_rct_forward`）；下一步将 diff/RCT 扩展为批量 SoA 布局，再实现固定 tile DCT；采用异步上传、双缓冲和一次性回读。
3. 对 CPU 结果逐值对拍，记录 upload/kernel/download/同步耗时和显存峰值。
4. 只有至少两个尺寸层达到规划门槛（端到端 1.5×，或明确 CPU 占用/功耗收益）后，才扩大 GPU 覆盖范围。

## 验证

```text
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
cargo build --manifest-path src-tauri/Cargo.toml --workspace --release --features nvidia-cuda
```

从 workspace 构建会同时生成两个文件：

```text
src-tauri/target/release/crf-viewer.exe
src-tauri/target/release/crf_cuda.dll
```

发布时请保持二者位于同一目录；也可通过 `CRF_CUDA_DLL` 指定 DLL 的绝对路径进行诊断。

探针失败、显存预算不足或小图输入都必须保留可解释的回退原因；不得在 encoder、decoder 或 Tauri command 中直接调用 CUDA API。
