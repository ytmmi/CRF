# CRF 有损精细参数接口规划

**规划日期**：2026-08-25  
**适用范围**：有损管线；无损管线不读取这些参数  
**状态**：V2 控制面、JSON/CLI/schema 已实施；V1 已删除，算法字段继续分阶段接入  
**关联方案**：[目标高质量预设与首帧优化](first-frame-optimization-plan.md)

> **进度同步（2026-08-26）**：V2 顶层模型、`ResolvedLossyReport`、严格校验、
> `resolve_without_encoding()`、Builder 与 V2 编码内核直连已落地；V1 API 及兼容适配器已删除；同时提供严格
> JSON、常用 CLI flags、专家配置文件、resolved 导出和 UI 专家面板 schema。仓库当前没有
> React/Tauri 前端源码，因此 UI 落地点是可直接供未来前端消费的 schema 与解析 API。

后续实现必须遵守[《CRF 项目开发、算法与构建标准》](project-standards.md)，不得把配置
解析、预设映射、像素算法、码流信令和 UI 逻辑重新杂交进同一模块。

性能后端、CPU/GPU 选择、设备能力和传输阈值的专项路线见
[《CRF 性能优化规划》](performance-optimization-plan.md)；本文只定义参数控制面，不在
这里重复 GPU kernel 或厂商运行时设计。

## 1. 目标

有损管线应同时提供两种使用方式：

1. **预设层**：用户选择 Q75、Q90、Q95、Q96、Q97 等质量编号，编码器自动解析为
   一组经过版本化标定的参数；最终用于 AVIF CQ18 对标的编号由率失真实验决定。
2. **精细层**：专业用户可以从预设继承后覆盖首帧、量化、色度、感知保护、参考策略、
   码率目标和编码努力等参数，也可以完全不使用预设而提供显式配置。

接口目标不是把所有内部常量直接公开，而是提供稳定、可验证、不会破坏码流语义的
控制面。默认用户只看到预设和少量安全选项；内部工具开关放在专家/实验命名空间。

## 2. 现有接口盘点

当前 `EncodeParams` 使用：

```text
lossy_quality: Option<u8>          // None=无损，Some(1..100)=有损质量
lossy_tuning: Option<LossyTuning>  // None=全部采用默认值
```

现有 `LossyTuning` 已公开八个字段：

| 字段 | 当前默认 | 当前作用 | 已知问题 |
|---|---:|---|---|
| `chroma_quant_percent` | 130 | Co/Cg 步长相对亮度比例 | Q=1 时整数截断使 130% 仍为 1 |
| `keyframe_interval` | 10 | 锚点/刷新间隔 | 注释仍混有旧链式语义；范围只有 `u8` |
| `deadzone_bias` | 0 | `-32..32` 死区偏置 | 亮度、色度、首帧和残差不能分别控制 |
| `chroma_half_res` | true | 色度 2×2 下采样 | 实际还受 `step > 1` 隐式阻断；布尔值不能表达 Auto/422 |
| `anchor_quality_percent` | 100 | 锚点步长比例 | 整数百分比精度不足，且与 `golden_lossless` 语义重叠 |
| `noise_adaptive` | false | 残差噪声感知 | 只有开/关，缺少自动策略和作用域 |
| `noise_tau_x100` | 150 | 噪声阈值系数 | 关闭噪声感知时被静默忽略 |
| `golden_lossless` | true | 强制首帧无损 | 不适合表达“匹配序列质量/质量偏移/显式质量” |

`lossy_quality` 目前按约 5 个质量点映射一个整数步长，且 Q95 有硬编码特殊语义，无法
精细表达 AVIF CQ18 落在现有 Q90 与 Q95 之间的率失真位置。公开 API 文档和用户指南
也尚未完整列出这些字段。

## 3. 设计原则

### 3.1 预设是配置，不是特殊算法分支

- 质量编号只选择一份版本化默认配置，不直接等同某个量化步长。
- 先标定目标率失真点，再决定公开编号；不能为了保留 Q95 名称强行改变目标。
- 较高编号必须表示不低于较低编号的目标质量；若 AVIF 对标档最终叫 Q96/Q97，需联合
  重标定相邻档位，不能只给一个较低质量产物改名。
