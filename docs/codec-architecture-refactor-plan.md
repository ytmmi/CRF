# CRF 编码器/解码器分层架构重构规划

**规划日期**：2026-08-26  
**适用范围**：CRF 编码器、解码器、公共格式核心、CPU/GPU 后端、批量/streaming API  
**状态**：迁移进行中（P0~P2 完成，P3~P5 部分完成，P6 测试子项完成、文档收敛进行中）  
**强制约束**：[CRF 项目开发、算法与构建标准](project-standards.md)

> **进度同步（2026-09-03）**：本规划已从「纯规划」转入「迁移进行中」。分层骨架已落地，
> decoder→encoder 反向依赖已解除，容器/分派/重建/会话分层与编码 session 层均已建立。
> 以下为当前迁移进度表，与 `src-tauri/src/crf/` 实际源码逐一核对：

### 迁移进度表（P0~P6）

| 阶段 | 规划内容 | 状态 | 落地证据 |
|---|---|---|---|
| P0 冻结边界+骨架 | 建立目标目录与公共契约 | ✅ 完成 | `codec/`、`core/`、`backend/` 三骨架；`core/contract.rs` 定义 `FramePacket`/`CandidateResult`/`ReferenceState`/`ResolvedConfig` 四契约 |
| P1 解除反向依赖 | dct_path 逆变换迁 core、CtxModel 迁 core | ✅ 完成 | `CtxModel` 迁至 `core/entropy/context.rs`；`dct_path` 逆变换迁至 `core/transform/reconstruct.rs`；decoder 中残留 `use crate::crf::encoder` 全部在 `#[cfg(test)]` 模块内 |
| P2 拆容器+分派 | decoder 容器/分派/重建/会话分层 | ✅ 完成 | `decoder/container/`、`decoder/frame/`（dispatcher/packet）、`decoder/reconstruct/`、`decoder/session.rs` 均落地，`decode_from_bytes` 转发至 `DecodeSession` |
| P3 拆 encoder session+frame | batch/streaming session、候选/RDO/payload | ✅ 完成 | `encoder/session/{batch,reference,session}.rs` 已建；P3.b 配置解析收敛已完成（facade 解析一次透传 `ResolvedConfig`，streaming 首帧 push 时补 components）；P3.c `encoder/frame/` 目录已建（`mod.rs` 单帧入口 + `candidate.rs` 候选竞争 + `intrabc.rs` 帧内块复制），原 `frame.rs`/`adaptive.rs`/`intrabc.rs` 三个单体迁入 |
| P4 整理公共工具层 | prediction/transform/entropy/color/config 迁 core | ◐ 部分（约 85%）| 已迁：prediction/{intra,cost}、transform/{dct4,dct8,rect,reconstruct,quant,closed_loop}、entropy/{context,scan,golomb,cabac}、color/rct、bitstream/{header,constants}、domain/types、config/lossy_v2，旧 `format/` 目录已删除；熵编码「语法定义 vs 状态机」已分离（k 选择 + RC 常量迁 core，GolombEncoder/RangeEncoder 等位级状态机按 §3.7 留在 encoder/decoder）；未迁：rdoq、noise |
| P5 接入性能 backend | scalar→SIMD→GPU | ◐ 骨架完成 | `backend/{scalar,cpu,gpu}` 已建；GPU 仍为能力探测（capability/cuda/runtime/memory），未接入默认 |
| P6 测试+文档收敛 | 拆大测试文件、更新架构图 | ◐ 部分 | ✅ 测试已拆：`test/{batch,mod,probe}.rs`、`encoder/tests/{lossy,roundtrip,rct_bypass,mod}.rs`，全项目最大文件 714 行（`core/prediction/intra.rs`）无超限；❌ 架构图与迁移记录文档收敛仍在进行（即本文档） |

### 剩余迁移项（按优先级）

1. **P4 续迁 `rdoq`/`noise`**：`rdoq`（Trellis 率失真优化量化）→ `core/transform/`，
   `noise`（噪声感知估计/软阈值）→ `core/perceptual/`（新建）。
2. **P6 文档收敛**：更新架构图、模块职责与迁移记录（本文档及 architecture.md）。

## 1. 重构动机

当前源码已经有 `encoder/`、`decoder/`、`format/`、`transform/` 子目录，但这只是文件
分组，不是完整的架构层级。主要问题如下：

