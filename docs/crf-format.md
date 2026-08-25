# CRF 格式规范

> **本文档已迁移**：CRF v1.8 格式的完整权威规范统一维护在仓库根目录
> [`crf格式标准.md`](../crf格式标准.md)，本页仅保留摘要与变更指引。

---

## 当前版本摘要（v1.8）

- **场景定位**：二次元插画差分序列的无损压缩（服装/表情/姿势差分等）
- **文件结构**：文件头(64B) + 帧索引 + 帧数据 × N + CRC32 文件尾(8B)
- **帧头**：11 字节 = `frame_size(u32)` + `pixel_count(u32)` + `frame_type(u8)` + `coding_params(u8)` + `pred_mode(u8)`
- **帧类型（frame_type）**：

  | 值 | 类型 |
  | :- | :--- |
  | 0 | 块级自适应 k Golomb |
  | 1 | RLE+Golomb（exp-Golomb 零行程） |
  | 2 | 条带自适应 RLE+Golomb（32 行条带，逐条带模式与 k） |
  | 3 | 三平面打包（YCoCg-R 后 Y/Co/Cg 独立编码，含 CfL + 可选色度半分辨率） |
  | 4 | 调色板模式（低色数 ≤256） |
  | 5 | RLE+CABAC 自适应算术编码（符号分离 + 邻域分级上下文） |
  | 6 | DCT 变换域量化（4×4 lifting DCT → 死区量化 → RLE+CABAC） |
  | 7 | 帧内块复制（ITBC，8×8 hash 链 LZ 式重复纹理） |
  | 8 | 预测后变换 + CABAC 系数编码（v1.14：三平面逐块预测→skip/DCT→run-level CABAC） |

- **预测模式**：None / Horizontal / Vertical / Average / DC / Med(JPEG-LS) / Paeth(AV1)
- **可逆色彩变换**：YCoCg-R（文件头 flags.bit1 标记）
- **变换核**：lifting 可逆整数 DCT 4×4（严格可逆）
- **真有损**：残差闭环死区量化（flags.bit2），LossyTuning 可调参数组：
  - `chroma_quant_percent` = 130%（色度步长放大）
  - `keyframe_interval` = 10（锚点间隔）
  - `deadzone_bias` = 0（死区偏置）
  - `chroma_half_res` = true（色度半分辨率）
  - `anchor_quality_percent` = 100（锚点帧质量百分比，100=与普通帧同 Q）
  - `noise_adaptive` = false（**JPEG 源噪声感知**，v1.8 新增）
  - `noise_tau_x100` = 150（噪声阈值系数 τ×100）
- **并行**：rayon 帧间 + 条带级并行；行采样 SAD 决策加速；golden 参考全并行
- **首帧参考**：残差基准为首帧 golden（误差零累积、随机访问解码）；golden 首帧在有损模式下强制无损

## 噪声感知（v1.8）

`lossy_tuning.noise_adaptive=true` 且有损模式时，差分帧启用两级机制：

1. **零中心性门控软阈值**：带符号中位数偏离 ≤ 幅度 P25 的场（零中心
   失真差）应用 T=τ·amp25 归零；系统性偏移（时间差分）与两极化结构场
   （服装/表情差分）自动零介入；
2. **闭环 per-band 自适应步长**：每 32 行条带按幅度 P25 推导有效步长
   Q_eff=clamp(τ·amp25, base, 16)，闭环量化逐行查表——死区宽度直接跟随
   局部失真水平。

解码端无感、格式零改动；无损模式不受影响。纯场景差分-1 实测 q90 −29%。

## 性能基准

| 测试组 | 源基准 | 全自适应无损 | 有损最优 |
| :--- | ---: | ---: | ---: |
| PNG1000（14 张 1024×1820） | PNG 16.64 MB | **10.63 MB（−36%）** | q75 **1.47 MB（−91%）** |
| AVIF 对标（av1_nvenc crf30 yuv420p） | 3.21 MB / 37.2dB | — | q50+half-res **2.38 MB（−26%），PSNR≈37.7dB** |

无损方案全部像素级校验通过。

## 完整规范

文件头字段表、位流格式、编码管线流程图、版本历史等详见：
[`../crf格式标准.md`](../crf格式标准.md)

## frame_type=8：预测后变换 + CABAC 系数编码（v1.14）

**编码路径**：RCT → 三平面拆分 → 每平面逐 8×8 块：
- 邻域预测（DC/H/V/MED，引用已重建像素）
- DC 模式 → transform skip（残差直通量化，避免 DCT 能量扩散）
- H/V/MED 模式 → 8×8 lifting DCT → 死区量化
- 系数 zigzag 扫描 → CABAC run-level 嵌入式编码（全零块 1 bit）

**载荷布局**：
`
[flags u8]                   // 保留（当前 0）
[len_y u32 LE][y_payload]    // Y 平面子载荷
[len_co u32 LE][co_payload]  // Co 平面子载荷
[len_cg u32 LE][cg_payload]  // Cg 平面子载荷
`
**子载荷布局**：
`
[mode_len u32 LE][mode_stream][coeff_stream]
`
- mode_stream：RLE+CABAC 压缩的预测模式表（每块值 0..3）
- coeff_stream：CABAC 系数流（3 上下文：nonzero/run/level_prefix + 直通余数/sign）

**CABAC 上下文**：
| 上下文 | 初始 prob | 作用 |
|---|---|---|
| ctx_nonzero | 2048 | 块是否有非零系数 |
| ctx_run | 2048 | run 截断一元 bit |
| ctx_level_q | 2048 | level 商前缀 bit |
| 余数 + sign | — | 等概率直通 |

**版本兼容**：frame_type=8 在 v1.14 引入；旧版本解码器遇到 type=8 返回
UnsupportedVersion 错误（不尝试猜测解码）。

**PNG1000 实测**（Y 平面探针，CABAC 版）：q90 帧1 −24.5%、帧2 −24.2%
（超越自适应空间域管线）；q75 帧1 −14.9%（从 v1 的 +171% 逆转）。
