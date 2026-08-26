# CRF - 差分图片无损压缩查看器

<p align="center">
  <img src="docs/icon.png" alt="CRF Logo" width="128" height="128">
</p>

<p align="center">
  <strong>面向二次元插画差分场景的跨平台无损压缩与查看工具（基于 Tauri 2）</strong>
</p>

<p align="center">
  <a href="#功能特性">功能特性</a> •
  <a href="#技术栈">技术栈</a> •
  <a href="#快速开始">快速开始</a> •
  <a href="#安装指南">安装指南</a> •
  <a href="#使用说明">使用说明</a> •
  <a href="#开发指南">开发指南</a> •
  <a href="#格式规范">格式规范</a>
</p>

---

## 功能特性

### 核心功能

- **差分图片处理**：批量接口支持 2~50 张差分图；流式接口支持超过 50 帧的序列
- **无损压缩**：基于 H.264 算法的完全无损压缩，保证像素级精确还原
- **CRF 格式支持**：自研 `.crf` (Compressing Residual Frames) 轻量级格式
- **高速编解码**：针对短序列优化，编解码延迟低于 100ms/帧
- **多平台支持**：Windows、macOS、Linux 全平台覆盖

### 查看功能

- **序列浏览**：逐帧查看差分图序列
- **差异可视化**：直观显示帧间差异
- **元数据查看**：显示文件头信息、编码参数等
- **对比模式**：支持原图与差分图对比查看

### 压缩特性

| 特性 | 说明 |
| :--- | :--- |
| 压缩算法 | YCoCg-R 可逆去相关 + 多模式空间预测（MED/PAETH/DC 等）自适应 + RLE+Golomb 混合熵编码 |
| 包装格式 | .crf (自定义轻量格式，v1.2 规范见 [crf格式标准.md](crf格式标准.md)) |
| 无损保证 | 数学严格可逆，逐像素精确还原，全部方案往返校验通过 |
| 帧数范围 | 批量 2~50 帧；流式最高 65535 帧（当前码流字段上限） |
| 自适应决策 | 逐帧预测模式选择、32 行条带级模式切换、首帧双路熵编码竞争、三平面打包——仅在更小时采用，单调不劣化 |

---

## 技术栈

### 后端 (Rust)

- **框架**：Tauri 2.0
- **语言**：Rust
- **图像处理**：image-rs
- **序列化**：serde / serde_json
- **压缩算法**：自研 Golomb-Rice / 指数哥伦布编码器

### 前端 (React)

- **框架**：React 18+
- **构建工具**：Vite
- **类型系统**：TypeScript
- **UI 组件**：待定（Ant Design / Material UI）
- **状态管理**：Zustand / React Context

### 构建与打包

- **包管理**：pnpm
- **代码检查**：ESLint + Prettier
- **测试框架**：Vitest (前端) + cargo-test (Rust)
- **CI/CD**：GitHub Actions

---

## 快速开始

### 环境要求

- **Node.js**：>= 18.0
- **pnpm**：>= 8.0
- **Rust**：>= 1.75
- **Tauri CLI**：>= 2.0

### 安装依赖

```bash
# 克隆项目
git clone https://github.com/your-username/crf-viewer.git
cd crf-viewer

# 安装前端依赖
pnpm install

# 安装 Rust 依赖（首次构建会较慢）
cargo build
```

### 开发模式

```bash
# 启动开发服务器（前端 + Rust 后端热重载）
pnpm tauri dev
```

### 构建发布版本

```bash
# 构建当前平台的发布版本
pnpm tauri build

# 构建产物位于 src-tauri/target/release/bundle/
```

Windows NVIDIA 版本采用标准 `exe + dll` 分发。Rust workspace 构建会同时生成：

```text
src-tauri/target/release/crf-viewer.exe
src-tauri/target/release/crf_cuda.dll
```

发布或制作安装包时必须将 `crf_cuda.dll` 与 `crf-viewer.exe` 放在同一目录；不能只分发
EXE。该 DLL 只使用 NVIDIA 驱动提供的 `nvcuda.dll`，目标机无需 CUDA Toolkit。没有 NVIDIA
驱动、DLL 缺失或 GPU 异常时，程序自动回退 CPU；CPU-only 构建可不携带该 DLL。

---

## 安装指南

### 预编译版本