1. 编码器顶层同时承担序列调度、首帧闭环、参考生成、候选竞争和码流组装；
2. 解码器顶层同时承担容器读取、帧头解析、frame type 分派、payload 解码、预测撤销、
   RCT 出口和 CRC/文件读取；
3. `encoder::dct_path` 被 decoder 直接调用，编码器内部实现反向成为解码器依赖；
4. `format` 同时包含公开数据模型、质量映射、预测公式、RCT、SIMD 和位格式辅助，
   公共领域模型与具体实现没有边界；
5. encoder/decoder 的预测、变换、熵编码和码流语义没有统一的公共契约，容易出现编码端
   写一种语义、解码端通过特殊分支猜另一种语义；
6. 批量与 streaming 有相似但不同的入口，参数解析、首帧参考和中间缓冲可能产生分叉；
7. CPU/GPU 性能后端尚无明确插入层，未来容易把 CUDA/HIP/Vulkan API 写进序列逻辑；
8. 测试按历史实现聚集，不能直接表达容器、预测、变换、熵编码和参考闭环的边界。

重构目标不是把目录改得更深，而是让每个层级有唯一职责、稳定数据契约和明确依赖方向。

## 2. 目标架构总览

```text
应用层
  ├─ Tauri commands / CLI / batch API / streaming API
  └─ 只负责输入转换、取消、进度、错误呈现

公共 codec facade
  ├─ EncodeRequest / DecodeRequest
  ├─ ResolvedConfig / CapabilityReport
  └─ 编码器、解码器生命周期与版本入口

序列层
  ├─ EncodeSession / StreamingEncodeSession
  ├─ DecodeSession / RandomAccessSession
  ├─ 首帧、golden、previous、scene anchor 生命周期
  └─ 帧级调度、并行边界、内存预算

帧管线层
  ├─ EncoderFramePipeline：候选生成 → RDO → 重建 → payload
  ├─ DecoderFramePipeline：frame header → payload → 重建
  ├─ intra / residual / anchor / reference 工具
  └─ 不直接读写文件，不直接调用 Tauri/GPU API

工具层
  ├─ prediction
  ├─ transform + quantization + RDOQ
  ├─ entropy / coefficient coding
  ├─ color / resample
  └─ CPU/GPU kernel trait

码流层
  ├─ container reader/writer
  ├─ header/index/frame header/footer
  ├─ payload registry / version / feature bits
  └─ bounds / CRC / compatibility validation

后端层
  ├─ scalar reference
  ├─ CPU SIMD/threads
  └─ optional GPU CUDA/HIP/Vulkan/wgpu
```

### 2.1 目标源码目录骨架

目录只表达依赖和职责边界；实现阶段可以按文件规模继续拆分，但不得把不同层级
重新合并到 `encoder/mod.rs` 或 `decoder/mod.rs`：

```text
src-tauri/src/crf/
├─ codec/                         # 对外 facade；不承载算法
│  ├─ encode.rs                   # EncodeRequest → EncodeReport
│  ├─ decode.rs                   # DecodeRequest → DecodeResult
│  └─ error.rs                    # 对外错误映射
├─ core/                          # 编解码器共享的纯契约/数学
│  ├─ domain/                     # image/frame/tile/coefficient
│  ├─ config/                     # request/preset/resolve/capability
│  ├─ bitstream/                  # header/index/frame/footer/边界
│  ├─ color/                      # layout/RCT/resample/range
│  ├─ prediction/                 # mode/intra/residual/IntraBC/cost
│  ├─ transform/                  # DCT/矩形变换/量化/RDOQ/逆变换
│  └─ entropy/                    # token/scan/RLE/Golomb/CABAC/context
├─ encoder/
│  ├─ session/                    # batch/streaming/reference/scheduler
│  ├─ frame/                      # candidate/RDO/reconstruct/payload
│  └─ tools/                      # core 与 backend 的适配器
├─ decoder/
│  ├─ container/                  # reader/header/index/footer/bounded input
│  ├─ frame/                      # packet/dispatcher/registry
│  ├─ payload/                    # 各 payload 语法 handler
│  └─ reconstruct/                # 逆预测/逆变换/色彩/参考恢复
├─ backend/
│  ├─ scalar/                     # 唯一正确性基准
│  ├─ cpu/                        # Rayon、AVX2/AVX-512/NEON
│  └─ gpu/                        # CUDA、HIP、Vulkan/wgpu（可选 feature）
└─ tests/                         # 按 domain/bitstream/frame/sequence/backend 分组
```

