# CRF 优化已完成项清单（代码进度同步）

**同步日期**：2026-09-06
**基线版本**：v1.15 标定轮（含 P0~P6 + frame_type=8 正式格式化 + P4.2/P4.3/P4.4 activity masking 接入 + S0/S1 探针）
**核对方式**：codegraph + 关键文件 Read + optimization-review.md §9~§28 实施记录交叉验证
**关联文档**：
- [首帧优化规划](first-frame-optimization-plan.md)（本文件标注其已完成项，未完成项仍留在原文件）
- [有损参数接口规划](lossy-tuning-interface-plan.md)（同上）
- [优化评审记录](optimization-review.md)（各轮实施细节记录）

本文档汇总两份规划文档中**已落地到代码**的项，每项标注代码位置与实施记录章节，供后续工作避免重复投入。未完成项仍保留在原规划文档中。

---

## 一、总览

| 规划阶段 | 总项数 | 已完成 | 部分完成 | 否决/关闭 | 未完成 |
|---|---:|---:|---:|---:|---:|
| 首帧规划 P0 闭环语义 | 6 | 6 | 0 | 0 | 0 |
| 首帧规划 P1 标定 | 6 | 3 | 1 | 1 | 1 |
| 首帧规划 P2 intra 路径 | 4 | 3 | 0 | 1（四叉树）| 0 |
| 首帧规划 P3 系数熵编码 | 7 | 2 | 0 | 1（DC/AC 分离）| 4 |
| 首帧规划 P4 感知量化 | 7 | 4 | 0 | 2（P4.6/P4.7） | 1（P4.1 后置） |
| 首帧规划 P5 序列优化 | 7 | 7 | 0 | 0 | 0 |
| 首帧规划 P6 速度内存 | 12 | 10 | 0 | 0 | 2 |
| 接口规划 V2 | 7 | 5 | 1 | 1 | 0 |
| **合计** | **56** | **40** | **2** | **6** | **8** |

> "部分完成"指阶段内部分子项落地；"否决/关闭"指经实测或架构判定不做。

---

## 二、首帧优化规划已完成项

### P0：闭环语义修正（全部完成 ✅）

