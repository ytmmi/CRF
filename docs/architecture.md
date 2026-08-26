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
- [前后端交互](#前后端交互)
- [核心算法](#核心算法)

---

## 系统概述

CRF Viewer 是一个基于 Tauri 2 框架构建的跨平台桌面应用，用于差分图片序列的无损压缩、存储和查看。系统采用 Rust 后端 + React 前端的架构，实现了高性能的图像处理和用户友好的交互界面。

### 设计目标

1. **高性能**：快速编解码，流畅的用户交互
2. **无损保证**：像素级精确还原
3. **跨平台**：Windows、macOS、Linux 统一代码库
4. **轻量级**：最小化外部依赖，自包含实现

---

## 整体架构

```
+=====================================================================+
|                        CRF Viewer 应用                               |
+=====================================================================+
|                                                                      |
|  +----------------------------------------------------------------+  |
|  |                    React 前端层                                  |  |
|  |  +-------------+ +-------------+ +---------------------------+  |  |
|  |  |   UI 组件   | | 状态管理    | |   服务层                   |  |  |
|  |  |  (Views)    | | (Zustand)   | | (Tauri Commands)          |  |  |
|  |  +-------------+ +-------------+ +---------------------------+  |  |
|  +----------------------------------------------------------------+  |
|                              |                                       |
|                              v                                       |
|  +----------------------------------------------------------------+  |
|  |                    Tauri IPC 层                                  |  |
|  |           (前后端通信桥接 - JSON 序列化)                         |  |
|  +----------------------------------------------------------------+  |
|                              |                                       |
|                              v                                       |
|  +----------------------------------------------------------------+  |
|  |                    Rust 后端层                                    |  |
|  |  +-------------+ +-------------+ +---------------------------+  |  |
|  |  |  命令处理   | |  图像处理   | |   CRF 编解码器             |  |  |
|  |  | (Commands)  | | (image-rs)  | | (Encoder/Decoder)         |  |  |
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

---

## 模块划分

### 1. 前端模块 (React)

```
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

**职责说明**：

| 模块 | 职责 |
| :--- | :--- |
| `ImageViewer` | 渲染图像、支持缩放/平移/对比 |
| `SequencePanel` | 序列导航、帧选择、播放控制 |
| `MetadataView` | 显示文件头、帧信息、编码参数 |
| `EncodeDialog` | 编码参数配置、进度显示 |
| `tauriCommands` | 封装所有后端调用，提供类型安全的 API |

### 2. 后端模块 (Rust)

```
src-tauri/src/
+-- main.rs              # 应用入口
+-- commands/            # Tauri 命令
|   +-- file.rs          # 文件操作命令
|   +-- codec.rs         # 编解码命令
|   +-- image.rs         # 图像处理命令
+-- crf/                 # CRF 格式核心
|   +-- mod.rs           # 模块导出
|   +-- format.rs        # 格式定义和常量
|   +-- header.rs        # 文件头解析
|   +-- encoder.rs       # 编码器实现
|   +-- decoder.rs       # 解码器实现
|   +-- checksum.rs      # 校验和计算
+-- image/               # 图像处理
|   +-- mod.rs
|   +-- loader.rs        # 图像加载
|   +-- processor.rs     # 图像预处理
+-- error.rs             # 错误类型定义
```

**职责说明**：

| 模块 | 职责 |
| :--- | :--- |
| `commands` | 暴露给前端的 Tauri API |
| `crf::encoder` | 将图像序列编码为 .crf 文件 |
| `crf::decoder` | 将 .crf 文件解码为图像序列 |
| `crf::format` | 格式常量、结构体定义 |
| `image` | 图像加载、格式转换、预处理 |

### 3. CRF 编解码核心

```
crf/
+-- format.rs            # 格式规范
|   +-- FILE_MAGIC       # 魔数定义
|   +-- HEADER_SIZE      # 头部大小
|   +-- CRF 结构体定义
+-- header.rs            # 头部处理
|   +-- parse_header()   # 解析头部
|   +-- write_header()   # 写入头部
+-- encoder.rs           # 编码器
|   +-- encode_frame()   # 单帧编码
|   +-- encode_sequence()# 序列编码
+-- decoder.rs           # 解码器
|   +-- decode_frame()   # 单帧解码
|   +-- decode_sequence()# 序列解码
+-- checksum.rs          # 校验
    +-- crc32()          # CRC32 计算
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

1. **图像加载**：使用 `image-rs` 读取 PNG/BMP/TIFF 文件
2. **格式统一**：转换为统一的像素格式（RGB/灰度，8/10/12/16位）
3. **残差计算**：如果输入是多张图像，计算相邻帧差值生成残差帧
4. **熵编码**：
   - Zigzag 扫描将二维数据转为一维
   - 使用 Golomb-Rice 或指数哥伦布编码压缩
5. **文件组装**：写入文件头 + 帧索引 + 编码数据 + 文件尾

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

### 3. 用户交互流程

```
+================================================================+
|                         用户界面                                |
+================================================================+
|                                                                 |
|   +----------+      +----------+      +----------+             |
|   | 拖拽导入 | ==>  | 选择文件 | ==>  | 预览确认 |             |
|   +----------+      +----------+      +----------+             |
|                                                 |              |
|                                                 v              |
|   +----------+      +----------+      +----------+             |
|   | 保存结果 | <==  | 压缩处理 | <==  | 设置参数 |             |
|   +----------+      +----------+      +----------+             |
|                                                                 |
+================================================================+
```

---

## 前后端交互

### Tauri 命令接口

前后端通过 Tauri 的命令系统进行通信。前端调用 Rust 函数，通过 JSON 序列化传递数据。

#### 文件操作命令

```rust
#[tauri::command]
pub fn open_file(path: String) -> Result<FileMetadata, String>

#[tauri::command]
pub fn save_file(data: Vec<u8>, path: String) -> Result<(), String>
```

#### 编解码命令

```rust
#[tauri::command]
pub fn encode_crf(frames: Vec<ImageData>, params: EncodeParams) 
    -> Result<Vec<u8>, String>

#[tauri::command]
pub fn decode_crf(data: Vec<u8>) -> Result<DecodeResult, String>

#[tauri::command]
pub fn get_crf_metadata(data: Vec<u8>) -> Result<CrfMetadata, String>
```

#### 图像处理命令

```rust
#[tauri::command]
pub fn load_image(path: String) -> Result<ImageData, String>

#[tauri::command]
pub fn compute_residual(base: ImageData, target: ImageData) 
    -> Result<ImageData, String>
```

### 数据结构定义

#### 前端 -> 后端

```typescript
// 图像数据
interface ImageData {
  width: number;
  height: number;
  bitDepth: number;
  colorFormat: 'gray' | 'rgb' | 'yuv444' | 'yuv422' | 'yuv420';
  pixels: number[];  // 一维像素数组
}

// 编码参数
interface EncodeParams {
  compressionType: 'golomb-rice' | 'exp-golomb' | 'transform';
  blockSize?: number;
  userMetadata?: string;
}
```

#### 后端 -> 前端

```typescript
// CRF 元数据
interface CrfMetadata {
  version: [number, number];
  frameCount: number;
  width: number;
  height: number;
  bitDepth: number;
  colorFormat: string;
  compressionType: string;
  hasIndex: boolean;
  userData: string;
  frames: FrameInfo[];
}

interface FrameInfo {
  index: number;
  offset: number;
  size: number;
  type: string;
}
```

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
