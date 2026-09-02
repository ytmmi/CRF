# CRF 性能优化规划：CPU 与 GPU 加速

**规划日期**：2026-08-26  
**适用范围**：CRF 编码、解码、参数搜索、批量 API、streaming API 和基准工具  
**状态**：性能探索规划；本轮不修改算法代码、不引入依赖、不执行构建  
**强制标准**：[CRF 项目开发、算法与构建标准](project-standards.md)

性能后端必须在编解码器分层完成后接入；公共 scheduler/kernel 契约和 encoder/decoder
依赖边界以[《CRF 编码器/解码器分层架构重构规划》](codec-architecture-refactor-plan.md)
为前置条件。

截至 2026-08-26 的源码规模检查发现 `src-tauri/src/test/mod.rs` 约 1030 行，已违反
1000 行硬限制；`src-tauri/src/crf/encoder/tests.rs` 约 980 行、
`src-tauri/src/crf/format/prediction.rs` 约 801 行进入预警区。性能实现不得绕过这项
准入；上述文件必须先按职责拆分，才能进入后续性能构建轮次。

## 1. 目标与边界

### 1.1 性能目标

CRF 性能优化的第一目标是降低**端到端用户可感知耗时**，而不是单独提高某个 kernel
的理论吞吐。端到端时间至少拆成：

```text
输入解码/预处理
→ 首帧编码与重建
→ 差分/参考生成
→ 预测/变换/量化/RDO
→ 熵编码
→ 帧组装、索引、CRC 和文件写入
```

同时报告：

- 首帧延迟、单帧延迟、完整序列延迟；
- p50/p95 延迟和像素吞吐（MPix/s）；
- CPU 时间、GPU kernel 时间、主机—设备传输时间；
- 峰值内存、显存、线程数、设备利用率和功耗（可获得时）；
- 编码速度与解码速度分别统计；
- 质量、码率、码流确定性和错误率不能因加速退化。

### 1.2 非目标

- 不为了速度破坏无损逐像素正确性；
- 不把有损和无损合并成一条难以维护的“快速路径”；
- 不在没有端到端收益时增加 GPU 依赖或复杂硬件抽象；
- 不直接复制完整 AV1/HEVC 的硬件视频编码器架构；
- 不以启发式剪枝替代有损 RDO，除非明确标记 effort 并通过质量护栏；
- 不把 GPU kernel、驱动、Tauri IPC 和文件 I/O 混进一个源文件。

### 1.3 硬约束

本规划服从项目标准：

- 所有手写源文件不超过 1000 行，800 行进入拆分预警；
- GPU 后端是可选模块，CPU 标量/ SIMD 路径永远保留；
- 编解码核心不能依赖 Tauri、UI 或 GPU 驱动才能工作；
- 同一 resolved config 的 deterministic 模式必须产生相同码流；
- GPU 不支持、驱动错误、显存不足或数据规模太小时必须安全回退或明确失败；
- 优化前后必须有基线、剖析、消融、回归和停止门槛。

## 2. 当前基线与热点假设

### 2.1 已有 CPU 能力

当前代码已经具备：

- `rayon` 帧级、条带级和候选模式并行；
- `format/simd.rs` 的 x86_64 AVX2 运行时分派，覆盖差分、部分 YCoCg-R 和软阈值；
- 标量回退和 SIMD 逐值/逐位对拍测试；
- streaming API，目标是 O(golden + 单帧 + 码流) 内存；
- CRC32 使用 `crc32fast`，可利用平台硬件指令；
- release LTO；明确不使用 `target-cpu=native` 生成不可移植产物。

### 2.2 未解决的 CPU 热点

优先使用 profiler 验证，不预先假设所有热点都适合 SIMD：

1. 首帧多候选的预测 SAD/SATD、局部预测和 RDO；
2. RCT 交织布局的加载、拆包和写回；
3. DCT/矩形变换、量化矩阵和 RDOQ/Trellis；
4. `Med/Paeth` 等带分支的空间预测；
5. RLE/Golomb/CABAC 的分支、位写入和上下文更新；
6. 每帧/每条带临时 `Vec` 分配、复制和缓存失配；
7. 首帧重建后大尺寸 golden 差分的内存带宽；
8. 批量路径与 streaming 路径重复解析配置或重复中间缓冲；
9. 输出 CRC、索引和文件 I/O 在小帧场景中的固定开销。

### 2.3 GPU 适合与不适合的工作

**优先适合 GPU**：

- 大批量、规则、整数或定点的逐像素差分、RCT、色度下采样/上采样；
- 固定尺寸 tile 的预测候选、SAD/SATD、梯度和 activity 统计；
- 4×4/8×8/矩形 DCT、量化矩阵和局部重建；
- 大量候选的并行 RDO 代价预计算；
- 可固定访问模式的 significance/非零统计。