从 [Releases](https://github.com/your-username/crf-viewer/releases) 页面下载对应平台的安装包：

| 平台 | 文件格式 |
| :--- | :--- |
| Windows | `.msi` / `.exe` |
| macOS | `.dmg` |
| Linux | `.deb` / `.AppImage` |

### 从源码构建

详细构建步骤请参考 [开发文档](docs/development.md)。

---

## 使用说明

### 基本操作

1. **导入差分图**
   - 点击「导入」按钮或拖拽文件到窗口
   - 选择 2~50 张差分图文件（支持 PNG、BMP、TIFF）
   - 系统自动验证图像一致性和格式

2. **压缩保存**
   - 点击「压缩」按钮
   - 选择保存路径和文件名
   - 等待压缩完成（通常 < 1秒）

3. **查看 CRF 文件**
   - 打开 `.crf` 文件
   - 使用序列浏览功能逐帧查看
   - 查看元数据和编码信息

4. **解压还原**
   - 打开 `.crf` 文件
   - 点击「解压」按钮
   - 选择输出目录
   - 获得与原始完全一致的差分图序列

### 有损 V2 命令行配置

常用精细参数可直接传入；完整专家参数使用版本化 JSON：

```bash
cd src-tauri
cargo run -- --lossy-quality 96.50 --chroma-sampling 420 --effort 8
cargo run -- --expert-config path/to/lossy-v2.json
cargo run -- --lossy-quality 96.50 --dump-resolved-config resolved.json
cargo run -- --dump-expert-schema panel-schema.json
```

`--expert-config` 同时接受直接的 V2 对象和 `{ "lossy": { ... } }` 包装格式。V2 是唯一
有损配置接口；详细字段、固定点单位和校验规则见
[API 文档](docs/api.md)。

### 快捷键

| 快捷键 | 功能 |
| :--- | :--- |
| `Ctrl/Cmd + O` | 打开文件 |
| `Ctrl/Cmd + S` | 保存/导出 |
| `←` / `→` | 上一帧/下一帧 |
| `Space` | 播放/暂停序列 |
| `Ctrl/Cmd + I` | 查看文件信息 |

### 文件格式

详细格式说明请参考 [CRF 格式规范文档](docs/crf-format.md)。

---

## 项目结构

```
crf-viewer/
├── docs/                          # 文档目录
│   ├── architecture.md           # 技术架构文档
│   ├── api.md                    # API 接口文档
│   ├── crf-format.md             # CRF 格式规范
│   ├── development.md            # 开发指南
│   ├── project-standards.md      # 强制项目开发、算法与构建标准
│   ├── codec-architecture-refactor-plan.md # 编解码器分层重构规划
│   ├── performance-optimization-plan.md # CPU/GPU 性能优化规划
│   ├── first-frame-optimization-plan.md # 有损目标预设与首帧优化规划
│   ├── lossy-tuning-interface-plan.md   # 有损精细参数接口规划
│   └── user-guide.md             # 用户手册
├── src/                          # 前端源码 (React)
│   ├── components/               # UI 组件
│   ├── hooks/                    # 自定义 Hooks
│   ├── services/                 # 前端服务
│   ├── stores/                   # 状态管理
│   └── utils/                    # 工具函数
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── commands/             # Tauri 命令
│   │   ├── crf/                  # CRF 格式处理
│   │   │   ├── encoder.rs       # 编码器
│   │   │   ├── decoder.rs       # 解码器
│   │   │   └── format.rs        # 格式定义
│   │   ├── image/                # 图像处理
│   │   └── main.rs               # 入口
│   ├── Cargo.toml
│   └── tauri.conf.json
├── public/                       # 静态资源
├── index.html
├── package.json
├── pnpm-lock.yaml
├── tsconfig.json
├── vite.config.ts
└── README.md
```

---

## 开发指南

详细开发说明请参考：

- [开发环境搭建](docs/development.md#开发环境搭建)
- [代码规范](docs/development.md#代码规范)
- [构建流程](docs/development.md#构建流程)
- [测试说明](docs/development.md#测试说明)
- [贡献指南](docs/development.md#贡献指南)

---

## 格式规范

`.crf` 格式的完整技术规范请参考：

- [CRF 格式规范文档](docs/crf-format.md)
- [项目开发、算法与构建标准](docs/project-standards.md)
- [编解码器分层重构规划](docs/codec-architecture-refactor-plan.md)
- [CPU/GPU 性能优化规划](docs/performance-optimization-plan.md)
- [压缩算法与后端探索路线](docs/compression-algorithm-exploration.md)
- [API 接口文档](docs/api.md)
- [技术架构文档](docs/architecture.md)
- [有损目标预设与首帧优化规划](docs/first-frame-optimization-plan.md)
- [有损精细参数接口规划](docs/lossy-tuning-interface-plan.md)

---

## 常见问题

### Q: 为什么选择自定义 .crf 格式而不是标准视频格式？

A: `.crf` 格式专为差分图序列优化，基于 H.264 标准简化而来：

- 仅保留必要的熵编码部分（Golomb-Rice / 指数哥伦布编码）
- 去除帧间预测、运动估计等复杂模块
- 编解码速度更快，实现更简单（约 500-800 行代码）
- 完全自描述，无外部依赖
- 适合科研、嵌入式等资源受限场景

### Q: 支持哪些图像格式？

A: 输入支持 PNG、BMP、TIFF 等常见格式。输出为自定义 `.crf` 格式。

### Q: 最大支持多少帧？

A: 50 帧只是批量接口的内存限制，不是 CRF 文件硬上限。超过 50 帧请使用流式编码 API；
当前码流帧数字段为 `u16`，最高支持 65535 帧。

### Q: 压缩率如何？

A: 针对二次元插画差分数据（大面积不变 + 平坦色块），自适应模式实测比 PNG 基准小约 17%；固定单模式通常为原始大小的 80%~95%。具体取决于插画的绘制风格与差分变化幅度。

---

## 性能指标

| 指标 | 数值 | 测试环境 |
| :--- | :--- | :--- |
| 编码速度（固定模式） | < 100ms/帧 | 1024x1820 RGB, 8bit |
| 编码速度（全自适应无损） | ≈ 260ms/帧（质量优先，多核并行） | PNG1000 实测 |
| 解码速度 | < 50ms/帧 | 位流单向扫描，无决策开销 |
| 无损压缩率（自适应，PNG1000） | 10.63 MB vs PNG 16.64 MB（−36.1%） | CABAC+CfL+三平面全竞争 |
| 真有损压缩率（PNG1000, q75/q50, 色度半分辨率） | 2.87 / 2.38 MB（−83% / −86%） | 对标 AVIF crf30：3.21MB/37.2dB，CRF 更小且 PSNR 持平 |
| 真有损压缩率（c 组 JPEG 源, q50） | 14.65 MB vs 源 43.84 MB（−67%） | 量化滤除源 JPEG 噪声 |
| 噪声感知（纯场景差分-1: gal 天气/时间混合源, q90） | **3.13 MB vs 关闭 4.38 MB（−28.5%）** | 零中心门控软阈值 + 闭环 per-band 自适应步长（PSNR 换体积的激进档） |
| CABAC 三模式竞争（MA 树 / 梯度档 / 统一，全线通用） | 无损自适应 −1.3% / q90 −1.8%（纯场景组） | MA 树数据驱动上下文划分（JPEG-XL 同款），帧内取码流最小者 |
| SIMD 向量化（差分/YCoCg-R/软阈值，AVX2 运行时分派） | 与标量逐位一致，热点算术 4-8× | CRC32 已由 crc32fast PCLMULQDQ 硬件加速 |
| 流式编码 API（>50 帧 / 超大分辨率） | 至多 65535 帧；内存 O(golden+单帧+码流) | 输出与批量编码逐字节一致 |
| 大像素组（PNG4000, 4500×8000×14帧） | 无损 110.77 MB（−38%） | 全部像素级校验通过 |
| 输入格式 | PNG / JPEG / WebP / BMP / TIFF | image-rs 解码 |
| 无损输出格式 | PNG（默认）/ WebP-VP8L | CRF_OUTPUT_FORMAT=webp 切换 |
| 内存占用 | < 50MB | 50帧序列 |
| 启动时间 | < 1秒 | 冷启动 |

**最新标定状态（2026-08-25）**：迭代后的默认有损参数与基准——完整实施
记录见 [docs/optimization-review.md](docs/optimization-review.md) §9~§23。

| 指标 | 数值 | 测试环境 |
| :--- | :--- | :--- |
| 默认调参 | `deadzone_bias=+4`、`chroma_deadzone_bias=Some(-4)`、`chroma_quant_percent=130%` | q90 亮/色度独立死区 |
| 色度半分辨率 | 不受 `step>1` 阻断（4:2:0 解耦） | q95 档首次真实启用 |
| CFL α 候选 | 9 个（±3 补齐） | 跨内容 −4~24%（q75） |
| 色度 band steps | Y 表 step_by(2) 下采样映射 | JPEG 源 −36%（noise_adaptive） |
| AVIF CQ18 对标 | CRF q90 = 3,500,035 B @ 49.86 dB vs AVIF 3,370,055 B @ 37.32 dB | +3.8% 体积 / +12.5 dB 质量 |
| 编码速度（q90） | ≈ 800ms/帧（自适应全候选） | PNG1000 14 帧 |
| 变换域候选 | frame_type=8（预测后变换 + CABAC 系数编码） | v1.14 正式格式化，已接入竞争 |

**双模式说明**：无损与真有损并行可选——`EncodeParams.lossy: Option<LossyOptionsV2>`
是唯一入口（None=无损），V2 提供质量预设、显式量化、色度、感知、首帧、时间参考与
序列码率控制（参考 JPEG/AVIF/WebP/H.264 的率失真工具设计）。

**噪声感知**（V2 `perceptual.noiseMode`）：面向
JPEG/WebP 有损源的差分压缩增强。按条带估计残差失真水平，两级生效：
①零中心性门控软阈值（系统性偏移场与两极化结构场自动零介入）；
②闭环 per-band 自适应步长（死区宽度跟随局部失真水平）。解码端无感、
格式零改动、无损模式不受影响。适用于天气/时间变化等稠密失真场景；
服装/表情类结构差分会自动判定为零介入。

---

## 贡献指南

欢迎贡献代码、报告问题或提出建议！

1. Fork 本仓库
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'Add amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 创建 Pull Request

详细贡献指南请参考 [CONTRIBUTING.md](CONTRIBUTING.md)。

---

## 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](LICENSE) 文件

---

## 致谢

- [Tauri](https://tauri.app/) - 构建跨平台桌面应用
- [React](https://react.dev/) - 用户界面构建
- [image-rs](https://github.com/image-rs/image) - Rust 图像处理库
- H.264 标准 - 算法设计参考

---

<p align="center">
  如果这个项目对你有帮助，请考虑给一个 ⭐ Star！
</p>