规划来源：[first-frame-optimization-plan.md §5-P0](first-frame-optimization-plan.md)
实施记录：[optimization-review.md §9](optimization-review.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 |
|---|---|---|---|---|
| P0.1 | avif.py 基准合同固定 | `E:\multimedia-compress\avif.py` 复现路径 | §19 AVIF CQ18 全量复测 | ✅ |
| P0.2 | 产生 G_hat，后续帧以 G_hat 为参考 | `encoder/sequence.rs` 路径 G L300 `decode_frame` 本地重建 | §9 P0 两阶段闭环 | ✅ |
| P0.3 | 批量/streaming/文件解码统一用 decoded frame0 恢复 | `test/mod.rs` verify 改用 CRF 解码 frame0 | §9 P0 修复历史缺陷 | ✅ |
| P0.4 | 单独报告首帧误差与残差误差 | `encoder/closed_loop_tests.rs` `p0_lossy_golden_no_reference_drift` | §9 P0-4 误差分解 | ✅ |
| P0.5 | 锁定无损管线回归 | `cargo test` 117→124 passed | §9 全量回归 | ✅ |
| P0.6 | streaming 首帧有损闭环（G_hat 对齐批量） | `encoder/streaming.rs` 首帧 push 本地重建 G_hat + header.lossy_quant 提前冻结 | §29 | ✅ 有损 golden 下 batch/streaming 逐字节一致（3 项新测试） |

**关键不变量**：无损 golden 时 `G_hat == frames[0]`，产物与旧实现逐字节一致；有损 golden 时误差不再向后续帧传导。

---

### P1：目标预设标定（部分完成）

规划来源：[first-frame-optimization-plan.md §5-P1](first-frame-optimization-plan.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 | 说明 |
|---|---|---|---|---|---|
| P1.1 | Q_target 允许 frame0 有损 | — | §16 标定扫描 | ❌ 否决 | golden_lossless=false 在当前架构为纯负优化（首帧误差注入残差暴增），保持默认 true |
| P1.2 | 色度 2×2 半分辨率解耦 | `format/quant.rs` `chroma_half_res`；`encoder/sequence.rs` L177；planar `ss_flags.bit0` | §10 P1a | ✅ | q95 step=1 首次真实启用 4:2:0 |
| P1.3 | 分离 luma/chroma 步长，解决 130% 截断 | `format/quant.rs` L91 `chroma_step` 升级逻辑 | §10 P1a | ✅ | Q=1×130%→floor=1 时升一级使百分比生效 |
| P1.4 | 亮度/色度低高频矩阵候选 | — | §11.1 修正 | ❌ 后置 | DCT(type6) 全档零胜出，矩阵工作后置到 P2 落地之后 |
| P1.5 | 联合搜索 deadzone/矩阵/色度/RDOQ λ | `format/quant.rs` `deadzone_bias`/`chroma_deadzone_bias` | §12/§13/§17 | ◐ 部分 | deadzone 标定完成（luma +4 / chroma -4）；矩阵/RDOQ λ 未做 |
| P1.6 | 固定目标率失真点 + Q95/Q96/Q97 编号 | — | §19 + §26 视觉标定 + 单调性验收 | ✅ | 视觉标定→Q_target=q90；DAT.1 跨内容单调性验收通过 |

**P1 色度死区偏置标定**（P1.5 子项，已闭环）：
- `LossyTuning.chroma_deadzone_bias: Option<i8>`（`format/quant.rs`）默认 `Some(-4)`
- `LossyTuning.deadzone_bias: i8` 默认 `4`
- 19 组扩展数据集标定（§13），q90 多组 −5~11% 且质量提升
- 符号语义勘误：批量 `quantize_residuals_tuned` 与闭环 `quant_scalar_biased` 的 bias 符号约定相反（§12）

---

### P2：首帧专用 intra 路径（探针 + 正式格式化完成 ✅）

规划来源：[first-frame-optimization-plan.md §5-P2](first-frame-optimization-plan.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 |
|---|---|---|---|---|
| P2.1 | 固定 tile 预测后变换探针 | `encoder/intra_probe.rs` 347 行 | §14 P2v1 / §15 P2v2 / §22 P2v4 | ✅ 探针完成 |
| P2.2 | transform skip（DC 模式直通） | `encoder/intra_transform.rs` L78-92；`decoder/intra_transform.rs` L126-130 | §22 突破 | ✅ |
| P2.3 | 正式格式化 frame_type=8 | `encoder/intra_transform.rs` 266 行 + `decoder/intra_transform.rs` 231 行 + `encoder/coeff_cabac.rs` 83 行 + `encoder/intra_transform_tests.rs` | README v1.14 + adaptive.rs 5 caller + decoder/mod.rs 3 caller | ✅ 已接入竞争 |
| P2.4 | 递归四叉树 | — | §13 R13 探针 | ❌ 关闭 | 水平二分探针 −0.46%/−0.25%，条带头翻倍侵蚀收益，完整四叉树不做 |

**frame_type=8 载荷布局**（P2.3 代码位置 `encoder/intra_transform.rs` L1-11）：
```
[flags u8][len_y u32 LE][y_payload][len_co u32 LE][co_payload][len_cg u32 LE][cg_payload]
每子载荷 = [mode_len u32 LE][k u8][mode_body(CABAC)][coeff_stream(CoeffCABAC)]
```

**探针关键结论**：
- P2v1（通用 CABAC 承载系数）负结果：+55~209%（§14）
- P2v2（EOB+zigzag）负结果：位置流开销 > 截断收益（§15）
- P3 run-level 嵌入式突破：q90 帧1/帧2 接近持平（§21）
- P2v4 transform skip 突破：q90 帧2 −17.8% 超越自适应管线（§22）

---

### P3：变换系数专用熵编码（最小版完成，深化未做）

规划来源：[first-frame-optimization-plan.md §5-P3](first-frame-optimization-plan.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 |
|---|---|---|---|---|
| P3.1 | 每块 EOB + 固定 zigzag | `encoder/coeff_cabac.rs` run-level 嵌入式（run 即零串，无单独位置流） | §21 突破 | ✅ |
| P3.2 | 方向扫描（H/V 预测对应） | — | — | ❌ 未完成 |
| P3.3 | significance map + 末端长零串 | `coeff_cabac.rs` run 截断一元（隐式处理长零串） | §21 | ✅ 隐式 |
| P3.4 | DC 邻块差分 + AC magnitude/sign 分离 | — | §21 DC/AC 分离实测 | ❌ 回退 | DC 主导内容下额外开销 > 收益 |
| P3.5 | plane/transform size/mode/邻块上下文 | — | — | ❌ 未完成 | 当前 CoeffCABAC 仅 4 上下文 |
| P3.6 | 小系数 ±1/±2 短码 + 逃逸码 | — | — | ❌ 未完成 |
| P3.7 | tile restart/长度设计 | — | — | ❌ 未完成 |

**CoeffCABAC 当前上下文模型**（`encoder/coeff_cabac.rs` L17-22）：
- `ctx_nonzero`：块是否有非零系数
- `ctx_run`：run 截断一元每个 bit
- `ctx_level_q`：level 商前缀每个 bit
- sign：等概率直通（`encode_direct`）

---

### P4：感知量化与内容保护（部分完成）

规划来源：[first-frame-optimization-plan.md §5-P4](first-frame-optimization-plan.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 |
|---|---|---|---|---|---|
| P4.1 | luma/chroma 独立矩阵 | — | §11.1 | ❌ 后置（DCT 零胜出）|
| P4.2 | activity masking | `core/perceptual/noise.rs` `estimate_band_activity_steps` + V2 `perceptual.activityMaskingX100`（batch/streaming 双接入） | §28 前置 | ✅ 内核已接入（默认 100 中性；⚠️ 分类 reference 整数除法失效，见新待办）|
| P4.3 | 平坦区防 banding | 同上 `flatAreaProtectionX100` | §28 前置 | ✅ 内核已接入（同上）|
| P4.4 | 线稿/锐边/文字保护 | 同上 `edgeProtectionX100` | §28 前置 | ✅ 内核已接入（同上）|
| P4.5 | 饱和色边防 bleeding | 色度下采样中值替代均值（边缘感知）| §20 P4 | ✅ |
| P4.6 | RDOQ λ 与 Q_target 共同标定 | `core/transform/rdoq.rs` λ 参数化能力保留 | §28 S1 探针 | ❌ 冻结（DCT 零胜出根因不在 λ，λ 扫描整体收益 ≈0.9%）|
| P4.7 | 去振铃/去块后处理 | — | §28 S0 探针 | ❌ 冻结（ringing 信号 ⊆ edge 信号，非独立）|

---

### P5：序列优化（基础实现）

规划来源：[first-frame-optimization-plan.md §5-P5](first-frame-optimization-plan.md)

| # | 规划项 | 状态 | 阻塞原因 |
|---|---|---|---|
| P5.1 | 参考竞争 golden/previous | ✅ | `LossyTuning.reference_mode`；仅引用编码端重建帧，按帧大小竞争 |
| P5.2 | 稀疏变化 mask | ✅ | `encoder/sequence_tools.rs` tile mask，静止 tile 残差置零 |
| P5.3 | 轻量位移补偿 | ✅ | `search_integer_motion` 小范围整数 SAD 搜索工具 |
| P5.4 | 残差专用工具 | ✅ | `sparsify_residual` 与现有 RDOQ/CABAC 候选复用 |
| P5.5 | 场景切换/新 anchor | ✅ | `scene_cut` 阈值检测，切换时回退 golden anchor 语义 |
| P5.6 | 序列码率控制 | ✅ | `RateControl` 目标/上限校验与复杂度步长分配工具 |
| P5.7 | 局部 RGB palette | ✅ | 复用 adaptive frame_type=4 palette 候选（可由 `PaletteMode` 配置） |

---

### P6：速度与内存（部分完成）

规划来源：[first-frame-optimization-plan.md §5-P6](first-frame-optimization-plan.md)

| # | 规划项 | 代码位置 | 实施记录 | 状态 |
|---|---|---|---|---|
| P6.1 | 候选剪枝（DCT 预筛） | `encoder/adaptive.rs` DCT 候选段预检 | §18 | ✅ | q90 −10.7% 耗时+体积+质量三赢 |
| P6.2 | 候选剪枝（CABAC+frame_type=8 预筛） | `encoder/adaptive.rs` avg_abs_res 共享指标 | §23 | ✅ | 平坦内容显著跳过 |
| P6.3 | Fast-Fail 试编码短路 | `encoder/frame.rs` L86 `encode_frame_inner_limited` + `rle_golomb.rs`/`rle_cabac.rs` `*_limited` | §8 R8/S1 | ✅ | 码流零改动 |
| P6.4 | SIMD（差分/YCoCg-R/软阈值） | `format/simd.rs` AVX2 运行时分派 | R1（§一.1）| ✅ | CRC32 走 crc32fast PCLMULQDQ |
| P6.5 | 熵编码批量位写入 | `encoder/golomb.rs` L70 `write_bits_msb` + `rle_golomb.rs` + `exp_golomb.rs` | §8 R14/S6 | ✅ | 码流逐字节一致 |
| P6.6 | 线程级并行 | rayon `par_iter`（SAD/残差帧）| R5 | ✅ | SAD 决策 8 模式并行 |
| P6.7 | SATD 预筛 | `format/cost.rs` 4×4 Hadamard SATD + 1/4 网格采样 | CPU 性能分支 | ✅ | 替代自适应与 IntraBC 的行采样 SAD 排序 |
| P6.8 | quant_scalar 批量 SIMD | `backend/cpu/simd.rs` AVX2 4-lane f64 精确除法 + scalar 极值/尾部回退 | §23 未执行项 | ✅ | frame_type=8 skip/DCT 每块 64 系数批量量化 |
| P6.9 | 帧/条带 Scratch Buffer | `encoder/scratch.rs` + `FrameScratch`/Rayon `map_init` 条带缓冲 | §23 未执行项 / 性能规划 §4.4 | ✅ | 帧候选复用整帧 residual/recon；条带任务复用 8 候选容量，无全局锁 |
| P6.10 | DCT 块级栈缓冲 | `core/transform/{dct4,dct8,rect,reconstruct}.rs` + 编解码 DCT 热路径 | 性能规划 §4.4 | ✅ | 新增兼容 `*_into` 内核；4×4/8×8/矩形正逆变换热路径每块零堆分配，码流不变 |
| P6.11 | palette 像素级抽样预筛 | `encoder/frame/candidate.rs` `palette_plausible` | optimization-review §39 | ✅ | palette span −33%（30.7s→20.5s）；bytes 逐字节不变；探针 `--probe-palette` 保留 |
| — | SIMD 预测泛化 | — | §6.2 S3 | ❌ 中长期 | Med/Paeth 分支向量化复杂 |
| — | 内存池零分配 | — | §6.2 S4 | ❌ 暂缓 | 待性能剖析后定向 |

---

## 三、有损参数接口规划已完成项

规划来源：[lossy-tuning-interface-plan.md](lossy-tuning-interface-plan.md)
实施记录：[optimization-review.md §10/§12/§17](optimization-review.md)

> **V2 已实施**：`EncodeParams.lossy: Option<LossyOptionsV2>` 与旧 V1 字段并存于兼容期，
> 但严格互斥；旧请求经 legacy revision 适配后进入相同 resolved/编码路径。核心实现位于
> `core/config/lossy_v2/`，用户入口位于 `codec/config.rs` 与 `cli_config.rs`。
> ```rust
> pub struct EncodeParams {
>     pub lossy: Option<LossyOptionsV2>,      // V2
>     pub lossy_quality: Option<u8>,        // V1 质量档位
>     pub lossy_tuning: Option<LossyTuning>, // V1 精细参数
> }
> ```

### V1 字段扩展（已完成）

| 规划字段 | V1 实际字段 | 代码位置 | 默认值 | 实施记录 |
|---|---|---|---|---|
| `quant.deadzone_chroma` | `LossyTuning.chroma_deadzone_bias: Option<i8>` | `format/quant.rs` | `Some(-4)` | §12/§13 |
| `quant.deadzone_luma` | `LossyTuning.deadzone_bias: i8` | `format/quant.rs` | `4` | §17 |
| `chroma.sampling` 解耦 | `LossyTuning.chroma_half_res: bool`（不再受 step>1 阻断）| `encoder/sequence.rs` L177 | `true` | §10 P1a |
| `quant.chroma_step` 升级 | `LossyTuning.chroma_step(luma_q)` 升级逻辑 | `format/quant.rs` L91 | 130% | §10 P1a |

### V1 既有字段（规划前已存在，未升级到 V2 语义）

| 字段 | 当前默认 | 已知问题（规划 §2 指出，未修复）|
|---|---|---|
| `chroma_quant_percent` | 130 | Q=1 截断（已由 chroma_step 升级部分缓解）|
| `keyframe_interval` | 10 | u8 范围限制；注释混旧链式语义 |
| `anchor_quality_percent` | 100 | 与 `golden_lossless` 语义重叠 |
| `noise_adaptive` | false | 只有开/关，无 Auto 策略 |
| `noise_tau_x100` | 150 | 关闭时静默忽略 |
| `golden_lossless` | true | §16 实测放开为纯负优化，保持默认 |

### 接口规划 §13 实施顺序核对

| # | 规划项 | 状态 | 说明 |
|---|---|---|---|
| 1 | 冻结 V2 字段命名 | ✅ | `core/config/lossy_v2/types.rs` |
| 2 | `resolve_without_encoding()` | ✅ | 含 warnings/ignored/fingerprint |
| 3 | 旧接口兼容适配器 | ✅ | legacy revision 0；核心控制往返测试锁定 |
| 4 | 批量/streaming 共用解析 | ✅ | `EncodeParams::normalize_lossy()` 为唯一内核映射入口 |
| 5 | P0 闭环后开放 first_frame != Lossless | ❌ 否决 | P0✅，但 §16 实测放开为纯负优化，保持 Lossless |
| 6 | 逐组接入参数 | ◐ 部分 | V2 控制面完整；现有内核可表达项已适配，新增算法字段继续分阶段接入 |
| 7 | 公开实验工具命名空间 + UI 专家面板 | ✅ | 严格 JSON + CLI + `expert_panel_schema()`；前端源码尚不存在 |

---

## 四、评审记录各轮已完成项汇总

来源：[optimization-review.md](optimization-review.md) §四 R1~R7 + §8~§23

### R1~R7（v1.10~v1.11 循环轮）

| 轮次 | 项目 | 代码位置 | 收益 |
|---|---|---|---|
| R1 | 多尺寸变换核 8×8 lifting DCT | `transform/{mod,dct4,dct8}.rs` | q90 −26.9%（§四 v1.10）|
| R2 | RDOQ/Trellis | `encoder/rdoq.rs` | 矩阵路径胜者再竞争 |
| R3 | IntraBC 帧内块复制 | `encoder/intrabc.rs` + `decoder/intrabc.rs` | 首帧重复花纹 |
| R4 | IntraBC 差分帧启用 | `encoder/intrabc.rs` 因果完备性修复 | 全场景安全 |
| R5 | SAD rayon 并行 + MA 第四属性 \|top_right\| | `encoder/adaptive.rs` + `ma_tree.rs` | 速度 + 边际 |
| R6 | 噪声感知 A/B 实验 | — | ❌ 零收益证伪，规范 §7 首条关闭 |
| R7 | 无损 DCT(Q=1) 候选放开 | `encoder/adaptive.rs` type=6 条件 | 能力保留，纹理内容自动受益 |

### v1.11 第二批评判实施项（§六）

| 项 | 代码位置 | 实施记录 |
|---|---|---|
| S1 Fast-Fail 试编码短路 | `rle_golomb.rs`/`rle_cabac.rs`/`frame.rs`/`adaptive.rs` `*_limited` | §8 R8 |
| S6 熵编码批量位写入 | `golomb.rs`/`rle_golomb.rs`/`exp_golomb.rs` `write_bits_msb` | §8 R14 |

### v1.13 首帧专项（R11~R13）

| 项 | 代码位置 | 实施记录 |
|---|---|---|
| RCT 首帧自适应（flags.bit3） | `format/header.rs` `first_frame_no_rct` + `encoder/sequence.rs` 双路竞争 | §12 R12 |
| 无损 DCT 矩形补齐（{8×4,4×8} flat） | `encoder/dct_path` variants 表 | §13 R13① |
| 多参考行预测 V2/H2（pred_mode 9/10） | `format/prediction.rs` | §13 R13③ |
| 超块分区先导验证 | `banded.rs` `probe_band_split_savings` | §13 R13② 数据驱动关闭 |

### v1.15 有损标定轮（§9~§23）

| 轮次 | 项目 | 代码位置 | 收益 |
|---|---|---|---|
| P0 | 有损 golden 闭环参考语义 | `encoder/sequence.rs` 路径 G 两阶段 + `test/mod.rs` + `closed_loop_tests.rs` | 正确性修复 |
| P1a | 色度解耦 | `format/quant.rs` chroma_step + `encoder/sequence.rs` chroma_half_res | q95 −47.2%（§11）|
| P1b | planar 色度死区偏置通道 | `format/quant.rs` `chroma_deadzone_bias` + `encoder/frame.rs` `FrameQuant.chroma_bias` + `encoder/planar.rs` | q95 −9.4%（§12）|
| P1c | chroma_deadzone 默认 Some(-4) | `format/quant.rs` default | 19 组 −2.38% 均值（§13）|
| P1d | CfL α 候选扩展（±3） | `encoder/planar.rs` | 跨内容 −4~24%（§20）|
| P1e | 色度 band steps（Y 表下采样映射） | `encoder/noise.rs` | JPEG 源 −36%（§20）|
| P1f | 亮度 deadzone 标定 +4 | `format/quant.rs` default | q90 −5~11%（§17）|
| P4 | 边缘感知下采样（中值替代均值） | 色度下采样路径 | 防渗色（§20）|
| P6 | DCT 候选预筛 + CABAC/frame_type=8 预筛 | `encoder/adaptive.rs` | q90 −10.7% 三赢（§18）+ 平坦内容跳过（§23）|
| 基准 | AVIF CQ18 全量复测 | — | CRF q90 = 103.8% AVIF @ +12.5dB（§19）|
| 预设 | 默认值固化 | `LossyTuning::default()` | None==Some(default) 逐字节（§20）|
| P2v1~v4 | 变换域探针 | `encoder/intra_probe.rs` | v4 transform skip 突破（§22）|
| P3 | 专用系数熵编码器 | `encoder/coeff_cabac.rs` + `decoder/coeff_cabac.rs` | run-level 嵌入式（§21）|
| frame_type=8 | 正式格式化 | `encoder/intra_transform.rs` + `decoder/intra_transform.rs` + `encoder/intra_transform_tests.rs` | 接入 adaptive 竞争 |

### v1.15 后置探针轮（§28，2026-09-06）

| 轮次 | 项目 | 代码位置 | 结论 |
|---|---|---|---|
| S0 | ringing 信号独立性探针 | `performance/probe_ringing.rs`（`--probe-ringing`） | P4.7 证伪：ringing ⊆ edge，非独立信号 |
| S1 | RDOQ λ 敏感性探针 | `performance/probe_lambda.rs`（`--probe-lambda`）+ `core/transform/rdoq.rs` λ 参数化 | P4.6 冻结：DCT 零胜出根因不在 λ；附带证伪「λ 两档竞争」待办 |
| P4.2~P4.4 | activity masking 三旋钮接入 | `core/perceptual/noise.rs` + batch/streaming 双路径 | ✅ 已接入；⚠️ reference 整数除法失效待修复 |

---

## 五、关键否决/关闭决策（避免重复投入）

| 决策 | 来源 | 理由 |
|---|---|---|---|
| **P4.7 ringing_control 第四分类** | §28 S0 探针 | Laplacian 高响应条带（ringing 风险信号）在真实二次元差分数据中罕见（0~4%），且出现时 100% 落在 edge 信号覆盖内——ringing 非独立信号，不值得第四分类内核；`ringing_control_x100` 冻结为「配置已定义、内核不消费」 |
| **P4.6 RDOQ λ 标定** | §28 S1 探针 | λ∈[×0.25,×4] 全部扫描点 DCT+Trellis 体积恒大于 adaptive 胜出者；仅 ×0.1 极端档 3/13 帧小胜（整体收益 ≈0.9% < 3% 门槛）——DCT 零胜出根因不在 λ；`rdo_lambda_scale_x1000` 冻结（λ 参数化能力保留待未来重评） |
| **Trellis λ 两档竞争（850/3400）待办** | §28 S1 探针 | λ≥×0.5 后 Trellis 输出饱和：425/850/1700/3400 四档产物体积完全相同，竞争无意义，从待办移除 |
| golden_lossless=false 不放开 | §16 | 首帧误差注入残差能量暴增，多数组体积+质量双输；依赖 P2 预测后变换才能成立 |
| 完整四叉树不做 | §13 R13 | 水平二分探针 −0.46%/−0.25%，条带头翻倍侵蚀收益，增量趋零 |
| 量化矩阵工作后置 | §11.1 | DCT(type6) 全档零胜出，矩阵标定前提（变换域有竞争力）不成立 |
| DC/AC 分离回退 | §21 | DC 主导内容下额外开销 > 收益 |
| EOB 位置流回退 | §15 | 位置流信令开销 > 末尾零截断收益 |
| 噪声感知自动启用证伪 | §6 R6 | 三组实测零收益（JPEG 噪声 P25 低于步长门限）|
| Trellis λ 多档竞争 | R5 | 偏向小 λ 使 PSNR 失控，与质量档位语义冲突 |
| 首帧专用 CABAC 模型 | R11#4 | 前提错误（MA 树已逐帧独立训练）|
| 内容感知 CABAC 上下文分组 | §6.1#2 | 与 MA 树功能重叠 |
| 量化步长帧级自适应 | §6.1#6 | 与 golden 统一质量语义冲突 |
| 分层比特流随机访问 type=8（旧提案）| §6.1#7 | 伪需求（帧级 O(1) 已解决）|
| ML 类全部 | §6.1#8/H14 | 项目规则禁止 |
| **CoeffCABAC 方向扫描（P3.2）** | §30 合成内容探针 | 零收益——SAD 自适应预测后残差能量已集中低频，zigzag 本就是 8×8 DCT 最优扫描；freq_x/freq_y 主序扫描只重排系数，run-level 总字节不变 |
| **CoeffCABAC 邻块上下文（P3.5）** | §30 合成内容探针 | ctx_nonzero 单 bit 熵低、单槽位自适应已逼近下限；4 槽位需"邻块 EOB 强相关"而合成内容未提供，唯一微弱信号（水平条纹 −1.3~−1.5%）远低于 3% 门槛 |
| **activity_masking 纹理掩蔽（P4.2）** | §31 三旋钮标定 | +3.87% 负收益——band_steps None→Some 路径切换代价 + activity 增步长 delta 整数除法≈0（g−ref 仅 1~3），「省码率」从未发生 |
| **flat_area_protection 平坦防 banding（P4.3）** | §31 三旋钮标定 | +3.87% 负收益——减步长保质量但基线已无 banding 可保护（PSNR +0.006dB），体积白增 |
| **edge_protection 边缘保护（P4.4）** | §31 三旋钮标定 | +6.30% 负收益——减步长但基线已无 ringing 可保护（PSNR +0.009dB，印证 §28 S0 ringing⊆edge），体积白增 |
| **首帧 RGB 直通内容门控跳过** | §34 首帧 RCT 双路探针 | 直通胜出 3/24（12.5%），但胜出组 G 零值占比 0.000~0.001——「G 恒零才直通」假设不成立（真实原因是 RGB 通道低相关），G 零值无法预测胜出；且 1000 组 bypass 仅比 rct 大 4.4%，Fast-Fail 上限过松。首帧 RCT 双路竞争维持现状 |
| **banded 64 行档剪枝** | §35 banded 条带高度探针 | 64 行档胜出 15/44（34.1%）、累计省 9639 字节——64 行档有真实字节收益（印证 A4 条带高度自适应），剪枝会损失 34% 帧的收益；32/64 两档竞争维持现状 |

---

## 六、未完成项指向

已完成项见本文件。未完成项仍保留在原规划文档：

- **首帧优化未完成项**：[first-frame-optimization-plan.md](first-frame-optimization-plan.md) P1.4、P3.2/P3.5~P3.7、P4.1、P4.6（已冻结，见否决表）、P4.7（已冻结，见否决表）、P6 预测 SIMD 泛化/全局内存池
- **activity 分类框架修复（✅ 已闭环，§28）**：`estimate_band_activity_steps` 的 reference 整数除法稀疏差分场景退化缺陷已修复（`.max(1)` + `test_activity_steps_sparse_diff_reference_activates` 回归单测）；默认旋钮 100 中性下产物逐字节不变（函数不被调用）。三旋钮（P4.2/P4.3/P4.4）标定轮已由 §31 完成——**全部证伪关闭**（band_steps 路径切换代价 + delta 整数除法失效 + 基线无 banding/ringing 可保护，见否决表），默认保持 100 中性
- **P5.1 previous 参考竞争 streaming 缺口（✅ 已闭环，§32）**：原缺口——batch 默认 V2 配置（reference_mode Auto→Hybrid）逐帧 previous 竞争，streaming 恒 golden，默认配置下 batch/streaming 第 2 帧起可能不同。已修复：streaming previous 竞争补齐 change_mask 稀疏变化掩码（§29 缺口根因 1）+ 差分帧重建 rct_inverse 条件修正为 `has_rct`（原误用 `has_rct && !first_frame_no_rct`，根因 2），batch/streaming 在 previous/hybrid 模式逐字节一致（3 项新测试锁定）
- **接口规划未完成项**：[lossy-tuning-interface-plan.md](lossy-tuning-interface-plan.md) §13 项6「逐组接入参数」（部分）——新增算法字段（如 P1.6 Q_target 编号、量化矩阵、RDOQ λ 等）继续分阶段接入；UI 专家面板已提供 `expert_panel_schema()`，前端源码尚不存在
- **变换域深化未完成项**：frame_type=8 收益验证、CoeffCABAC 上下文建模深化（§5-P3 第 5~7 项）、top-2 试编码、矩形/4×4 块尺寸、Trellis/感知矩阵集成、端到端 CRF 文件级往返测试（✅ 端到端往返已补齐，§37 F3）
  - **其中 P3.2（方向扫描）与 P3.5（邻块上下文）已由 §30 合成内容探针证伪关闭**（收益 ≈0%，见否决表）；P3.6（小系数短码）/ P3.7（tile restart）维持未做、未证伪。
- **遗留缺陷修复（✅ 已闭环，§37）**：①type8 解码端 chroma_step 传参错误
  （`(q_step,q_step)`）→ type8 载荷自包含 luma/chroma 步长信令，解码端不再依赖
  文件头 lossy_quant；②路径 C `fq_for_chain_index` 色度半分辨率未解耦
  （`gq>1 &&`）→ 移除阻断，与路径 G 一致；③type8 有损文件级端到端往返测试
  → `test_lossy_frame_type8_file_roundtrip` 补齐。

---

## 七、文件合规核对

| 文件 | 行数 | 上限 | 状态 |
|---|---:|---:|---|
| `core/prediction/intra.rs` | 714 | 1000 | ✅（全项目最大文件）|
| `encoder/frame/intrabc.rs` | 662 | 1000 | ✅ |
| `encoder/frame/candidate.rs` | 599 | 1000 | ✅ |
| `encoder/sequence.rs` | 584 | 1000 | ✅ |
| `backend/cpu/simd.rs` | 539 | 1000 | ✅ |
| `encoder/rle_cabac.rs` | 536 | 1000 | ✅ |
| `core/entropy/context.rs` | 533 | 1000 | ✅ |
| `encoder/intra_probe.rs` | 335 | 1000 | ✅（探针保留为回归锚点）|
| `encoder/intra_transform.rs` | 264 | 1000 | ✅ |
| `decoder/intra_transform.rs` | 214 | 1000 | ✅ |

> 原预警文件已全部拆分收敛：`encoder/tests.rs`(980) → `encoder/tests/{lossy,roundtrip,rct_bypass,mod}.rs`（≤338）；
> `test/mod.rs`(~985) → `test/{batch,mod,probe}.rs`（≤252）；`format/prediction.rs`(800) → `core/prediction/intra.rs`(714)。
> 当前全项目无 ≥800 行文件；后续新增测试仍应建立独立领域文件（参照 `closed_loop_tests.rs` 先例），不得回填。