**首期不宜 GPU 化**：

- 强分支、短数据、上下文依赖的 CABAC/RLE/Golomb 主熵编码；
- 文件头、索引、CRC 和磁盘写入；
- 需要频繁回读 CPU 决策的细粒度模式切换；
- 只有几万像素或一两帧的任务；
- 不同 GPU 上无法保证确定性或边界一致性的浮点算法。

首期推荐混合路径：GPU 执行规则的像素/块候选计算，CPU 执行最终模式裁决、系数符号化、
码流组装和 I/O；只有在传输与同步成本可控时，才把更多 RDO 阶段移入 GPU。

## 3. 总体后端架构

### 3.1 三层结构

```text
codec core（与硬件无关）
  ├─ scalar reference
  ├─ CPU backend（SIMD + threads）
  └─ GPU backend（CUDA / HIP / Vulkan 等）
         ↓
   backend scheduler
         ↓
   resolved performance config + capability report
```

核心算法只依赖抽象操作和明确布局，不直接调用 `cuda`, `hip`, `vulkan` 或具体 SIMD
intrinsic。设备后端实现同一组小接口，例如：

```text
pixel_diff
rct_forward / rct_inverse
chroma_resample
predict_costs
transform_quantize
reconstruct_tile
```

每个操作必须有标量参考实现、CPU 实现和可选 GPU 实现；后端负责资源生命周期和错误，
核心序列编排负责阶段顺序，不负责设备 API 细节。

### 3.2 建议模块边界

```text
performance/
  config.rs          # backend/effort/threshold/deterministic 参数
  capability.rs      # CPU/GPU 能力发现与版本
  scheduler.rs       # 任务切分、批量、回退和同步
  telemetry.rs       # 计时、计数、profiling 事件

backend/cpu/
  scalar.rs          # 参考实现
  simd_x86.rs        # SSE2/AVX2/可选 AVX-512
  simd_arm.rs        # NEON
  kernels.rs         # CPU kernel 统一入口

backend/gpu/
  trait.rs           # 与设备无关的能力/队列/缓冲抽象
  memory.rs          # host/device buffer 与传输策略
  cuda.rs            # NVIDIA 后端适配
  hip.rs             # AMD/ROCm 后端适配
  vulkan.rs          # 跨厂商 compute 后端适配
  kernels/           # 按职责拆分的 GPU kernel 源文件
```

实际目录名可以调整，但禁止把 CPU、CUDA、HIP、Vulkan 和调度器集中到一个“万能 backend”
文件。任何文件达到 800 行，优先按职责拆分；达到 1000 行禁止继续构建。

### 3.3 后端选择

建议解析顺序：

```text
用户请求 backend
→ capability discovery
→ 输入尺寸/帧数/内存预算估算
→ 传输成本模型
→ 选择 CPU / GPU / Hybrid
→ resolved backend report
```

建议公开枚举：

```text
Auto
Cpu
GpuAuto
NvidiaCuda
AmdHip
VulkanCompute
```

`Auto` 不得仅根据“检测到显卡”就启用 GPU，必须考虑数据规模、驱动、显存、传输和历史
基准。GPU 后端不可用时：

- `Auto/GpuAuto`：记录告警后回退 CPU；
- 用户强制指定的后端：默认返回结构化错误，可由 `allow_fallback=true` 明确允许回退；
- deterministic 模式：回退不能静默改变 resolved config，必须记录实际后端。

## 4. CPU 加速分支

### 4.1 CPU 优化顺序

1. 建立阶段级 profiler 和可复现基准；
2. 消除重复分配、重复 RCT、重复参数解析和不必要的内存复制；
3. 统一线程调度，避免 Rayon 嵌套造成过度订阅；
4. 扩展逐元素 SIMD；
5. 优化 tile/plane 内存布局和缓存局部性；
6. 对 DCT、量化、RDOQ、预测代价做专门 kernel；
7. 最后评估熵编码和 I/O 的并行/批处理。

每一步都要先有 profile 数据；不能因为某个操作容易向量化就优先实现。

### 4.2 SIMD 指令集路线

#### x86_64 基线

- SSE2 作为 x86_64 最低可用假设，仅用于轻量回退或确认；
- AVX2 作为当前主优化路径，覆盖差分、平面 RCT、量化前处理、统计和简单预测；
- AVX-512 作为可选高端路径，只在寄存器宽度收益超过频率下降和功耗成本时启用；
- 运行时检测，不把 `target-cpu=native` 写入分发构建；
- 每个 intrinsic kernel 必须有同输入标量对拍、边界长度测试和不支持指令集回退。

#### ARM