- 预设解析后进入与自定义配置相同的编码路径，避免 Q95/Q96 等散落硬编码。

### 3.2 固定点公开，浮点只用于界面显示

配置文件和 Rust API 使用定点整数，例如：

- `quality_x100=9650` 表示 Q96.50；
- `lambda_scale_x1000=1000` 表示 1.0；
- `quant_scale_x1000=1300` 表示 1.3。

这样可保证跨平台解析、哈希、回归和批量/streaming 决策一致。JSON/UI 可以显示小数，
但进入编码器前必须转换为定点值。

### 3.3 Auto、继承、强制关闭必须可区分

- `None`/字段缺失：从预设继承；没有预设时采用安全默认值。
- `Auto`：允许编码器按内容决定。
- `Enabled` / `Disabled`：用户强制打开或关闭。
- 显式数值：覆盖预设。

不能再用一个 `bool` 同时承担“继承默认”“自动判断”和“强制打开”三种语义。

### 3.4 解码语义和编码搜索分层

参数分为三类：

| 类别 | 示例 | 是否需要码流信令 |
|---|---|---|
| 编码器搜索参数 | RDO λ、候选预算、质量护栏、effort | 否 |
| 编解码工具参数 | 色度采样、滤波器、参考类型、变换尺寸 | 是，或其选择结果需要信令 |
| 记录/诊断参数 | 预设版本、配置摘要、告警 | 可选元数据，不应影响解码 |

公开接口不能让用户直接填写 frame flag 或 bit 位。用户选择语义，编码器负责生成合法
码流信令。

## 4. 建议的顶层模型

建议用新的版本化结构承载有损配置，而不是继续无限扩张 `LossyTuning`：

```rust
pub struct LossyOptionsV2 {
    pub api_version: u16,
    pub base: LossyBase,
    pub rate: RateControl,
    pub first_frame: FirstFrameTuning,
    pub quant: QuantizationTuning,
    pub chroma: ChromaTuning,
    pub perceptual: PerceptualTuning,
    pub temporal: TemporalTuning,
    pub experimental: Option<ExperimentalToolTuning>,
    pub performance: PerformanceTuning,
}

pub enum LossyBase {
    Preset {
        quality_x100: u16,
        revision: Option<u16>,
    },
    Explicit,
}
```

顶层编码参数建议逐步过渡为：

```rust
pub lossy: Option<LossyOptionsV2> // None = 无损管线
```

V1 的 `lossy_quality` 和 `lossy_tuning` 已删除，不提供运行时兼容适配。

## 5. 稳定公开参数

以下参数面向 SDK、CLI、配置文件和高级 UI。范围是实施前的建议验证范围，不代表本轮
已支持。

### 5.1 基础质量与码率

| 参数 | 类型/建议范围 | 默认 | 语义 |
|---|---|---|---|
| `base.quality_x100` | `100..10000` | 由用户选择 | 可表达 Q96.50；选择对应版本化预设 |
| `base.revision` | `u16?` | 最新稳定版 | 固定预设配置版本，保证复现实验 |
| `rate.mode` | `ConstantQuality / TargetBytes / TargetBpp / ConstrainedQuality` | `ConstantQuality` | 质量优先、大小优先或双约束 |
| `rate.target_bytes` | `u64?` | 无 | 完整 CRF 目标字节；与 target_bpp 互斥 |
| `rate.target_bpp_x10000` | `u32?` | 无 | 每像素目标比特率，定点表示 |
| `rate.max_bytes` | `u64?` | 无 | 硬上限；无法满足质量下限时必须报错/告警 |
| `rate.min_quality_x100` | `100..10000?` | 从预设继承 | 码率控制不得突破的最低质量编号 |
| `rate.rdo_lambda_scale_x1000` | `250..4000?` | 1000 | 整体码率/失真权重；编码器参数 |
| `rate.max_frame_drop_db_x100` | `0..500?` | 100 | 单帧相对质量锚点允许下降的上限 |

约束：`TargetBytes/TargetBpp` 必须与质量下限配合。达不到二者时返回“码率不足”结果，
不能静默牺牲画质或偷偷突破大小上限。

