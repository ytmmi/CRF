# NVIDIA CUDA 初步接入

本文记录性能规划 P3/P4 的第一步：在不改变 CRF 码流和默认 CPU 行为的前提下，建立 NVIDIA 能力探针、后端选择和显存预算边界。

## 已实现

- `crf::backend::gpu::capability::probe_nvidia`：通过 `nvidia-smi` 读取设备名、驱动版本、显存和 compute capability。
- `crf::backend::gpu::resolve_backend`：按请求后端、像素阈值、驱动可用性和显存预算选择 CPU 或 NVIDIA CUDA；`Auto/GpuAuto` 安全回退，强制 `NvidiaCuda` 返回 `Unavailable`。
- `crf::backend::gpu::memory`：统一传输模式枚举和 i32 批处理显存估算，避免尺寸计算溢出。
- `NvidiaCudaBackend`：通过公共 `BackendKernel` 暴露窄适配层。当前 CPU-safe 构建明确返回 `Unsupported`，不会伪装成已执行 GPU kernel。

默认构建不引入 CUDA SDK，也不依赖 NVIDIA 驱动；无 NVIDIA 环境仍可安装、启动并使用 CPU 路径。

## 下一步（真正 CUDA kernel 前）

1. 固定 diff/RCT/resample 的 scalar 测试向量和端到端基线。
2. 增加可选 `nvidia-cuda` 构建 feature，在独立 `cuda` 适配层加载 driver/runtime。
3. 先实现批量 SoA diff/RCT，再实现固定 tile DCT；采用异步上传、双缓冲和一次性回读。
4. 对 CPU 结果逐值对拍，记录 upload/kernel/download/同步耗时和显存峰值。
5. 只有至少两个尺寸层达到规划门槛（端到端 1.5×，或明确 CPU 占用/功耗收益）后，才考虑接入默认 `Auto` 策略。

## 验证

```text
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
```

探针失败、显存预算不足或小图输入都必须保留可解释的回退原因；不得在 encoder、decoder 或 Tauri command 中直接调用 CUDA API。