- AArch64 NEON 作为独立实现，不复制 x86 intrinsic 代码；
- Apple Silicon、Linux ARM 和 Windows ARM 的检测与 feature flag 分开处理；
- 若编译器无法提供稳定运行时分派，则使用编译目标能力加安全回退；
- 不因 ARM 支持引入改变数学舍入的浮点路径。

#### SIMD 优先级

优先级从高到低：

1. SoA 平面差分、RCT 和色度重采样；
2. 绝对值、阈值、量化查表和 activity 统计；
3. 固定 4×4/8×8 DCT 与矩阵乘法；
4. H/V/DC/平均预测和 SAD；
5. MED/Paeth 等分支预测；
6. CABAC/RLE：只有 profile 证明收益稳定后再做批量符号优化。

交织 RGB/YCoCg 数据会限制 SIMD 负载效率。允许在 tile 内采用短生命周期 SoA 布局，
但必须以总复制成本、缓存收益和批量/streaming 内存上界共同决定，不能盲目全局改布局。

### 4.3 CPU 线程与任务调度

- 统一由 scheduler 创建和配置线程池；禁止 sequence、adaptive、banded 各自再创建线程池；
- 禁止 Rayon 嵌套并行，除非有明确的线程预算和基准证明；
- 任务粒度优先为帧、tile、条带，避免对短行或单个系数启动任务；
- 首帧阶段允许候选并行，但后续块的空间预测仍遵守重建因果顺序；
- streaming 保持帧间内存上界，不以无限队列换吞吐；
- deterministic 模式固定候选遍历顺序、归约顺序和 tie-break；
- NUMA/大核小核调度作为后续实验，不在核心算法中读取平台私有拓扑。

### 4.4 CPU 内存与缓存

优先调查：

- 复用 frame/tile scratch buffer，减少每候选分配；
- 将只读参考、预测结果和重建结果分离，避免写放大；
- 对 DCT/量化使用连续 tile、对齐访问和小型栈/线程本地缓冲；
- 降低 RGB 交织与 RCT 之间的往返复制；
- 对大图采用分块流水，避免同时驻留所有候选；
- 预留内存上界并在超过预算前调整 tile/候选数或回退；
- 测量 allocator、memcpy、cache miss 和 page fault，不凭经验引入对象池。

任何 buffer pool 必须放在独立 memory 模块；不得把生命周期管理塞进算法文件。

### 4.5 CPU 熵编码与 I/O

- RLE/Golomb/exp-Golomb 的批量位写入优先减少函数调用和分支；
- CABAC 上下文更新保持确定性，先优化内存访问和批量接口，再考虑表驱动/分支预测；
- 不把上下文自适应熵编码粗暴搬到 GPU；
- 索引、CRC 和 footer 可以在 CPU 上合并写入，避免小块 I/O；
- 大文件使用缓冲写和顺序布局；不得为了并行写入破坏索引和 deterministic 顺序。

### 4.6 CPU 验收门槛

单项 CPU 优化必须至少满足：

- 目标场景端到端加速 ≥10%，或热点阶段加速 ≥25% 且不增加总内存超过 10%；
- 无损逐像素一致；deterministic 码流逐字节一致（若声明码流兼容）；
- SIMD/标量随机及边界对拍通过；
- 有损 PSNR/SSIM、最差帧、frame0 和完整字节无系统性退化；
- 低端/不支持指令集设备有安全回退；
- 速度收益在至少三个内容层和两个尺寸层保持，不能只在微型合成数据上成立。

若只改善 microbenchmark 而端到端低于 3%，停止正式接入，保留探针或删除。

## 5. GPU 总体路线

### 5.1 GPU 采用条件

GPU 不是默认更快。只有满足以下条件才进入 GPU 实验：

- 输入至少达到预设像素阈值，且帧/tile 数量足以隐藏启动开销；
- kernel 可批量处理，CPU 与 GPU 之间不会每个块往返；
- 设备驱动和编译工具链在目标平台可复现；
- 传输、同步、临时显存和设备初始化成本已计入端到端基准；
- CPU fallback 保持完整功能；
- GPU 结果能满足 deterministic 或明确标记为非确定模式，并通过质量护栏。

建议初始阈值不写死在格式中，放在版本化性能 profile：像素数、帧数、tile 数、显存预算
和历史 kernel/传输速度共同决定是否启用。阈值必须由基准校准，而不是凭显卡型号猜测。

### 5.2 GPU 流水线

首期采用混合流水：

```text
CPU 输入解码/预处理
  → pinned/staging upload（按帧或 tile 批量）
  → GPU diff/RCT/resample/predict-cost/transform
  → 一次性回读候选代价或重建 tile
  → CPU RDO 裁决 + entropy + frame assembly
  → CPU index/CRC/file I/O
```

大图可使用双缓冲或三缓冲，使当前 GPU kernel、下一批上传和上一批 CPU 熵编码重叠；
但必须避免同一重建参考尚未完成就启动依赖 kernel。GPU stream/queue 数由 scheduler 统一管理。