`codec` 是唯一稳定的应用入口；`core` 不知道文件路径和设备 API；`encoder` 与
`decoder` 只能通过 `core` 契约交换语义；`backend` 只实现 kernel trait；`tests`
调用公共契约和 golden vectors，不能成为生产逻辑的提供者。旧目录在迁移期间只能
保留薄转发层，转发完成后删除，避免新旧实现并存。

目标依赖方向：

```text
app
  ↓
facade
  ↓
sequence ───────────────┐
  ↓                      │
frame pipeline           │
  ↓                      │
prediction / transform / entropy / color
  ↓                      │
backend kernels          │
  ↓                      │
domain + bitstream contracts
                          ↑
container reader/writer ──┘
```

更严格地说：

- 应用层只能依赖 facade；
- facade 只能编排公开 request、resolved config 和 session；
- sequence 只能依赖 frame pipeline、reference、container facade 和 backend scheduler；
- 工具层只能依赖公共 domain、layout 和 backend trait；
- 码流层只能依赖 domain/format contract 和位读写；
- decoder 不得依赖 encoder；encoder 与 decoder 通过公共 transform/prediction/entropy
  契约共享纯算法或语法定义；
- backend 不得依赖具体 encoder/decoder session；
- 任何底层模块不得依赖 Tauri、UI、测试运行器或文件路径。

## 3. 公共核心层

### 3.1 domain：数据模型与不变量

建议建立不包含编码决策的公共领域模型：

```text
core/domain/
  image.rs          # ImagePlane/ImageFrame、尺寸、stride、位深
  pixel.rs          # 分量、范围、符号和饱和规则
  frame.rs          # FrameKind、ReferenceKind、重建帧元数据
  tile.rs           # TileRect、边界、邻域窗口
  coefficient.rs    # CoefficientBlock、扫描位置、EOB 元数据
  error.rs          # 领域错误，不包含 Tauri 字符串
```

领域类型只表达数据和不变量：

- 维度、分量数、stride 和 buffer 长度必须可验证；
- `ImageFrame` 区分原始帧、残差帧、重建帧，禁止靠布尔字段猜语义；
- `ReferenceFrame` 标明 reconstructed golden/previous/anchor，禁止携带原始图隐式补偿；
- `TileRect` 负责边界裁剪，不让每个预测器重复处理边界；
- 系数块携带 transform size、plane、scan 和 EOB 语义，但不负责写位流。

### 3.2 config：请求、解析和有效配置

```text
core/config/
  request.rs        # EncodeRequest / DecodeRequest
  preset.rs         # 质量预设与 revision
  lossy.rs          # 精细有损参数
  capability.rs     # CPU/GPU 能力
  resolve.rs        # 继承、Auto、冲突与 resolved config
  report.rs         # warnings/fallback/fingerprint
```

配置层禁止执行像素循环、分配编码 buffer、读写文件或选择具体 frame type。它只输出
不可变 `ResolvedConfig`，后续 session 全部使用同一份实例；批量和 streaming 不得分别解析。

### 3.3 bitstream：格式契约与容器

```text
core/bitstream/
  version.rs        # bitstream version/feature bits
  header.rs         # Header/Flags 编解码
  index.rs          # FrameIndexEntry
  frame_header.rs   # FrameHeader/coding_params
  footer.rs         # Footer/CRC 语义
  reader.rs         # bounded reader/bit reader
  writer.rs         # bounded writer/bit writer
  payload_registry.rs # frame type 与 decoder handler 注册
  validate.rs       # 长度、偏移、保留位、兼容性
```

容器层只负责“字节和边界”，不负责预测、DCT、量化或参考恢复。它应能在不加载完整图像
的情况下解析元数据、索引和 payload 范围，并提供安全的 bounded slice/reader。

### 3.4 color：色彩和采样

```text
core/color/
  rct.rs            # YCoCg-R 纯数学正逆变换
  layout.rs         # RGB/plane/interleaved layout
  resample.rs       # 4:4:4/4:2:2/4:2:0 抽样与重建
  range.rs          # 位深、范围、饱和与色彩边界
```

`rct.rs` 不应继续放在既包含 SIMD 又包含公开 `format` 类型的混合文件中。CPU/GPU
实现通过 backend kernel 提供，公共 `color` 只定义参考语义、布局和测试向量。

### 3.5 prediction：预测契约

```text
core/prediction/
  mode.rs           # PredictionMode 及合法值
  intra.rs          # DC/H/V/MED/Paeth/planar/D45/D135
  residual.rs       # 预测残差与逆预测契约
  intrabc.rs        # COPY/PRED 语义
  context.rs        # 因果邻域、边界和邻块上下文
  cost.rs           # SAD/SATD/梯度代价接口
```

