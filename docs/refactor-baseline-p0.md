# P0 基线锁定记录

**锁定日期**：2026-08-26  
**基线 commit**：`408adbc`（perf(P6): CABAC 候选预筛收尾 + banded golden 标志位修复 + batch 内存预检）  
**规划文档**：[codec-architecture-refactor-plan.md](codec-architecture-refactor-plan.md) §10 P0

## 1. 基线目的

P0 阶段冻结边界，不改变码流。本文件记录迁移前的基线状态，作为后续 P1~P6 阶段的
回归参照。任何阶段完成后都应与本基线对比，确认：

- 代码行为不变；
- 旧文件解码不变；
- deterministic 产物不变。

## 2. 现有源码结构基线

### 2.1 顶层模块（`src-tauri/src/crf/`）

| 模块 | 文件数 | 说明 |
|---|---|---|
| `mod.rs` | 1 | 模块入口与快捷 API（115 行） |
| `checksum.rs` | 1 | CRC32（4517 字节） |
| `error.rs` | 1 | 错误类型（97 行） |
| `format/` | 10 | 公共类型/常量/header/prediction/quant/rct/simd/zigzag |
| `transform/` | 4 | DCT4/DCT8/rect 变换（独立顶层目录） |
| `encoder/` | 25 | 编码器全部实现（含 dct_path 子目录） |
| `decoder/` | 14 | 解码器全部实现 |

### 2.2 已知规模预警（commit 408adbc 时）

| 文件 | 行数 | 状态 |
|---|---|---|
| `src-tauri/src/test/mod.rs` | ~1030 | **超过 1000 行硬限制** |
| `src-tauri/src/crf/encoder/tests.rs` | ~980 | 预警区（>=800） |
| `src-tauri/src/crf/format/prediction.rs` | ~801 | 预警区（>=800） |

> 注：上述预警文件在 P0 阶段不拆分（P0 只冻结边界）。P6 测试收敛阶段统一处理。

## 3. 关键依赖债务（P1 优先处理）

### 3.1 decoder → encoder 反向依赖

| 依赖位置 | 依赖对象 | 类型 | 处理阶段 |
|---|---|---|---|
| `decoder/rle_cabac.rs:174` | `crate::crf::encoder::ma_tree::CtxModel` | 生产依赖 | P1 |
| `decoder/transform.rs:110` (test) | `crate::crf::encoder::transform::encode_frame_transform` | 测试夹具 | P1 后 |

### 3.2 混合职责文件

| 文件 | 混合职责 | 目标拆分 |
|---|---|---|
| `decoder/mod.rs` (18645 字节) | 容器读取 + header/index + CRC + frame dispatch + RCT | P2 拆为 container/frame/reconstruct |
| `encoder/sequence.rs` (24437 字节) | 序列编排 + 首帧 + 参考 + 候选 + 输出 | P3 拆为 session/frame |
| `format/prediction.rs` (~801 行) | 预测数学 + 公共类型混合 | P4 迁移到 core/prediction |

## 4. 测试基线

### 4.1 测试套件

P0 基线的测试通过状态（commit 408adbc）：

- `cargo test` 全部通过（含 `#[cfg(test)]` 内联测试）；
- 无损往返：PNG1000 14 帧逐像素校验通过；
- 旧文件解码：v1.0~v1.14 全版本码流可解码；
- streaming 路径：与批量编码逐字节一致。

### 4.2 基线锁定方式

P0 不增加新测试代码（避免触碰旧聚合文件）。基线锁定方式为：

1. 记录 `cargo test` 通过状态（本文件 §4.1）；
2. P0 完成后立即运行 `cargo test`，确认与基线一致；
3. P1 起各阶段完成后运行 `cargo test`，确认无回归。

### 4.3 golden vector（P6 接入）

P6 测试收敛阶段建立 golden vector 目录 `tests/golden/`，包含：

- 各 frame_type 的参考码流；
- 标量/CPU SIMD 对拍向量；
- 非法输入/截断/随机访问回归用例。

## 5. 码流格式基线

### 5.1 当前格式版本

- bitstream version：v1.14（`VERSION_MAJOR=1`, `VERSION_MINOR=3`）
- P0 不改变码流格式版本。

### 5.2 frame type 清单（v1.14）

| frame_type | 语义 | 状态 |
|---|---|---|
| 0 | 块级自适应 k | 已实现 |
| 1 | RLE+Golomb | 已实现 |
| 2 | 条带级自适应（banded） | 已实现 |
| 3 | 三平面打包（planar） | 已实现 |
| 4 | 调色板（palette） | 已实现 |
| 5 | IntraBC（COPY/PRED） | 已实现 |
| 6 | DCT 变换域量化 | 已实现 |
| 7 | (保留) | — |
| 8 | 预测后变换 + CABAC 系数编码 | 已实现（v1.14） |

## 6. P0 完成标准

P0 阶段完成的最低条件（规划文档 §10 P0 验收）：

- [x] 建立目标目录和空的 public traits；
- [x] 把 `FramePacket`、`ReferenceState`、`ResolvedConfig` 和 `CandidateResult` 写成公共契约；
- [x] 增加依赖检查和源码行数检查；
- [x] 锁定当前批量、streaming、无损和有损产物基线（本文件）；
- [x] 禁止新功能继续进入旧聚合文件（门禁脚本执行）；
- [x] 代码行为、旧文件解码和 deterministic 产物不变（`cargo test` 通过）。

## 7. 后续阶段预告

| 阶段 | 目标 | 触及文件 |
|---|---|---|
| P1 | 解除 decoder → encoder 反向依赖 | `encoder/ma_tree.rs` → `core/entropy/context.rs`；`decoder/rle_cabac.rs` |
| P2 | 拆容器层和 frame dispatch | `decoder/mod.rs` → `decoder/container/*` + `decoder/frame/*` |
| P3 | 拆 encoder session 和 frame pipeline | `encoder/sequence.rs` → `encoder/session/*` + `encoder/frame/*` |
| P4 | 整理公共工具层 | `format/*` → `core/*` 各子模块 |
| P5 | 接入性能 backend | `backend/scalar` + `backend/cpu` + `backend/gpu` |
| P6 | 测试和文档收敛 | `tests/*` 按领域拆分 |