### 5.3 GPU 内存策略

- 传输单位优先为连续 frame/tile，不允许每个像素或系数单独传输；
- 使用 pinned host buffer、device buffer pool 和明确的 ownership；
- 计算中尽量保持数据驻留 GPU，直到一组候选完成；
- 显存不足时按 tile 分块或回退 CPU，不允许静默 OOM；
- 记录 host↔device bytes、传输次数、同步次数和峰值显存；
- 对零拷贝、统一内存和 mapped memory 必须实测，不能假设它们总比显式传输快；
- GPU 资源释放、驱动上下文和线程绑定放在 backend/memory 模块，禁止散落在编码器。

### 5.4 GPU 数值与确定性

- 优先使用整数或固定点 kernel，保持与标量参考相同的舍入、溢出和边界语义；
- 浮点仅用于明确允许的估计/排序，不能改变无损结果；
- 并行归约固定树形或使用确定性归约，禁止未定义顺序的原子最小值决定模式；
- 候选相同代价时使用固定 tie-break（模式、变换、坐标顺序）；
- GPU deterministic 模式必须与 CPU deterministic 结果逐项对拍；
- 若某后端只能提供非确定模式，必须明确能力位、质量统计和用户选择，不得冒充确定模式。

## 6. NVIDIA GPU 分支

### 6.1 首选后端：CUDA

NVIDIA 路线以 CUDA 为主要实验后端，适合：

- 大规模逐像素 diff/RCT/色度处理；
- 固定 tile 预测代价和 DCT/量化批处理；
- 多候选并行 RDO 预筛；
- CUDA streams、异步 memcpy 和 pinned memory 流水。

规划要求：

- CUDA 代码与 Rust 核心通过窄适配层连接，禁止在 `sequence.rs` 直接写 driver API；
- 可采用运行时加载或可选构建 feature，CPU-only 构建不得要求 CUDA SDK；
- 能力发现至少记录 compute capability、显存、驱动版本、kernel 版本和编译选项；
- 不假设 Tensor Core 或半精度适用于整数无损路径；除非质量语义允许，否则使用整数/定点；
- NVIDIA 专有优化不能改变公共 backend trait 的语义。

### 6.2 NVIDIA kernel 优先级

1. coalesced 的 SoA diff/RCT/resample；
2. tile 内 H/V/DC/梯度预测代价；
3. 4×4/8×8 DCT 与量化；
4. 候选代价矩阵和 activity 统计；
5. 重建 tile 和首帧参考准备；
6. 只有在符号批处理结构稳定后才探测 GPU coefficient token 化；
7. CABAC 主循环、文件组装和随机小帧不列为首期 CUDA 目标。

### 6.3 NVIDIA 版本与分发

- 明确最低驱动、最低 compute capability 和支持的操作系统；
- PTX/CUBIN/运行时编译策略必须记录在构建文档，避免只在开发机可运行；
- 不把 CUDA runtime 大体积、许可证和安装要求隐藏在普通 CPU 构建中；
- GPU 产物必须能在无 NVIDIA 环境下安装、启动并回退 CPU；
- 设备丢失、上下文错误和 kernel 超时必须转为结构化后端错误并释放资源。

### 6.4 NVIDIA 验收

除通用 GPU 门槛外，至少测试：

- 同一设备冷启动/热启动、单流/多流；
- 不同显存大小和 PCIe 代际；
- 低于阈值的小图是否正确回退；
- CUDA 与 CPU 的重建逐值对拍；
- 显存不足、驱动不可用、设备拔出/重置的错误路径；
- 在至少一个 NVIDIA 中端和一个高端设备上，端到端加速 ≥1.5× 或明确记录不适合场景。

## 7. AMD GPU 分支

### 7.1 首选后端：HIP/ROCm

AMD 路线以 HIP/ROCm 为高性能实验后端，主要面向 ROCm 支持稳定的平台。规划重点：

- HIP kernel 与 CUDA kernel 尽量共享算法结构和测试向量，不共享含厂商 API 的文件；
- Linux/ROCm 先行，Windows HIP SDK 的支持范围单独验证；
- 记录 GPU 架构（gfx）、ROCm/HIP 版本、驱动、显存和编译工具链；
- 不把 ROCm 依赖强制带入 CPU-only 或 NVIDIA-only 构建；
- HIP 不可用时只能回退到 CPU 或跨厂商 compute 后端，必须报告实际后端。

### 7.2 跨厂商后端：Vulkan Compute / wgpu

为覆盖 AMD Windows、Linux 以及其他厂商，可规划 Vulkan Compute 或基于 wgpu 的后端：