预测函数必须明确输入是原始邻域还是 reconstructed 邻域。正式编码路径只能引用允许的
已重建数据；探针可以读取原图，但不得复用为生产函数。

### 3.6 transform：变换、量化和重建

```text
core/transform/
  transform.rs      # TransformKind/TransformSize
  dct4.rs           # 4×4 lifting DCT
  dct8.rs           # 8×8 lifting DCT
  rect.rs           # 4×8/8×4 等矩形变换
  quant.rs          # 标量/矩阵/定点量化
  rdoq.rs           # Trellis/RDOQ
  reconstruct.rs    # 逆量化/逆变换/块重建
```

编码和解码共享的是数学契约与逆变换语义，不是“decoder 调用 encoder 私有模块”。当前
`encoder/dct_path` 中的公共逆变换必须迁移到 `core/transform/reconstruct` 或等价的
公共模块；decoder 只能依赖公共 transform，不得依赖 encoder namespace。

### 3.7 entropy：符号化和熵编码

```text
core/entropy/
  symbol.rs         # signed/unsigned/zigzag/token 定义
  scan.rs           # zigzag/directional scan/EOB
  rle.rs            # 零行程和 run token
  golomb.rs         # Golomb/Rice/Exp-Golomb 语法
  cabac.rs          # CABAC 状态与二值化
  coeff.rs          # coefficient token stream
  context.rs        # 上下文模型定义和更新契约
```

建议把“语法定义”和“具体 encoder/decoder 状态机”分开：

- `entropy/*` 定义 token、状态转移和边界；
- `encoder/entropy_writer` 将系数/预测残差写入 token；
- `decoder/entropy_reader` 从 bounded bit reader 还原 token；
- 两侧共享向量化测试、黄金向量和错误分类，不互相调用私有实现。

## 4. 编码器目标层级

### 4.1 Public facade

```text
codec/encode.rs
  encode(request) -> EncodeResult
  encode_to_writer(request, writer) -> EncodeReport
```

职责只有：参数校验入口、session 创建、取消/进度传播、错误边界和结果报告。禁止在
facade 内出现预测、变换、熵编码或设备 API。

### 4.2 EncodeSession：序列生命周期

```text
encoder/session/
  session.rs        # EncodeSession 生命周期
  batch.rs          # 批量输入与内存预算
  streaming.rs      # streaming push/finish 外壳
  reference.rs      # golden/previous/anchor reconstructed refs
  scheduler.rs      # 帧级并行、阶段依赖、backend 调度
  output.rs         # frame index/body/footer 委托
```

`EncodeSession` 负责：

1. 接收并验证输入帧；
2. 解析并冻结 `ResolvedConfig`；
3. 编码首帧并得到 `G_hat`；
4. 建立后续帧的 reconstructed reference；
5. 安排可并行的残差帧；
6. 请求 `FrameEncoder` 产生帧 payload；
7. 交给 container writer 组装文件。

它不实现候选预测、DCT、CABAC 或 GPU kernel。

### 4.3 FrameEncoder：单帧候选编排

```text
encoder/frame/
  frame_encoder.rs   # 单帧入口与阶段状态
  candidate.rs       # 候选描述、预算、tie-break
  intra.rs           # 首帧 intra 候选
  residual.rs        # 差分/残差帧候选
  reference.rs       # 参考快照与重建输入
  rdo.rs             # J=D+λR/质量约束裁决
  reconstruct.rs     # 编码后本地重建
  payload.rs         # 胜者 payload 请求
```

单帧阶段必须明确：

```text
InputFrame + ReferenceSnapshot
  → PredictorCandidate
  → TransformCandidate
  → QuantizedCandidate
  → EntropyCandidate
  → RDO decision
  → EncodedFramePayload + ReconstructedFrame
```

候选只能返回数据和代价，不自行写最终文件。所有候选使用固定 tie-break，保证
deterministic。

### 4.4 工具适配层

```text
encoder/tools/
  prediction_runner.rs
  transform_runner.rs
  quant_runner.rs
  entropy_writer.rs
  backend_dispatch.rs
```

这些文件是 frame pipeline 与公共工具/backend 之间的适配，不应重新实现算法。每个工具
必须可以单独做标量、CPU SIMD 和 GPU backend 对拍。

## 5. 解码器目标层级

### 5.1 Public facade

