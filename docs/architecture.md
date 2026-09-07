# CRF Viewer 技术架构文档

> 本文包含部分早期架构描述。后续模块职责、依赖方向、单文件 1000 行硬限制及
> “禁止多功能杂交”要求，以
> [《CRF 项目开发、算法与构建标准》](project-standards.md)为强制准则；行为现状仍须
> 以代码和当前格式规范核对。

当前文档中的 `crf/encoder.rs`、`crf/decoder.rs` 等结构图是早期概览，不代表目标实现
边界。编码器/解码器的正式分层、公共核心、容器层、帧管线、参考状态和迁移顺序以
[《CRF 编码器/解码器分层架构重构规划》](codec-architecture-refactor-plan.md)为准。

## 目录

- [系统概述](#系统概述)
- [整体架构](#整体架构)
- [模块划分](#模块划分)
- [数据流设计](#数据流设计)
- [接口与交互（CLI / Rust API）](#接口与交互cli--rust-api)
- [核心算法](#核心算法)

---

## 系统概述

CRF Viewer 是一个**纯 Rust CLI 工具**（无前端、无 Tauri GUI），用于差分图片序列的无损/有损压缩、存储与校验。系统采用单进程 Rust 架构，通过命令行参数驱动编解码、基准与探针。

### 设计目标

1. **高性能**：快速编解码，流畅的用户交互
2. **无损保证**：像素级精确还原
3. **跨平台**：Windows、macOS、Linux 统一代码库
4. **轻量级**：最小化外部依赖，自包含实现

---

## 整体架构

```
+=====================================================================+
|                        CRF CLI（纯 Rust 进程）                        |
+=====================================================================+
|                                                                      |
|  +----------------------------------------------------------------+  |
|  |                    CLI 参数解析层                                |  |
|  |   main.rs / cli_config.rs：--test / --bench / --probe-* /       |  |
|  |   --lossy-quality / --expert-config / --dump-*                  |  |
|  +----------------------------------------------------------------+  |
|                              |                                       |
|                              v                                       |
|  +----------------------------------------------------------------+  |
|  |                  crf::codec 对外 facade                          |  |
|  |         (EncodeRequest / DecodeRequest / ResolvedConfig)        |  |
|  +----------------------------------------------------------------+  |
|                              |                                       |
|                              v                                       |
|  +----------------------------------------------------------------+  |
|  |                    Rust 核心层（编解码）                          |  |
|  |  +-------------+ +-------------+ +---------------------------+  |  |
|  |  |  encoder    | |  decoder    | |  core（公共契约与数学）     |  |  |
|  |  | (session/    | | (container/ | |  + backend（scalar/SIMD/   |  |  |
|  |  |  frame/...)  | |  frame/...) | |    GPU 能力探测）          |  |  |
|  |  +-------------+ +-------------+ +---------------------------+  |  |
|  +----------------------------------------------------------------+  |
|                              |                                       |
|                              v                                       |
|  +----------------------------------------------------------------+  |
|  |                    文件系统                                      |  |
|  |        (图像文件读写 / .crf 文件读写)                            |  |
|  +----------------------------------------------------------------+  |
+=====================================================================+
```

> 无 `Tauri IPC`、无 `React 前端层`——仓库没有 `src/` 前端源码，所有输入输出经由
> CLI 参数与文件系统完成。

---

## 模块划分

### 1. 前端模块 (React) —— 不存在

> **项目现状（2026-09-07 更新）**：本项目为**纯 CLI**，仓库**没有** React/Tauri 前端
> 源码，也没有 `src/`、`package.json`、`vite.config.ts` 等前端文件。Cargo.toml 无
> tauri 依赖。以下目录树是早期规划的历史记录，**不代表已实现**，仅作未来 UI 参考；
> 有损参数接口的 UI 落地点是 `expert_panel_schema()` / `lossy_expert_schema_json()`
> 导出的 schema 与解析 API（见
> [《CRF 有损精细参数接口规划》](lossy-tuning-interface-plan.md)）。

```text
（历史规划，未实现）
src/
+-- components/           # UI 组件
|   +-- ImageViewer/     # 图像查看器
|   +-- SequencePanel/   # 序列面板
|   +-- MetadataView/    # 元数据查看
|   +-- EncodeDialog/    # 编码设置对话框
|   +-- DropZone/        # 拖拽导入区域
|   +-- Common/          # 通用组件
+-- hooks/               # 自定义 Hooks
|   +-- useImageLoader.ts
|   +-- useCrfCodec.ts
|   +-- useSequencePlayer.ts
+-- stores/              # 状态管理
|   +-- appStore.ts      # 应用状态
|   +-- imageStore.ts    # 图像状态
+-- services/            # 服务层
|   +-- tauriCommands.ts # Tauri 命令封装
+-- utils/               # 工具函数
|   +-- format.ts        # 格式化工具
|   +-- constants.ts     # 常量定义
+-- types/               # TypeScript 类型
    +-- crf.ts           # CRF 相关类型
    +-- image.ts         # 图像相关类型
```

**职责说明**（历史规划，未实现）：

| 模块 | 职责 |
| :--- | :--- |
| `ImageViewer` | 渲染图像、支持缩放/平移/对比 |
| `SequencePanel` | 序列导航、帧选择、播放控制 |
| `MetadataView` | 显示文件头、帧信息、编码参数 |
| `EncodeDialog` | 编码参数配置、进度显示 |
| `tauriCommands` | 封装所有后端调用，提供类型安全的 API |

### 2. 后端模块 (Rust)

> 以下结构与 `src-tauri/src/` 实际源码一致（2026-09-03 核对）。早期文档中的
> `crf/encoder.rs`/`crf/decoder.rs`/`format.rs`/`header.rs` 扁平结构已废弃：
> `format/` 与顶层 `transform/` 目录已删除，实现迁入 `core/`；
> 分层契约以 [《CRF 编码器/解码器分层架构重构规划》](codec-architecture-refactor-plan.md)为准。

```
src-tauri/src/
+-- main.rs              # 应用入口（纯 CLI 参数解析；无 Tauri）
+-- cli_config.rs        # CLI 配置解析（有损 V2 flags / 专家配置）
+-- test/                # 集成测试（批量/流式/探针）
|   +-- mod.rs
|   +-- batch.rs         # 批量 + 流式编码测试套件
|   +-- probe.rs         # 分区/DCT 候选探针
+-- crf/                 # CRF 格式核心
|   +-- mod.rs           # 模块导出 + 公共 re-export
|   +-- error.rs         # 错误类型（CrfError/CrfResult）
|   +-- checksum.rs      # CRC32 校验
|   +-- codec/           # 对外 facade（应用层唯一稳定入口）
|   |   +-- mod.rs
|   |   +-- encode.rs    # EncodeRequest → EncodeReport
|   |   +-- decode.rs    # DecodeRequest → DecodeResult
|   |   +-- config.rs    # V2 JSON 解析 / 专家面板 schema
|   |   +-- error.rs     # CodecError 对外错误映射
|   +-- core/            # 编解码器共享的纯契约与数学（不依赖 encoder/decoder）
|   |   +-- mod.rs
|   |   +-- contract.rs  # FramePacket / ReferenceState / ResolvedConfig / CandidateResult
|   |   +-- domain/      # 数据模型与不变量（ImageData/EncodeParams/Flags/FrameHeader）
|   |   +-- config/      # 请求/预设/解析（lossy_v2 有损参数模型）
|   |   +-- bitstream/   # 格式契约与容器边界（header/constants）
|   |   +-- color/       # YCoCg-R 可逆色彩变换（rct）
|   |   +-- prediction/  # 预测契约（intra 帧内预测 / cost SATD 代价）
|   |   +-- transform/   # 变换/量化/RDOQ/重建（dct4/dct8/rect/plane/qm/quant/rdoq/closed_loop/reconstruct）
|   |   +-- entropy/     # 熵编码语法（scan zigzag / golomb k 选择 / cabac RC 常量 / context）
|   |   +-- perceptual/  # 噪声感知估计与软阈值（noise）
|   +-- encoder/         # 编码器
|   |   +-- mod.rs
|   |   +-- session/     # 序列生命周期（session/batch/reference）
|   |   +-- frame/       # 帧管线（mod 单帧入口 / candidate 候选竞争 / intrabc 块复制）
|   |   +-- dct_path/    # frame_type=6 候选编排
|   |   +-- sequence.rs  # 序列编码主流程（encode_sequence）
|   |   +-- streaming.rs # 流式编码 API（>50 帧）
|   |   +-- golomb.rs / rle_golomb.rs / exp_golomb.rs / rle_cabac.rs / coeff_cabac.rs  # 熵编码器族
|   |   +-- planar.rs / banded.rs / intra_transform.rs / transform.rs  # 候选
|   |   +-- scratch.rs   # 帧/条带 Scratch Buffer 复用
|   +-- decoder/         # 解码器
|   |   +-- mod.rs
|   |   +-- container/   # 容器层（reader/footer，CRC/边界校验）
|   |   +-- frame/       # 帧分派层（dispatcher/packet）
|   |   +-- reconstruct/ # 重建层（逆预测/逆变换/色彩还原）
|   |   +-- session.rs   # 解码会话（序列编排 + 时间参考恢复）
|   |   +-- golomb.rs / rle_golomb.rs / exp_golomb.rs / rle_cabac.rs / coeff_cabac.rs  # 熵解码器族
|   |   +-- planar.rs / banded.rs / palette.rs / intrabc.rs / intra_transform.rs / transform.rs  # payload 解码
|   +-- backend/         # 性能后端（可选，CPU 标量/ SIMD 永远保留）
|   |   +-- mod.rs
|   |   +-- scalar/      # 标量参考实现（正确性基准）
|   |   +-- cpu/         # AVX2 SIMD 运行时分派
|   |   +-- gpu/         # CUDA 能力探测（capability/runtime/memory）
|   |   +-- ops.rs       # 后端算子契约
|   +-- performance/     # 可观测性层（telemetry 阶段计时 / bench / probe）
```

**职责说明**：

| 模块 | 职责 |
| :--- | :--- |
| `crf::codec` | 对外 facade：应用层唯一稳定入口，编排 session，禁止算法/容器/设备 API |
| `crf::core` | 编解码器共享的纯契约与数学；不知道文件路径和设备 API |
| `crf::encoder` | 将图像序列编码为 .crf 文件（session + frame 管线 + 熵编码器） |
| `crf::decoder` | 将 .crf 文件解码为图像序列（容器 → 分派 → 重建 → 会话） |
| `crf::backend` | 性能后端（scalar/SIMD/GPU），仅通过 backend trait 接入，不依赖 session |
| `crf::performance` | 阶段计时 telemetry + 端到端 bench + 探针，不进入生产路径 |
| `test` | 批量/流式集成测试与探针（大图组、字节透明校验） |

### 3. CRF 编解码核心

```
crf/
+-- codec/                       # 对外 facade
|   +-- encode(request)          # EncodeRequest → EncodeReport
|   +-- decode_from_bytes()      # DecodeRequest → DecodeResult
+-- core/                        # 共享契约与数学
|   +-- contract.rs              # FramePacket / ReferenceState / ResolvedConfig / CandidateResult
|   +-- domain/                  # ImageData / EncodeParams / Flags / FrameHeader / FrameIndexEntry
|   +-- config/lossy_v2/         # 有损参数模型（types/resolve/kernel/builder/json/ui）
|   +-- bitstream/               # CrfHeader / 常量（HEADER_SIZE / FRAME_HEADER_SIZE / BAND_HEIGHT）
|   +-- color/rct.rs             # YCoCg-R 正逆变换
|   +-- prediction/              # intra（predict_at/apply/undo）+ cost（SATD）
|   +-- transform/               # dct4/dct8/rect/plane/qm/quant/rdoq/closed_loop/reconstruct
|   +-- entropy/                 # scan（zigzag）+ golomb（k 选择）+ cabac（RC 常量）+ context（MA 树）
|   +-- perceptual/noise.rs      # 噪声感知估计/软阈值
+-- encoder/
|   +-- session/                 # EncodeSession（batch/reference）
|   +-- frame/                   # 单帧入口 / 候选竞争 / 帧内块复制
|   +-- sequence.rs              # encode_sequence（头构建/RCT/golden 差分/文件组装）
|   +-- streaming.rs             # StreamingEncoder（逐帧推送，>50 帧）
|   +-- golomb.rs / rle_golomb.rs / exp_golomb.rs / rle_cabac.rs / coeff_cabac.rs
+-- decoder/
|   +-- container/               # reader/footer（CRC、边界）
|   +-- frame/                   # dispatcher/packet（frame_type 路由）
|   +-- reconstruct/             # 逆预测/逆变换/色彩还原
|   +-- session.rs               # DecodeSession（decode_bytes + 时间参考恢复）
|   +-- golomb.rs / rle_golomb.rs / exp_golomb.rs / rle_cabac.rs / coeff_cabac.rs
+-- checksum.rs                  # crc32()
```

---

## 数据流设计

### 1. 编码流程（图像 -> CRF）

```
+--------------+     +--------------+     +--------------+     +--------------+
|  用户导入    | ==> |  图像加载    | ==> |  预处理      | ==> |  CRF 编码    |
|  图像文件    |     |  (image-rs)  |     |  格式统一    |     |              |
+--------------+     +--------------+     +--------------+     +--------------+
                                                |                   |
                                                v                   v
                                          +--------------+     +--------------+
                                          |  内存中的    |     |  写入文件    |
                                          |  像素数据    |     |  (.crf)      |
                                          +--------------+     +--------------+
```

**详细步骤**：

1. **图像加载**：使用 `image-rs` 读取 PNG/BMP/TIFF/JPEG/WebP 文件
2. **格式统一**：转换为统一的像素格式（RGB/灰度，8/10/12/16 位）
3. **残差计算**：golden 参考架构——首帧编码并本地重建得到 `G_hat`，后续帧差分至
   `G_hat`（而非相邻帧差值）；有损时误差不沿链累积，帧间零依赖可全并行
4. **熵编码**：
   - 帧内预测（MED/Paeth/DC/planar/斜向等）+ 自适应候选竞争
   - 变换域候选（DCT/矩形变换 + 感知矩阵 + Trellis）+ CABAC/RLE/Golomb 混合熵编码
   - 帧级/条带级/三平面多路竞争，字节最小者胜出（单调不劣化）
5. **文件组装**：写入文件头 + 帧索引 + 编码数据 + CRC32 文件尾

### 2. 解码流程（CRF -> 图像）

```
+--------------+     +--------------+     +--------------+     +--------------+
|  打开 CRF    | ==> |  解析头部    | ==> |  解码帧      | ==> |  还原图像    |
|  文件        |     |              |     |              |     |              |
+--------------+     +--------------+     +--------------+     +--------------+
      |                   |                   |                   |
      v                   v                   v                   v
+--------------+     +--------------+     +--------------+     +--------------+
|  读取字节流  |     |  验证魔数    |     |  熵解码      |     |  输出文件    |
|              |     |  版本检查    |     |  还原像素    |     |  (PNG等)     |
+--------------+     +--------------+     +--------------+     +--------------+
```

### 3. 用户交互流程（CLI）

```
+================================================================+
|                         命令行交互                              |
+================================================================+
|                                                                 |
|   +------------+     +------------+     +------------+         |
|   | 指定输入组 | ==> | 选择参数   | ==> | 执行编码   |         |
|   | (目录/文件) |     | (质量/专家)|     | (cargo run)|         |
|   +------------+     +------------+     +------------+         |
|                                               |                |
|                                               v                |
|   +------------+     +------------+     +------------+         |
|   | 校验/导出  | <== | 产物 .crf  | <== | --test 校验 |        |
|   | (verify)   |     | (文件系统) |     | (往返/像素) |        |
|   +------------+     +------------+     +------------+         |
|                                                                 |
+================================================================+
```

---

## 接口与交互（CLI / Rust API）

> 项目为**纯 CLI**：无 Tauri 命令系统、无前端调用。所有能力通过
> `src-tauri/src/main.rs` 的命令行入口与 `crf::codec` facade（Rust 层唯一稳定入口）
> 暴露。以下历史规划中的 `#[tauri::command]` / TypeScript 结构**均已废弃**，
> 仅保留作为未来 UI 参考。

### CLI 入口（main.rs）

| 参数 | 功能 |
| :--- | :--- |
| `--test <dir>` | 端到端集成测试（编码→解码→逐像素校验；支持质量/流式/探针） |
| `--bench <dir>` | 端到端基准（预热 + 5 轮，p50/p95 与 MPix/s） |
| `--lossy-quality <q>` | 有损质量预设（如 96.50） |
| `--expert-config <path>` | V2 专家配置 JSON |
| `--dump-resolved-config <path>` | 导出 resolved 配置 |
| `--dump-expert-schema <path>` | 导出专家面板 schema |
| `--probe-*` | 算法探针（split/planar/banded/ringing/lambda/activity/coeff-ctx 等） |
| `--debug-pixels <a> <b>` | 像素级调试 |

### Rust 入口（crf::codec facade）

```rust
// 编码：EncodeRequest → EncodeReport
crf::codec::encode(request)?;

// 解码：DecodeRequest → DecodeResult
crf::codec::decode_from_bytes(data)?;

// 配置：V2 JSON 解析 / resolved 导出 / 专家面板 schema
crf::codec::resolve_lossy_json(request_json)?;
crf::codec::lossy_expert_schema_json()?;
```

### 数据结构定义

核心数据模型在 `crf::core::domain`（`ImageData` / `EncodeParams` / `Flags` /
`FrameHeader`）与 `crf::core::contract`（`FramePacket` / `ReferenceState` /
`ResolvedConfig` / `CandidateResult`），全部为 Rust 类型；JSON 序列化仅用于
`--expert-config` 输入与 `--dump-*` 导出。

---

## 核心算法

### 1. Golomb-Rice 编码

适用于残差数据集中在零附近的场景。

```
编码过程：
1. Zigzag 扫描：二维 -> 一维，有符号 -> 无符号
2. 对每个值 v：
   - 商 q = v >> k
   - 余数 r = v & ((1 << k) - 1)
   - 编码：q 个 '1' + 一个 '0' + k 位二进制 r
3. 参数 k 根据数据自适应调整
```

### 2. 指数哥伦布编码 (EGC)

适用于数值动态范围较大的残差。

```
编码过程：
1. 对非负整数 v：
   - 计算 m = floor(log2(v + 1))
   - 编码：m 个 '0' + (m+1) 位二进制表示 (v + 1)
2. 对有符号值：先用 Zigzag 映射为非负整数
```

### 3. 变换编码（可选）

```
1. 将残差帧划分为 block_size x block_size 的块
2. 对每个块进行整数 DCT 或 Hadamard 变换
3. 变换是完全可逆的，量化步长为 1（无损）
4. 对变换系数进行扫描和熵编码
```