- 只承诺公共整数/定点 kernel 子集，避免以最低共同能力假设所有高级扩展；
- 设备、队列、descriptor、pipeline 和 buffer 生命周期封装在独立模块；
- shader 版本、SPIR-V/wgpu 编译和缓存策略必须可复现；
- 先用于 diff/RCT/resample/DCT 等规则 kernel，再考虑复杂 RDO；
- Vulkan/wgpu 的性能可能低于原生 HIP/CUDA，必须按设备 profile 选择，不能强制替代；
- 驱动差异、扩展缺失和设备丢失都必须有 CPU 回退。

Vulkan/wgpu 是兼容性路线，不等同于 AMD 专属高性能路线。AMD 高端 Linux 设备优先比较
HIP，Windows 或无 ROCm 环境再比较 Vulkan/wgpu。

### 7.3 AMD 验收

至少覆盖：

- 一个 ROCm 支持的 AMD 平台和一个 Windows Vulkan/wgpu 平台（若发布范围包含）；
- HIP 与 CPU 逐值对拍，Vulkan/wgpu 与 CPU 逐值对拍；
- shader 编译失败、扩展缺失、显存不足、设备丢失和驱动升级后的回退；
- 端到端 ≥1.5× 加速或证明该设备/尺寸应选择 CPU；
- 不把 HIP 的性能特性误写为所有 AMD/所有操作系统都具备。

### 7.4 其他 GPU 后端（后续评估）

若项目发布范围需要覆盖其他设备，按以下顺序评估，不与 NVIDIA/AMD 首期路线同时铺开：

- **Intel**：优先评估 Vulkan Compute；只有目标设备和发行版明确需要时再考虑 oneAPI/SYCL，
  不为单一实验环境引入完整 oneAPI 运行时；
- **Apple**：macOS/iOS 可评估 Metal Compute，但必须单独封装，不能把 Metal API 混入
  Vulkan/wgpu 文件；Apple 路线不改变 Windows/Linux 的公共后端契约；
- **OpenCL**：只作为旧设备兼容探针，不作为首选生产后端，除非实测维护成本和端到端
  收益都优于 Vulkan/wgpu；
- **DirectX 12 Compute**：仅在 Windows 发布需求明确且 Vulkan/wgpu 不达标时评估，
  先证明设备覆盖和 SDK 维护成本合理。

所有新增厂商后端都必须复用统一 kernel 契约、能力报告、CPU 回退和逐值对拍；“支持某个
API”不等于“在所有该厂商显卡上承诺同一性能”。

## 8. 跨厂商与后端统一标准

### 8.1 能力矩阵

运行时返回 capability report：

```text
backend: cpu | cuda | hip | vulkan | wgpu
device_name / vendor / architecture
driver_and_runtime_version
simd_or_gpu_feature_set
max_buffer / available_memory
supported_kernels
deterministic_support
recommended_thresholds
fallback_reason
```

resolved config 必须记录“请求后端”和“实际后端”。用户不能只看到 `Auto` 而不知道最终
使用了 CPU、NVIDIA CUDA、AMD HIP 还是 Vulkan。

### 8.2 统一 kernel 契约

每个可加速操作必须定义：

- 输入布局、输出布局、stride、尺寸和边界；
- 数值范围、舍入、溢出和 alpha/色度语义；
- 同步点和是否允许异步；
- 标量参考测试向量；
- CPU SIMD 与各 GPU 后端的逐值容差（无损必须为零）；
- 失败和回退行为；
- benchmark 场景和尺寸阈值。

不满足契约的 kernel 不得接入生产 scheduler，只能作为隔离探针。

### 8.3 混合调度策略

推荐三种模式：

| 模式 | 适用 | 行为 |
|---|---|---|
| `CpuOnly` | 小图、无 GPU、调试、兼容 | 全部 CPU，使用标量/SIMD/线程调度 |
| `Hybrid` | 首期默认 GPU 路线 | GPU 规则 kernel，CPU RDO/熵编码/I/O |
| `GpuHeavy` | 大图、批量 tile、后期验证 | GPU 扩大到候选 RDO，减少回读 |

`GpuHeavy` 必须保留 `CpuOnly` 等价路径；模式不是格式语义，不能改变解码器要求。

## 9. 基准与验收矩阵

### 9.1 数据集

必须同时覆盖：

- 2 帧、14 帧和长序列 streaming；
- 小图、中图、大图和超大分辨率；
- 灰度、RGB、不同位深、不同色度格式；
- 干净 PNG、赛璐璐、渐变、照片、JPEG/WebP 源、透明和高饱和边缘；
- 首帧复杂、差分稀疏、差分密集、全帧变化和低色数图；
- CPU 低端/中端/高端，NVIDIA 中端/高端，AMD ROCm 和 Vulkan/wgpu 平台。

### 9.2 测量规则