```text
codec/decode.rs
  decode_from_bytes(data) -> DecodeResult
  decode_from_reader(reader) -> DecodeResult
  inspect_metadata(data/reader) -> Metadata
```

只负责建立 `DecodeSession`、错误映射、取消和结果出口。

### 5.2 ContainerReader：安全读取

```text
decoder/container/
  session.rs        # DecodeSession 生命周期
  reader.rs         # bytes/Read+Seek 统一输入
  header.rs         # Header 读取/校验
  index.rs          # 索引读取/范围校验
  frame_reader.rs   # bounded frame header/payload reader
  footer.rs         # CRC/footer 验证
  metadata.rs       # 只读元数据/随机访问
```

读取层不能知道 frame type 的预测或 DCT 细节，只把合法的 `FramePacket` 提供给分派层。

### 5.3 FrameDispatcher：载荷分派

```text
decoder/frame/
  dispatcher.rs     # frame_type + compression_type 路由
  packet.rs         # FramePacket/FrameDecodeContext
  registry.rs       # 版本化 payload handler 注册
  errors.rs         # 非法组合/不支持类型
```

每个 frame type 通过独立 handler 实现：

```text
decoder/payload/
  golomb.rs
  exp_golomb.rs
  rle_golomb.rs
  rle_cabac.rs
  coefficient.rs
  banded.rs
  planar.rs
  palette.rs
  intrabc.rs
  intra_transform.rs
```

handler 只负责 payload token → 变换前/预测前的中间数据；不负责读取文件头、不负责
序列参考、不负责最终导出。

### 5.4 Reconstruction：重建层

```text
decoder/reconstruct/
  inverse_prediction.rs
  inverse_transform.rs
  inverse_quant.rs
  color_inverse.rs
  reference_restore.rs
  frame_output.rs
```

重建层接收 `DecodedPayload + FrameDecodeContext`，输出 `DecodedFrame`。首帧、golden、
previous 的时间还原必须显式使用 `ReferenceState`，不能从测试或源 PNG 读取补偿数据。

### 5.5 DecodeSession：序列和随机访问

`DecodeSession` 负责：

- 读取并验证 header/index/footer；
- 建立帧范围和版本能力；
- 选择顺序解码或随机访问；
- 管理 reconstructed golden/previous reference；
- 调用 FrameDispatcher 和 Reconstruction；
- 输出 `DecodeResult` 或元数据。

它不包含 Golomb、CABAC、DCT 或 RGB 变换数学实现。

## 6. 当前源码到目标架构的映射

### 6.1 现有 encoder 映射

| 当前模块 | 当前混合职责 | 目标归属 |
|---|---|---|
| `encoder/sequence.rs` | 序列编排、首帧、参考、候选入口、输出 | `encoder/session/*` + `encoder/frame/*` |
| `encoder/streaming.rs` | streaming 生命周期、配置、首帧、payload | `encoder/session/streaming.rs`，复用 session |
| `encoder/frame.rs` | 单帧入口、预测/量化工具 | `encoder/frame/frame_encoder.rs` + tools |
| `encoder/adaptive.rs` | 候选预测、SAD、候选排序、模式信令 | `encoder/frame/candidate.rs` + `core/prediction/cost.rs` |
| `encoder/planar.rs` | planar/CfL 候选和载荷 | `encoder/frame/candidates/planar.rs` |
| `encoder/banded.rs` | 条带候选和编码调度 | `encoder/frame/candidates/banded.rs` |
| `encoder/intrabc.rs` | IntraBC 搜索、决策、载荷 | `core/prediction/intrabc.rs` + encoder/decoder handlers |
| `encoder/intra_probe.rs` | 探针、模式收益统计 | `tools/probe/intra.rs`，不得进入生产路径 |
| `encoder/intra_transform.rs` | 首帧预测后变换候选 | `encoder/frame/candidates/intra_transform.rs` |
| `encoder/dct_path/*` | DCT 平面、矩阵、逆变换混合 | `core/transform/*` + entropy coefficient |
| `encoder/rdoq.rs` | RDOQ/Trellis | `core/transform/rdoq.rs` |
| `encoder/rle_*` | 熵编码状态机和位流 | `core/entropy/*` + encoder writer |
| `encoder/coeff_*` | 系数 token 化/CABAC | `core/entropy/coeff.rs` + side adapters |
| `encoder/ma_tree.rs` | CABAC 上下文决策树 | `core/entropy/context.rs` |
| `encoder/noise.rs` | 噪声估计/软阈值/band step | `core/perceptual/noise.rs` + encoder adapter |
| `encoder/transform.rs` | transform frame candidate | `encoder/frame/candidates/transform.rs` |