### 5.2 首帧与锚点

| 参数 | 类型/建议范围 | 默认 | 语义 |
|---|---|---|---|
| `first_frame.mode` | `MatchSequence / Lossless / QualityOffset / Explicit` | `MatchSequence` | 目标高质量预设默认让首帧参与有损优化 |
| `first_frame.quality_offset_x100` | `-2000..2000` | 0 | 相对全局质量编号偏移；仅 QualityOffset |
| `first_frame.quality_x100` | `100..10000?` | 无 | Explicit 模式的首帧质量 |
| `first_frame.max_bytes` | `u64?` | 无 | 单独约束 frame0 载荷 |
| `first_frame.rdo_lambda_scale_x1000` | `250..4000?` | 1000 | 首帧专用率失真权重 |
| `temporal.anchor_interval` | `0..65535 / Auto` | `Auto` | 0 表示不插入周期锚点；场景切换仍可触发 |
| `temporal.anchor_quality_offset_x100` | `-2000..2000` | 0 | 后续锚点相对全局质量偏移 |

无论首帧取何种质量，后续残差必须基于编码端重建的 `G_hat`。`Lossless` 只是用户显式
选择，不能让编码器恢复到“用源 frame0 计算残差”的非闭环路径。

### 5.3 量化

| 参数 | 类型/建议范围 | 默认 | 语义 |
|---|---|---|---|
| `quant.mode` | `FromQuality / ExplicitSteps` | `FromQuality` | 质量驱动或直接量化步长 |
| `quant.luma_step_q8` | `256..65280?` | 自动 | Q8 定点亮度基础步长；仅 ExplicitSteps |
| `quant.chroma_step_q8` | `256..65280?` | 自动 | Q8 定点色度基础步长；解决 Q=1×130% 截断 |
| `quant.luma_scale_x1000` | `500..4000?` | 1000 | 从预设继承后的亮度倍率 |
| `quant.chroma_scale_x1000` | `500..4000?` | 由预设决定 | 色度倍率，替代整数百分比 |
| `quant.dc_scale_x1000` | `250..2000?` | 1000 | DC/最低频保护或释放 |
| `quant.high_freq_scale_x1000` | `500..4000?` | 由矩阵决定 | 高频量化强度 |
| `quant.deadzone_luma_x256` | `-128..256?` | 0 | 亮度死区偏置 |
| `quant.deadzone_chroma_x256` | `-128..256?` | 0 | 色度死区偏置 |
| `quant.matrix` | `Auto / Flat / Perceptual / EdgePreserving` | `Auto` | 稳定矩阵配置 |
| `quant.rdoq` | `Auto / Off / Fast / Full` | `Auto` | Trellis/RDOQ 努力等级 |

`ExplicitSteps` 是专家接口，会绕过质量编号到基础步长的映射；此时输出报告不得声称为
某个标准 Q 预设。自定义量化矩阵数组先放实验接口，稳定 API 只公开版本化矩阵名称。

### 5.4 色度采样与滤波

| 参数 | 类型 | 默认 | 语义 |
|---|---|---|---|
| `chroma.sampling` | `Auto / Cs444 / Cs422 / Cs420` | `Auto` | 目标 AVIF 档预期允许 Cs420 |
| `chroma.downsample_filter` | `Auto / Box / Bilinear / SixTap` | `Auto` | 下采样抗混叠滤波 |
| `chroma.upsample_filter` | `Auto / Bilinear / FourTap / SixTap` | `Auto` | 解码重建滤波，需要码流可判定 |
| `chroma.siting` | `Auto / Centered / Cosited` | `Auto` | 色度样本位置 |
| `chroma.edge_protection_x100` | `0..200` | 100 | 饱和边缘的色度保护强度 |

采样、滤波和 siting 必须作为一个组合验证，不能只公开 `half_res` 布尔值。强制 Cs420
时若输入不是 RGB 三分量，应返回明确错误或告警，而不是静默忽略。

### 5.5 感知控制