- 首次运行与稳定运行分开；记录 GPU 初始化、shader/JIT 编译是否计入；
- 至少重复 5 次，报告中位数、P95 和离散度；
- 分开报告 upload、kernel、download、CPU 等待和端到端；
- 关闭/固定后台并行，记录线程数、设备电源/时钟（可获得时）；
- 大小、质量、码流确定性和错误率与速度一起记录；
- GPU 结果必须与 CPU 参考按帧、按 tile、按系数比较，不能只比较最终文件大小。

### 9.3 接入门槛

#### CPU 单项

- 端到端 ≥10%，或目标热点 ≥25% 且总体内存不增加超过 10%；
- 三个内容层、两个尺寸层和至少一个长序列成立；
- 无损逐像素、deterministic 码流、批量/streaming 和旧文件回归通过。

#### GPU 单项

- 大图/批量端到端 ≥1.5×，或明确证明降低 CPU 占用/功耗且用户有价值；
- 传输和初始化计入，不用 kernel-only 数字冒充端到端收益；
- 小图自动选择 CPU，不得因 GPU 路径变慢；
- 至少两个设备/后端或一条明确的设备限制说明；
- 设备失败、显存不足和驱动缺失安全回退；
- 无损逐值一致；有损通过质量护栏和主观视觉检查。

#### 停止门槛

- 只在单一 GPU、单一尺寸或合成数据上获益：停止正式接入；
- 端到端收益低于 10%（CPU）或 1.2×（GPU），且无稳定功耗/交互收益：保留探针或删除；
- 需要把核心模块和 GPU API 混合才能取得收益：先重构边界，不接入；
- 后端维护成本超过收益且无目标平台需求：不发布该后端。

## 10. 参数与可维护性

精细性能接口建议接入 `lossy-tuning-interface-plan.md` 的性能字段：

```text
performance.backend = auto|cpu|gpu-auto|nvidia-cuda|amd-hip|vulkan-compute
performance.effort
performance.threads
performance.deterministic
performance.memory_limit_mb
performance.fast_fail
gpu.device_id / gpu.threshold_pixels / gpu.max_memory_mb
gpu.transfer_mode = auto|pinned|mapped|unified
gpu.streams = auto|1..N
gpu.fallback = error|cpu|hybrid
```

这些是编码控制参数，不应写进 CRF 核心码流，除非某工具选择影响解码语义；resolved
report 必须记录实际后端、版本、回退和阈值。参数解析、能力发现、调度、内存和 kernel
不得放进同一文件。

每次新增后端必须单独回答：

1. 它解决哪个真实端到端瓶颈？
2. 为什么 CPU 或已有后端不能满足？
3. 依赖、许可证、安装和发布体积是什么？
4. 无设备时如何运行和测试？
5. 是否需要新码流语法？若需要，版本和回退如何设计？
6. 失败时能否完整删除而不污染核心模块？

## 11. 实施阶段

### P0：基准与可观测性

- 建立阶段计时、CPU/GPU 事件、内存/显存和后端报告；
- 固定 CPU 标量参考和当前 AVX2/Rayon 基线；
- 增加端到端、首帧、长序列和小图/大图基准；
- 不改码流、不改变默认后端。

**门槛**：没有可靠 profile 和可复现基线，禁止进入 SIMD 泛化或 GPU。

**状态（2026-09-02）：已完成**。实现如下：

- `crf::performance::telemetry`：RAII 阶段守卫 + 分位数报告，默认关闭（`CRF_PERF=1` 或 `enable()` 开启），零码流影响；
- `crf::performance::bench`：`--bench <dir>` 端到端基准（预热 1 + 测量 5 轮，报告 p50/p95 与 MPix/s 吞吐）；
- 编码入口注入阶段：`encode.rct` / `encode.first_frame` / `encode.rest_frames` / `encode.assemble`，解码入口 `decode.bytes`。

**多尺寸基线（release，本机 2026-09-02）**：

| 组 | 分辨率 | 帧数 | encode p50 | decode p50 | 首帧占比 |
|---|---|---:|---:|---:|---:|
| 2 | 1400×2711 | 2 | 15.2s | 0.30s | 91% |
| 1000 | 1024×1820 | 14 | 12.8s | 0.81s | 47% |
| 2000 | 3541×2508 | 8 | 52.6s | 2.0s | 75% |

**首帧内部细分（1000 组，`encode.adaptive.*` 单轮均值）**：

| 阶段 | 均值 | 结论 |
|---|---:|---|
| `encode.adaptive.satd` | 43.9ms | 可忽略（已 rayon 并行） |
| `encode.adaptive.trial_encode` | 116.8ms | 次要 |
| `encode.adaptive.dct` | 467.6ms | 次要（p95 1.13s，有损 Trellis 更重） |
| `encode.adaptive.planar` | **2750.4ms** | **绝对主导** |