迁移时不要求一次性移动所有文件；先建立新接口和适配层，再逐模块替换。旧路径只能在
迁移期保留，不能同时维护两套生产实现。

### 6.2 现有 decoder 映射

| 当前模块 | 当前混合职责 | 目标归属 |
|---|---|---|
| `decoder/mod.rs` | bytes/file 读取、header/index、CRC、frame dispatch、RCT | `decoder/container/*` + `decoder/frame/dispatcher.rs` + `decoder/reconstruct/*` |
| `decoder/golomb.rs` | Golomb payload 解码 | `core/entropy/golomb.rs` + decoder payload |
| `decoder/exp_golomb.rs` | Exp-Golomb payload 解码 | `core/entropy/golomb.rs` + decoder payload |
| `decoder/rle_golomb.rs` | RLE+Golomb payload | `core/entropy/rle.rs` + decoder payload |
| `decoder/rle_cabac.rs` | CABAC/RLE payload | `core/entropy/cabac.rs` + decoder payload |
| `decoder/coeff_cabac.rs` | 系数 CABAC 解码 | `core/entropy/coeff.rs` + decoder payload |
| `decoder/intra_transform.rs` | 首帧预测后变换解码 | decoder payload + core transform inverse |
| `decoder/transform.rs` | transform frame 解码 | decoder payload + core transform inverse |
| `decoder/planar.rs` | 三平面 payload | decoder payload/planar |
| `decoder/banded.rs` | 条带 payload | decoder payload/banded |
| `decoder/palette.rs` | palette payload | decoder payload/palette |
| `decoder/intrabc.rs` | COPY/PRED 重建 | decoder payload/intrabc + reconstruct |
| `decoder/image_export.rs` | 文件导出/像素出口 | app/image adapter，不属于 decoder core |

当前 decoder 直接引用 `crate::crf::encoder::dct_path` 的逆变换代码必须优先解除；这是
层级反向依赖和未来 GPU/CPU 后端分裂的最大风险之一。

源码检查还发现以下反向依赖债务：

- `decoder/rle_cabac.rs` 的生产解码函数接收 `encoder::ma_tree::CtxModel`；上下文模型
  契约必须迁移到公共 `core/entropy/context`；
- `decoder/mod.rs` 的 DCT frame type 直接调用 `encoder::dct_path` 逆变换；必须迁移到
  `core/transform/reconstruct`；
- 部分 decoder 单元测试从 encoder 取 encoder 实现生成测试 payload；测试应迁移到
  `tests/entropy` 或共享 golden-vector helper，不能让生产 decoder 依赖 encoder；
- 任何仅由 `#[cfg(test)]` 引入的 encoder 依赖，应区分为测试夹具依赖，不得被误认为合法
  的生产层级；测试夹具也不能复制生产恢复逻辑。

解除顺序应是“生产依赖先于测试依赖”：先移出 decoder 生产路径的公共数学/上下文类型，
再拆测试夹具，最后删除旧 encoder namespace 转发。

### 6.3 当前 format/transform 映射

| 当前模块 | 目标 |
|---|---|
| `format/types.rs` | `core/domain/*` 与 public API 类型分离 |
| `format/header.rs` | `core/bitstream/header.rs` |
| `format/constants.rs` | `core/bitstream/constants.rs` |
| `format/prediction.rs` | `core/prediction/*`，去除无关公共类型 |
| `format/quant.rs` | `core/config/lossy.rs` + `core/transform/quant.rs` |
| `format/rct.rs` | `core/color/rct.rs` |
| `format/simd.rs` | `backend/cpu/simd_*`；只保留公共 kernel 契约在 core |
| `format/zigzag.rs` | `core/entropy/scan.rs` |
| `transform/dct*.rs` | `core/transform/dct*.rs` |
| `transform/rect.rs` | `core/transform/rect.rs` |

## 7. 关键架构契约

### 7.1 Encode/Decode 不共享高层实现

允许共享：

- domain 类型和范围校验；
- bitstream 头、索引、frame header 的读写契约；
- prediction/transform/entropy 的纯数学定义和测试向量；
- CPU/GPU backend trait；
- 错误码和版本能力。

禁止共享：

