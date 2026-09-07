# 用户手册

## 目录

- [简介](#简介)
- [安装说明](#安装说明)
- [CLI 用法](#cli-用法)
- [功能说明](#功能说明)
- [操作步骤](#操作步骤)
- [常见问题](#常见问题)
- [术语表](#术语表)

---

## 简介

CRF Viewer 是一款专业的差分图片**无损/有损**压缩工具（**纯 Rust CLI**，无 GUI）。
它支持将多张差分图（残差帧）序列压缩为紧凑的 `.crf` 格式文件，并保证逐像素还原；
同时提供有损高质量档位（对标 AVIF CQ18）。

### 主要功能

- **差分图压缩**：将 2~50 张差分图无损压缩为单个 .crf 文件（流式接口支持 >50 帧）
- **有损压缩**：Q75/Q90/Q95 等质量预设，或 V2 专家配置 JSON
- **端到端校验**：`--test` 自动执行编码→解码→逐像素校验
- **元数据与配置**：`--dump-resolved-config` / `--dump-expert-schema` 导出
- **基准**：`--bench` 输出端到端 p50/p95 与吞吐

### 适用场景

- 科研实验中的图像差分分析
- 图像处理算法的中间结果存档
- 嵌入式设备的图像数据传输
- 需要精确保存差分数据的任何场景

---

## 安装说明

### 下载安装

从 Releases 页面下载对应平台的安装包：

**Windows**：
- 下载 `.msi` 安装包，双击运行安装向导
- 或下载 `.exe` 免安装版本，直接运行

**macOS**：
- 下载 `.dmg` 文件，打开并将应用拖入 Applications 文件夹
- 首次运行需在「系统偏好设置 > 安全性」中允许运行

**Linux**：
- 下载 `.deb` 包：`sudo dpkg -i crf-viewer_*.deb`
- 或下载 `.AppImage`：`chmod +x crf-viewer_*.AppImage && ./crf-viewer_*.AppImage`

### 系统要求

| 项目 | 最低要求 |
| :--- | :--- |
| 操作系统 | Windows 10+ / macOS 12+ / Ubuntu 20.04+ |
| 内存 | 4GB RAM |
| 磁盘空间 | 100MB |
| 附加 | Windows NVIDIA 版需 NVIDIA 驱动（无则自动回退 CPU） |

---

## CLI 用法

> 项目为**纯 CLI**：无界面、无快捷键。所有操作通过命令行完成。

```bash
cd src-tauri

# 无损编码 + 端到端校验（test/png/1000 为示例图像组）
cargo run -- --test test/png/1000

# 有损编码（质量预设 + 色度采样）
cargo run -- --lossy-quality 96.50 --chroma-sampling 420 --effort 8 --test test/png/1000

# 专家配置 JSON
cargo run -- --expert-config path/to/lossy-v2.json --test test/png/1000

# 导出 resolved 配置 / 专家面板 schema
cargo run -- --dump-resolved-config resolved.json
cargo run -- --dump-expert-schema panel-schema.json

# 端到端基准
cargo run -- --bench test/png/1000
```

常用 CLI 参数：

| 参数 | 说明 |
| :--- | :--- |
| `--lossy-quality <q>` | 有损质量（如 96.50），缺省为无损 |
| `--chroma-sampling <auto\|444\|422\|420>` | 色度采样 |
| `--effort <0..10>` | 编码努力 |
| `--expert-config <path>` | V2 专家配置 JSON（同时接受 `{"lossy": {...}}` 包装） |
| `--dump-resolved-config <path>` | 导出解析后的有效配置 |
| `--dump-expert-schema <path>` | 导出专家面板 schema |
| `--test <dir>` | 端到端集成测试 |
| `--bench <dir>` | 端到端基准 |

---

## 功能说明

### 1. 打开文件

支持打开以下类型的文件：

| 文件类型 | 说明 |
| :--- | :--- |
| 图像文件 | PNG、BMP、TIFF 格式的单张或多张图像 |
| CRF 文件 | 已压缩的 .crf 格式文件 |

### 2. 导入差分图序列

将多张差分图导入应用进行查看或压缩：

- 支持拖拽导入
- 支持通过菜单或工具栏按钮导入
- 自动验证图像尺寸和格式一致性
- 支持 2~50 张图像

### 3. 压缩为 CRF

将差分图序列压缩为 .crf 格式：

- 选择压缩算法（Golomb-Rice / 指数哥伦布 / 变换编码）
- 设置压缩参数
- 显示压缩进度
- 保存生成的 .crf 文件

### 4. 查看 CRF 文件

打开并查看 .crf 文件的内容：

- 显示文件元数据（版本、帧数、尺寸等）
- 逐帧浏览解码后的差分图
- 查看每帧的编码信息

### 5. 解压还原

将 .crf 文件解压还原为原始差分图：

- 选择输出目录
- 选择输出格式（PNG / BMP / TIFF）
- 显示解压进度
- 生成与原始完全一致的图像文件

---

## 操作步骤

### 步骤 1：构建

```bash
cargo build --manifest-path src-tauri/Cargo.toml --workspace --release
```

### 步骤 2：准备输入

- 准备一个目录，内含 2~50 张同尺寸/位深/色彩格式的差分图（PNG/JPEG/WebP/BMP/TIFF）
- 超过 50 帧的序列走流式编码路径（`StreamingEncoder`，最高 65535 帧）

### 步骤 3：执行编码与校验

```bash
# 无损
cargo run -- --test <图像组目录>

# 有损（质量预设）
cargo run -- --lossy-quality 90 --test <图像组目录>

# 有损（V2 专家配置）
cargo run -- --expert-config config.json --test <图像组目录>
```

`--test` 路径自动执行：编码 → 写 `test_adaptive.crf` → 解码 → 逐像素校验，
并输出帧类型分布、体积与质量指标。

### 步骤 4：基准与导出

```bash
# 端到端基准（p50/p95 / MPix/s）
cargo run -- --bench <图像组目录>

# 导出配置 / schema
cargo run -- --dump-resolved-config resolved.json
cargo run -- --dump-expert-schema panel-schema.json
```

---

## 快捷键

> 无 GUI，不存在快捷键。CLI 参数见上文「CLI 用法」与「操作步骤」。

---

## 常见问题

### Q: 支持哪些图像格式？

A: 输入支持 PNG、BMP、TIFF 等常见格式。输出为自定义 `.crf` 格式。

### Q: 最大支持多少帧？

A: 50 帧是当前批量导入/编码路径的限制，不是 CRF 文件硬上限。超过 50 帧可使用流式
编码 API；当前码流字段最高可表示 65535 帧。

### Q: 压缩率如何？

A: 由于采用完全无损压缩且不做帧间预测，压缩率通常为原始大小的 50%~80%。具体取决于差分图的内容复杂度。

### Q: 打开 CRF 文件时提示"不是有效的 CRF 文件"？

A: 可能的原因：
- 文件已损坏
- 文件不是 CRF 格式
- 文件版本不兼容

请确认文件来源和完整性。

### Q: 压缩/解压速度很慢？

A: 正常情况下压缩/解压速度很快（< 100ms/帧）。如果很慢，请检查：
- 文件尺寸是否过大（建议不超过 4K 分辨率）
- 位深是否过高（16 位会比 8 位慢）
- 系统资源是否充足

### Q: 可以在 CRF 文件中添加备注吗？

A: 可以。编码请求（`EncodeParams.user_data`）支持最多 40 字节（UTF-8）的用户备注，
保存在文件头中。

### Q: 如何查看 CRF 文件的详细信息？

A: 通过 Rust API（`crf::codec` / 解码后的 `CrfHeader`）可读取完整元数据，包括版本、
帧数、尺寸、位深、压缩类型、flags 与 lossy_quant 等；`--dump-resolved-config` /
`--dump-expert-schema` 导出配置侧信息。

### Q: 不同位深的图像可以一起压缩吗？

A: 不可以。同一序列中的所有图像必须具有相同的宽高、位深和色彩格式。

---

## 术语表

| 术语 | 英文 | 说明 |
| :--- | :--- | :--- |
| 差分图 | Difference Image | 两帧图像之间的差异图像，也称为残差帧 |
| 残差帧 | Residual Frame | 与差分图含义相同，指像素级的差值数据 |
| 无损压缩 | Lossless Compression | 压缩后可完全还原原始数据，不丢失任何信息 |
| 有符号整数 | Signed Integer | 可以表示正数、负数和零的整数类型 |
| 位深 | Bit Depth | 每个像素通道使用的二进制位数 |
| 色彩格式 | Color Format | 图像的颜色编码方式（如 RGB、YUV） |
| 子采样 | Subsampling | 降低色度分辨率以减少数据量的技术 |
| 熵编码 | Entropy Encoding | 利用数据统计特性进行压缩的编码方式 |
| Golomb-Rice | — | 一种适合小数值的熵编码算法 |
| 指数哥伦布 | Exponential-Golomb | 一种适合大动态范围的熵编码算法 |
| 变换编码 | Transform Coding | 通过数学变换集中能量后再编码的方式 |
| 魔数 | Magic Number | 文件开头用于标识格式的固定字节序列 |
| CRC32 | — | 32位循环冗余校验，用于检测数据完整性 |
| Zigzag 扫描 | Zigzag Scan | 将二维矩阵转换为一维序列的扫描方式 |
| 帧索引 | Frame Index | 记录每帧在文件中位置的索引表 |
| 元数据 | Metadata | 描述文件属性的数据（如尺寸、格式等） |