**结论修正**：原 §2.2 假设首帧热点是 SAD/SATD/DCT，实测不成立。真正热点是
planar 候选——3 分量帧上 planar 每平面递归跑一次完整自适应流水线（含自身
SATD/DCT/CABAC 竞争），单次投入约 6 倍于 DCT。后续 CPU 优化以 planar
剪枝/加速为第一优先级，SATD 不再投入。

### P1a：planar 剪枝（profile 验证 + 字节预算 Fast-Fail）

**profile 验证（`--probe-planar`，2026-09-02）**：假设「色度平坦度低于阈值
⟹ planar 必败」可作为剪枝信号。探针对 9 组共 51 帧测量 RCT 后 Co/Cg 长零
行程占比与 planar 实际胜出与否，结论**否定该假设**：

- planar 胜出仅 3/51 帧（组 1 两帧、组 e 一帧），且胜出帧平坦度仅 0.40~0.72；
- 平坦度 0.39~0.41 的组 5 却全败——平坦度与胜出无单调关系，不能作为阈值。

**实现**：改用**数学上安全的字节预算 Fast-Fail**（`encode_planar_payload_limited`）。
planar 逐子平面累加体积，一旦「已累计体积 + 剩余子平面的最小可能体积
（长度前缀 4 + 帧头 11 + 载荷 1 字节）」超过当前最优总长，即提前终止。判定
只用下界，绝不跳过潜在胜者，最终字节与剪枝前逐字节一致。

**收益**：1000 组 release 实测 planar 均值 2750ms→2665ms（−3%），字节 11,090,743
完全不变；全量单测通过。收益有限是因为 lossless 下 planar 极少胜出、Fast-Fail
下界较松；真正大幅收益需进一步分析 planar 子平面内部热点，而非剪枝本身。

### P1b：planar 子平面内部热点分析 + 次级候选剪枝

**子平面内部分解（`encode.adaptive.planar.*` 细分计时，组 1000 五轮 75 次
planar 调用，2026-09-02）**：

| 阶段 | 均值 | 说明 |
|---|---:|---|
| `planar.subplane_y` | 794ms | Y 子平面（全分辨率） |
| `planar.subplane_co` | 926ms | Co 子平面 |
| `planar.subplane_cg` | 923ms | Cg 子平面 |
| `planar.cfl` | 0.012ms | CfL α 搜索，**可忽略** |

三子平面合计 ≈2642ms，占 planar 总耗时 2669ms 的 99%。即：planar 的开销
**几乎全部来自 3 个子平面各自递归跑一遍完整自适应流水线**（SATD + top-2
试编码 + banded + palette + intrabc + cabac + dct）。CfL 搜索与色度下采样
（lossless 下 half_res=false 不运行）均非热点。

**profile 验证（`--probe-planar-sub`，2026-09-02）**：对全部测试组逐子平面
统计胜出 frame_type，结论**决定性**：

- **Y 子平面**：cabac(frame_type=5) 100% 胜出；
- **Co/Cg 子平面**：cabac 主导，rle(frame_type=1) 与 banded(frame_type=2)
  偶有胜出（组 4/9 的 Co/Cg 为 rle，组 10/组 e 的 Cg 为 banded）；
- **dct(6)、intrabc(7)、palette(4)、intra_transform(8) 在全部测试组子平面
  胜出 0 次**。

结论：单分量子平面（生产路径唯一 components==1 场景）经 RCT+CfL 去相关后，
已无 Intrabc 的 8×8 精确重复纹理，也无 DCT 可聚集的频域能量——cabac 对残差流
已最优。dct/intrabc 在子平面上从不 set `best`，跳过它们**字节透明**。banded
会胜出（组 10 Cg 4/4、组 e Cg 1/3），必须保留；palette 在合成低色数单分量
数据下可胜出（`test_palette_roundtrip_low_color`），也保留。

**实现**（`encode_frame_adaptive`）：
- intrabc 候选加 `components > 1` 门控——单分量子平面跳过（本来就仅无损运行）；
- dct 候选加 `!(components == 1 && !fq.is_lossy())` 门控——仅 lossless 单分量
  子平面跳过；有损单分量（色度 chroma_step 量化）下 DCT 可能真实胜出，无探针
  证据，保留。

**收益**（1000 组 release）：

| 指标 | 剪枝前 | 剪枝后 | 变化 |
|---|---:|---:|---:|
| encode p50 | 14904ms | 9176ms | **−38.4%** |
| planar 均值 | 2669ms | 1481ms | **−44.5%** |
| 字节 | 11,090,743 | 11,090,743 | 逐字节一致 |

全量单测 159 passed / 0 failed。这是 P1a Fast-Fail（−3%）之外的大幅收益：
根因不再是"planar 极少胜出"，而是"planar 子平面内部有一半开销花在从不
胜出的次级候选上"。