- encoder session 直接调用 decoder session；
- decoder 直接调用 encoder private module；
- 通过 `#[cfg(test)]` 或 `pub` 暴露生产内部状态作为跨层捷径；
- 复制一份算法逻辑后在 encoder/decoder 各自修改。

### 7.2 FramePacket 契约

建议统一内部帧包：

```rust
pub struct FramePacket<'a> {
    pub header: FrameHeader,
    pub payload: &'a [u8],
    pub range: ByteRange,
    pub feature_set: FeatureSet,
}
```

编码器输出 `EncodedFrame { header, payload, reconstructed }`，容器层只接收前两项；
解码器输入 `FramePacket`，输出 `DecodedFrame`。`reconstructed` 不得序列化进隐藏字段，
必须明确由 session 保存或释放。

### 7.3 Candidate 与 Payload 契约

```rust
pub struct CandidateResult {
    pub syntax: CandidateSyntax,
    pub distortion: Distortion,
    pub rate_bits: u64,
    pub reconstructed: TileOrFrame,
}
```

候选不得直接写全局文件或修改其他候选状态。RDO 层负责 `D+λR`、硬质量约束和 tie-break；
胜者才交给 payload writer。这样可以替换 CPU/GPU kernel，而不改变格式组装层。

### 7.4 ReferenceState 契约

```rust
pub struct ReferenceState {
    pub golden: Option<ReconstructedFrame>,
    pub previous: Option<ReconstructedFrame>,
    pub anchors: AnchorMap,
}
```

ReferenceState 只保存重建帧，禁止存储对后续解码不可得的原始 frame。首帧有损时必须先
完成 `golden`；后续帧可以并行，但不能绕过该阶段依赖。

### 7.5 BackendKernel 契约

```text
KernelInput(layout, dimensions, bit_depth, plane)
  → KernelOutput(values, costs, optional reconstructed tile)
```

每个 kernel 必须有 scalar、CPU SIMD 和可选 GPU 实现；后端错误由 scheduler 处理，不能
由算法层捕获后静默改变质量语义。

## 8. 分层后的编码/解码流程

### 8.1 编码流程

```text
EncodeRequest
  → resolve config/capability
  → create EncodeSession
  → validate input domain
  → encode frame0 through FrameEncoder
  → local decode/reconstruct G_hat
  → update ReferenceState
  → parallel residual frame sessions
  → candidate RDO
  → winner payload writer
  → ContainerWriter(index + frames + CRC)
  → EncodeReport
```

### 8.2 解码流程

```text
DecodeRequest
  → create ContainerReader
  → validate magic/version/size/CRC
  → read index and bounded FramePacket
  → FrameDispatcher selects payload handler
  → entropy decode
  → inverse transform / prediction / color
  → update ReferenceState
  → DecodeSession temporal restore
  → DecodeResult / random-access frame
```

容器字节解析、payload 语法解码、空间重建和时间参考还原必须是四个可测试阶段。不能在
一个 `decode_frame` 巨型函数中通过大量 `if frame_type` 继续增长。

## 9. 测试分层

```text
tests/
  domain/             # 尺寸、布局、范围、不变量
  bitstream/          # header/index/footer/非法输入/兼容
  prediction/         # 预测与逆预测、边界、重建邻居
  transform/          # DCT/矩形/量化/RDOQ 往返
  entropy/            # token/bitstream/参考向量
  frame/              # 候选、RDO、frame type handler
  sequence/           # golden/previous/anchor/闭环
  backends/           # scalar/SIMD/GPU 对拍
  integration/        # 批量/streaming/文件/随机访问
  benchmark/          # 速度、内存、码率和质量，不作为生产依赖
```

当前 `encoder/tests.rs`、`test/mod.rs` 等历史聚合文件应按上述领域拆分。测试不得重新
实现生产恢复逻辑；测试 helper 只能构造输入、调用公共 API 和断言结果。

## 10. 迁移顺序

### P0：冻结边界，不改变码流

1. 建立目标目录和空的 public traits；
2. 把 `FramePacket`、`ReferenceState`、`ResolvedConfig` 和 `CandidateResult` 写成公共
   契约；
3. 增加依赖检查和源码行数检查；
4. 锁定当前批量、streaming、无损和有损产物基线；
5. 禁止新功能继续进入旧聚合文件。

**验收**：代码行为、旧文件解码和 deterministic 产物不变。

### P1：解除 decoder → encoder 反向依赖