| 参数 | 类型/建议范围 | 默认 | 语义 |
|---|---|---|---|
| `perceptual.activity_masking_x100` | `0..200` | 100 | 纹理区允许更强量化 |
| `perceptual.flat_area_protection_x100` | `0..200` | 100 | 平坦/渐变区防 banding |
| `perceptual.edge_protection_x100` | `0..200` | 100 | 线稿、文字、轮廓保护 |
| `perceptual.ringing_control_x100` | `0..200` | 100 | 抑制变换振铃的倾向 |
| `perceptual.metric` | `Sse / SsimHybrid / Auto` | `Auto` | RDO 失真代理，不改变验收指标 |
| `perceptual.noise_mode` | `Off / Auto / Manual` | `Auto` | 有损源噪声处理策略 |
| `perceptual.noise_tau_x100` | `50..400?` | 150 | Manual 模式阈值系数 |

这些字段表达用户意图，不应直接等同某个内部公式常数。编码器版本可以改进实现，但固定
预设 revision 和 deterministic 模式时必须保持可复现。

### 5.6 时间参考与变化建模

| 参数 | 类型/建议范围 | 默认 | 语义 |
|---|---|---|---|
| `temporal.reference_mode` | `Auto / Golden / Previous / Hybrid` | `Auto` | 参考 reconstructed golden、前帧或逐帧竞争 |
| `temporal.scene_cut` | `Off / Auto / Manual` | `Auto` | 场景切换插入新 anchor |
| `temporal.scene_cut_threshold_x1000` | `0..1000?` | 自动 | Manual 模式阈值 |
| `temporal.change_mask` | `Auto / Off / On` | `Auto` | 稀疏变化 tile mask |
| `temporal.motion_mode` | `Off / Auto / Integer` | `Auto` | 轻量位移补偿 |
| `temporal.motion_range` | `0..32?` | 自动 | 整数像素搜索半径 |

所有参考模式只能引用编码端已重建帧。强制 `Previous` 时需提示随机访问和误差传播特性；
`Hybrid` 的逐帧选择结果必须进入码流信令。

## 6. 专家/实验参数

以下参数与码流工具耦合较强，不建议一开始作为稳定 SDK 承诺。它们应位于
`experimental.tools` 下，并要求 `allow_experimental=true`：

| 参数 | 候选值 | 用途 |
|---|---|---|
| `tile_size` | `Auto / 32 / 64` | 首帧固定 tile 探针 |
| `transform_sizes` | `{4x4, 8x8, 4x8, 8x4, 16x16}` | 限制变换候选 |
| `prediction_modes` | `Auto` 或模式集合 | 限制 DC/H/V/Paeth/planar/D45/D135 |
| `transform_skip` | `Auto / Off / On` | 线稿/文字保护与消融 |
| `coefficient_coding` | `Auto / PlaneLegacy / BlockEob` | 通用平面或逐块 EOB 语法 |
| `palette` | `Auto / Off / On` | 局部 RGB palette/颜色缓存 |
| `rdo_candidate_limit` | `0..N` | 0=完整候选；非零用于速度实验 |

实验参数可能随格式版本改名或移除。若某工具需要新码流语法，编码器必须先检查目标
format version，不能在旧格式下静默降级。

## 7. 性能控制

性能参数不直接代表质量，但可能因候选搜索预算改变率失真结果：

| 参数 | 类型/建议范围 | 默认 | 说明 |
|---|---|---|---|
| `performance.effort` | `0..10` | 7 | 统一控制候选数、RDOQ 和搜索范围 |
| `performance.threads` | `0..系统上限` | 0=Auto | 批量和 streaming 共用语义 |
| `performance.memory_limit_mb` | `u32?` | 无 | 超限时返回或选择已声明的低内存策略 |
| `performance.deterministic` | `bool` | true | 相同输入、版本、配置产生相同码流 |
| `performance.fast_fail` | `Auto / Off / On` | `Auto` | 只允许安全剪枝；不得改变胜者 |

`effort` 若会改变最终码率，报告中必须记录；`fast_fail=On` 只能启用数学上保证不改变
最优结果的剪枝。

## 8. 参数解析优先级

建议固定为：