### P1：CPU 内存和调度

- 消除重复分配、重复转换和嵌套线程池；
- 统一 scratch buffer 和 tile 流水；
- 优化批量/streaming 共用 resolved config；
- 验证低内存和大图峰值。

**门槛**：端到端 ≥10% 或热点 ≥25%，无损/有损质量和码流不退化。

### P2：CPU SIMD 扩展

- 先 SoA diff/RCT/resample/统计；
- 再量化、DCT、固定预测代价；
- x86 AVX2 稳定后评估 AVX-512；
- 并行实现 AArch64 NEON；
- 所有路径保留标量回退和对拍。

### P3：GPU 抽象与 Hybrid 探针

- 先实现 capability/scheduler/memory 的窄边界，不接入正式默认；
- 选择一个大图规则 kernel 做 CPU/CUDA 或 CPU/HIP/Vulkan 对拍；
- 计入传输、同步和初始化；
- 失败自动 CPU 回退。

**门槛**：端到端 ≥1.5× 或有明确 CPU 占用/功耗收益，且至少两个尺寸层成立。

### P4：NVIDIA CUDA

- NVIDIA 平台先做 diff/RCT/resample 和固定 tile DCT；
- 使用异步传输和双缓冲；
- 暂不搬 CABAC 和文件 I/O；
- 记录驱动、compute capability、显存和构建方式。

### P5：AMD HIP/ROCm 与 Vulkan/wgpu

- Linux/ROCm 先验证 HIP；
- AMD Windows 或无 ROCm 环境验证 Vulkan/wgpu；
- 公共 kernel 契约和测试向量复用，后端源文件分开；
- 不把 HIP 性能结果泛化到所有 AMD 平台。

### P6：GPU 扩大 RDO 范围

仅在 P3~P5 证明传输和确定性可控后探索：

- tile 候选代价批量归约；
- 变换尺寸/矩阵并行竞争；
- significance 统计和重建 tile 常驻 GPU；
- 减少 CPU 回读，只回读胜者或紧凑代价。

如果需要大规模原子操作、非确定归约或复杂后端专用分支才能工作，退回 Hybrid，不强行
GPU 化熵编码。

### P7：发布与维护

- 将后端作为可选 feature/组件和独立构建说明；
- 完成无设备安装、驱动错误、回退、升级和卸载测试；
- 文档记录支持矩阵、性能 profile、已知限制和停止条件；
- 每个 backend 文件保持单一职责和行数门禁；
- 发布前重复完整标准构建流程。

## 12. 禁止事项

- 禁止把 CUDA/HIP/Vulkan API 直接写入 `sequence.rs`、`frame.rs` 或 Tauri command；
- 禁止以 GPU kernel 时间替代端到端时间；
- 禁止默认强制 GPU，或在检测到显卡后绕过传输成本模型；
- 禁止为了 GPU 使用浮点近似破坏无损数学语义；
- 禁止在 GPU 与 CPU 之间每个块同步/回读；
- 禁止用未定义原子归约决定有损模式，破坏 deterministic；
- 禁止把厂商专属 shader、编译脚本和许可证复制到公共后端文件；
- 禁止用一个“跨厂商万能 kernel”掩盖不同能力和精度边界；
- 禁止因为 GPU 优化而同时改动格式、质量预设、UI 和无关重构；
- 禁止超过 1000 行后继续堆功能；
- 禁止为了性能删除标量参考、回退路径或失败测试。

## 13. 构建前检查清单

每次性能实现轮必须记录：

```text
目标热点与 profile 证据：
CPU/GPU 后端及设备：
修改模块及单一职责：
最大手写文件及行数：
是否触及 800 行预警：
是否改变码流/API/默认后端：
输入/输出布局与数值契约：
CPU 标量对拍：
GPU 与 CPU 对拍：
传输/同步/初始化耗时：
端到端 p50/p95 与像素吞吐：
内存/显存峰值：
无损结果：
有损质量/码率结果：
批量/streaming 结果：
回退和设备失败结果：
未执行项及原因：
停止或回退条件：
```

缺少 profile、回退路径、对拍或行数审查时，性能优化不得进入正式构建。

## 14. 与其他文档的关系

- 项目硬门禁、文件拆分和构建流程以 `project-standards.md` 为准；
- 有损首帧、质量和码率目标以 `first-frame-optimization-plan.md` 为准；
- 精细性能参数和 resolved config 以 `lossy-tuning-interface-plan.md` 为准；
- 已实现的 CPU 优化历史以 `optimization-review.md` / `optimization-completed.md` 为准；
- 已发布码流字段以根目录 `crf格式标准.md` 为准。

本规划只增加性能路线，不改变现有格式字段、无损语义或当前默认后端。任何正式实现
必须先完成 P0，并按项目标准逐阶段验收。