1. 把 `encoder/dct_path` 的公共逆变换迁移到 `core/transform/reconstruct`；
2. decoder 改依赖公共 transform；
3. encoder 继续通过同一公共契约生成正变换；
4. 删除 decoder 对 encoder namespace 的调用；
5. 加入 transform 参考向量和旧 frame type 回归。

**验收**：无损逐像素一致、旧文件全部可解码、无新增码流位。

### P2：拆容器层和 frame dispatch

1. 从 decoder/mod.rs 提取 bytes/file reader、CRC、header/index/footer；
2. 把各 frame type handler 注册到 dispatcher；
3. 统一 bounded reader 和错误类型；
4. 保持 `decode_from_bytes`/`decode_from_file` facade 兼容；
5. 解码器不再在 dispatcher 中执行时间参考恢复。

**验收**：非法输入、截断、未知 frame type、随机访问和 streaming 解码回归。

### P3：拆 encoder session 和 frame pipeline

1. 从 sequence.rs 提取 batch/streaming session；
2. 提取首帧编码、本地重建和 ReferenceState；
3. 提取候选、RDO、payload writer；
4. 让 streaming 复用同一 FrameEncoder；
5. 保持旧 public facade 和参数适配。

**验收**：批量/streaming 语义一致；首帧有损使用 G_hat；旧无损产物不退化。

### P4：整理公共工具层

按领域迁移 prediction、transform、entropy、color 和 config；每迁移一个领域都删除旧
实现或改为纯兼容转发，不得维护两份生产代码。

**验收**：模块依赖无环，单元/参考向量通过，文件行数均低于 800 或有明确拆分计划。

### P5：接入性能 backend

在层级稳定后，按[性能优化规划](performance-optimization-plan.md)接入 scalar → CPU
SIMD/threads → Hybrid GPU。GPU 只能通过 backend trait 进入，不能修改 sequence/decoder
容器层。

### P6：测试和文档收敛

拆分历史大测试文件、更新架构图、公共 API、格式说明和构建门禁；删除一次性迁移适配器。

## 11. 兼容与失败回退

- 对外 `encode_sequence`、`decode_from_bytes`、`decode_from_file` 在迁移期保留；内部
  立即转发到新 facade，不能复制旧实现；
- bitstream version 不因目录重构改变；
- 任何新 frame type 仍必须走格式版本/feature bit 流程；
- 新 backend/新工具不可用时由 scheduler 回退，不让 decoder 猜测；
- 迁移阶段发现行为差异，优先保留旧 facade 的正确性，再定位层级错误；
- 禁止以“架构重构”为理由同时改质量预设、量化语义和格式位。

## 12. 1000 行与单一职责门禁

架构重构不能通过拆文件绕过职责要求：

- 任何手写文件 `>1000` 行，禁止构建、合并和发布；
- `>=800` 行只能修复、拆分和清理，不得继续加入新功能；
- 目标目录中的 `mod.rs` 只做模块导出和少量 facade，不承载算法；
- 每个文件必须能用一句话说明职责；
- encoder、decoder、container、transform、entropy、backend 和 tests 不得混在一起；
- 新增 GPU 后端必须独立文件/feature，不得把厂商 API 写入公共层；
- 测试按 domain/bitstream/prediction/transform/entropy/frame/sequence/backend/integration
  拆分，禁止继续扩大历史聚合测试文件。

## 13. 架构验收标准

重构阶段完成的最低条件：

1. encoder 和 decoder 通过公共 facade 使用，外部 API 行为兼容；
2. decoder 不依赖 encoder 私有模块；
3. 容器读取、payload 解码、空间重建、时间参考四层可独立测试；
4. 编码 sequence、frame candidate、RDO、payload writer 职责分离；
5. batch/streaming 复用同一配置和 FrameEncoder/ReferenceState；
6. CPU scalar/SIMD/GPU 只通过 backend 契约接入；
7. 无损逐像素、旧文件、非法输入、deterministic 和随机访问回归通过；
8. 有损使用 decoded reconstructed reference，质量/码率数据无源 golden 误导；
9. 所有手写源文件不超过 1000 行，800 行文件完成拆分或没有继续增长；
10. 文档、依赖图、模块职责和迁移记录与真实代码一致。

## 14. 本轮边界

本文档最初只定义目标架构、模块职责、迁移顺序和验收标准。截至 2026-09-03，迁移已进入
实施阶段（见文首进度同步）：P0~P2 完成、P3~P5 部分完成、P6 测试子项完成。迁移全程保持
CRF 码流逐字节不变、无损逐像素正确、deterministic 语义不变，且不引入任何 GPU/CPU 依赖。