```text
安全默认值
  → 预设 quality_x100 + preset revision
  → 稳定公开 overrides
  → experimental overrides
  → 输入能力校验
  → 内容自适应决策（只在 Auto 范围内）
  → resolved configuration + warnings
```

用户显式值优先于预设和 Auto，但不能突破格式合法性。解析结果应返回：

```rust
pub struct ResolvedLossyReport {
    pub api_version: u16,
    pub preset_revision: Option<u16>,
    pub effective: ResolvedLossyOptions,
    pub warnings: Vec<ConfigWarning>,
    pub ignored: Vec<IgnoredField>,
    pub config_fingerprint: [u8; 16],
}
```

默认不允许静默忽略。只有调用者显式设置 `allow_ignored=true` 时，未知或无效于当前输入的
字段才可进入 `ignored` 后继续编码。

## 9. 互斥关系与校验

实施时至少校验：

| 条件 | 行为 |
|---|---|
| `lossy=None` 却提供有损 tuning | 报参数冲突；不得污染无损管线 |
| 同时提供 V2 与旧 `lossy_quality/lossy_tuning` | 报冲突，不猜优先级 |
| `ExplicitSteps` 未给 luma/chroma step | 报缺失字段 |
| `FromQuality` 同时强制 raw step | 报互斥冲突 |
| `TargetBytes` 未给 `target_bytes` | 报缺失字段 |
| `target_bytes` 与 `target_bpp` 同时提供 | 报互斥冲突 |
| `first_frame=QualityOffset` 未给 offset | 报缺失字段 |
| `noise_mode!=Manual` 却给 tau | 告警或报冲突，不能静默忽略 |
| 非三分量输入强制 Cs420 | 报不支持或明确回退告警 |
| 参考模式使用未重建帧 | 编码器内部不变量失败，禁止输出 |
| 实验工具不受目标格式版本支持 | 报版本不兼容 |
| 较高质量编号解析为更激进配置 | 预设发布校验失败 |

所有数值在进入大图分配、乘法或码率计算前做范围和溢出检查。

## 10. API、CLI 与 UI 规划

### 10.1 Rust/SDK

- 提供 builder，避免用户必须构造所有嵌套字段。
- 提供 `validate()` 和 `resolve_without_encoding()`，让调用者在编码前查看有效配置。
- `Preset(Q96.50).with_chroma(Cs420).with_first_frame_offset(+50)` 这类调用只作为
  便利封装，底层仍序列化为 V2 固定点字段。
- 批量编码器和 `StreamingEncoder` 必须接收同一 resolved config，不分别解析默认值。

### 10.2 JSON/Tauri

预设加少量覆盖示例：

```json
{
  "lossy": {
    "apiVersion": 2,
    "base": { "type": "preset", "quality": 96.5, "revision": 1 },
    "firstFrame": { "mode": "match-sequence", "maxBytes": 297745 },
    "chroma": { "sampling": "420", "edgeProtection": 1.2 },
    "perceptual": {
      "flatAreaProtection": 1.25,
      "edgeProtection": 1.2,
      "activityMasking": 1.0
    },
    "performance": { "effort": 8, "deterministic": true }
  }
}
```

完全显式模式示例：

```json
{
  "lossy": {
    "apiVersion": 2,
    "base": { "type": "explicit" },
    "rate": {
      "mode": "constrained-quality",
      "targetBytes": 3370055,
      "minQuality": 96.0
    },
    "quant": {
      "mode": "explicit-steps",
      "lumaStep": 1.375,
      "chromaStep": 1.75,
      "matrix": "edge-preserving",
      "rdoq": "full"
    },
    "firstFrame": { "mode": "quality-offset", "qualityOffset": 0.5 },
    "temporal": { "referenceMode": "hybrid", "changeMask": "auto" }
  }
}
```

JSON 小数必须按规定精度转换；超出两位质量精度或三位倍率精度时返回错误，不能依赖
二进制浮点的隐式舍入。

### 10.3 CLI

建议分为常用和专家两组：

```text
--lossy-quality 96.50
--target-bytes 3370055
--first-frame-quality-offset 0.50
--chroma-sampling auto|444|422|420
--perceptual-strength 0..200
--effort 0..10

--expert-config path/to/lossy-v2.json
--dump-resolved-config path/to/resolved.json
```

不建议为几十个实验字段全部增加独立 CLI flag；专家配置统一走版本化 JSON。

### 10.4 UI

- 默认面板：质量预设、目标大小（可选）、effort。
- 高级面板：首帧、色度、感知保护、时间参考。
- 实验面板：只有启用实验功能后显示工具级参数。
- 每个字段显示“继承值/有效值”，修改后即时展示冲突和预计影响方向。
- 提供“恢复预设”和“导出 resolved config”，避免用户无法复现实验。

## 11. 兼容迁移

旧接口适配建议：

| 旧字段 | V2 映射 |
|---|---|
| `lossy_quality=Some(q)` | `base=Preset { quality_x100=q*100, revision=legacy_v1 }` |
| `chroma_quant_percent` | `quant.chroma_scale_x1000=percent*10` |
| `keyframe_interval` | `temporal.anchor_interval`，需按旧 0 语义转换 |
| `deadzone_bias` | 同时映射 luma/chroma deadzone，保留旧 `/64` 换算 |
| `chroma_half_res=true/false` | `chroma.sampling=Cs420/Cs444`，同时保留旧滤波器 ID |
| `anchor_quality_percent` | 转为 anchor quality offset/scale 的 legacy 解释 |
| `noise_adaptive` | `perceptual.noise_mode=Manual/Off` |
| `noise_tau_x100` | `perceptual.noise_tau_x100` |
| `golden_lossless=true` | `first_frame.mode=Lossless` |
| `golden_lossless=false` | P0 闭环完成后映射 `MatchSequence` |

兼容适配只保证旧配置的行为可复现，不代表旧 Q95 被继续用作 AVIF 对标档。新预设 revision
可以把目标档标为 Q96/Q97，但必须通过整套单调性和率失真验收。

API 版本、预设 revision 和 CRF bitstream version 必须分开：

- API version 决定字段解析；
- preset revision 决定默认配置；
- bitstream version 决定解码工具和信令。

三者不能复用一个版本号。

## 12. 结果可复现与报告

每次有损编码建议输出或可查询：

- 请求配置、resolved config 和配置 fingerprint；
- 编码器版本、预设 revision、bitstream version；
- frame0/完整文件字节、每帧参考类型和有效质量参数；
- 工具胜出率、4:2:0/矩阵/RDOQ/参考模式的实际选择；
- PSNR/SSIM 等指标仅在调用者提供原始参考时计算；
- 所有参数告警、回退和被忽略字段。

敏感的绝对文件路径和源图内容不进入 CRF 元数据；配置摘要只记录算法参数。

## 13. 实施顺序与验收

1. 冻结 V2 字段命名、定点单位、Auto/继承规则和错误模型。
2. 实现纯参数解析与 `resolve_without_encoding()`，先不改变码流。
3. 编写旧接口到 V2 的兼容适配器，并锁定 legacy resolved config。
4. 让批量/streaming 共用同一解析结果；任何有效参数差异视为失败。
5. P0 reconstructed-golden 闭环完成后，再开放 `first_frame != Lossless`。
6. 逐组接入量化/色度、感知、首帧、时间参考参数；每组都有范围、互斥和往返测试。
7. 最后公开实验工具命名空间和 UI 专家面板。

接口验收要求：

- 无损模式不受任何有损字段影响；
- 相同输入、版本、resolved config 在 deterministic 模式下产生相同码流；
- 不存在未报告的参数忽略或自动回退；
- 预设解析与“同一 resolved config 的显式模式”逐字节一致；
- 所有公开数值都有单位、范围、默认、作用域和冲突规则；
- 较高质量编号的分层验证集质量不得低于较低编号；
- 新接口不会绕过 reconstructed-reference、格式版本或解码安全检查。

## 14. 本轮边界

本文只规划未来的有损参数控制面。没有修改 `EncodeParams`、`LossyTuning`、Tauri、CLI、
UI 或码流格式，也没有执行构建。具体字段范围应在 AVIF 对标实验和至少 30 个分层序列
完成后冻结。
