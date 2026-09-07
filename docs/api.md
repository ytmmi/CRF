# CRF Viewer API 接口文档

## 目录

- [概述](#概述)
- [CLI 与 Rust API](#cli-与-rust-api)
- [数据结构定义](#数据结构定义)
- [错误处理](#错误处理)

---

## 概述

CRF Viewer 是**纯 Rust CLI 工具**（无前端、无 Tauri）。编解码能力通过
`src-tauri/src/main.rs` 的命令行入口与 `crf::codec` facade（Rust 层唯一稳定入口）
暴露，JSON 仅用于 `--expert-config` 输入与 `--dump-*` 导出。

> ⚠️ **历史规划说明**：本文档下文中仍保留早期规划的 `#[tauri::command]` /
> TypeScript 接口描述。项目现状为纯 CLI，**这些接口全部未实现**，仅作为未来 UI
> 的参考，不代表当前 API 承诺。

### 调用方式（当前）

```bash
# CLI 调用（当前唯一方式）
cargo run --manifest-path src-tauri/Cargo.toml -- --lossy-quality 96.50 --test <dir>

# Rust API 调用（facade）
let report = crf::codec::encode(request)?;
let result = crf::codec::decode_from_bytes(data)?;
let resolved = crf::codec::resolve_lossy_json(request_json)?;
let schema  = crf::codec::lossy_expert_schema_json()?;
```

---

## Tauri 命令接口（历史规划，未实现）

> 以下 `#[tauri::command]` 与 `invoke()` 示例均为早期规划，项目当前**没有** Tauri
> 命令系统。保留本节仅为未来 UI 提供接口形态参考。

### 文件操作

#### `open_file`

打开文件并返回元数据。

```rust
#[tauri::command]
pub fn open_file(path: String) -> Result<FileMetadata, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `path` | String | 文件绝对路径 |

**返回值**：`FileMetadata`

```typescript
{
  path: string;       // 文件路径
  name: string;       // 文件名
  size: number;       // 文件大小（字节）
  modified: string;   // 修改时间（ISO 8601）
  type: 'image' | 'crf' | 'unknown';  // 文件类型
}
```

**示例**：

```typescript
const metadata = await invoke<FileMetadata>('open_file', { 
  path: '/Users/test/image.png' 
});
console.log(`文件大小: ${metadata.size} bytes`);
```

---

### 编解码命令

#### `encode_crf`

将图像序列编码为 CRF 格式。

```rust
#[tauri::command]
pub fn encode_crf(
    frames: Vec<ImageData>, 
    params: EncodeParams
) -> Result<Vec<u8>, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `frames` | ImageData[] | 批量图像帧数组（2~50 帧）；更长序列使用流式编码 API |
| `params` | EncodeParams | 编码参数 |

**EncodeParams 结构**：

```typescript
{
  compressionType: 'golomb-rice' | 'exp-golomb' | 'transform';
  blockSize?: number;      // 变换块大小（仅 transform 模式）
  userMetadata?: string;   // 用户自定义元数据（最多40字节）
  lossy?: LossyOptionsV2;  // 唯一有损配置入口；省略即无损
}
```

### 有损 V2 配置 API

Rust/SDK 提供：

```rust
LossyOptionsV2Builder::preset(9650)
    .chroma_sampling(ChromaSampling::Cs420)
    .first_frame_offset(50)
    .effort(8)
    .build()?;

options.validate()?;
let report = options.resolve_without_encoding()?;
let canonical_json = crf::codec::resolve_lossy_json(request_json)?;
let panel_schema = crf::codec::lossy_expert_schema_json()?;
```

JSON 支持文档中的人类小数字段（例如 `quality: 96.5`、`lumaStep: 1.375`），解析时先以
十进制定点转换，再进入 V2；canonical/resolved JSON 使用带单位后缀的整数，例如
`qualityX100: 9650`。未知字段、超精度小数和范围错误均明确报错。

当前 CLI 用户入口：

```text
--lossy-quality 96.50
--target-bytes 3370055
--first-frame-quality-offset 0.50
--chroma-sampling auto|444|422|420
--perceptual-strength 0..200
--effort 0..10
--expert-config path/to/lossy-v2.json
--dump-resolved-config path/to/resolved.json
--dump-expert-schema path/to/panel-schema.json
```

仓库当前没有 React/Tauri 前端实现；`lossy_expert_schema_json()` 提供高级/实验面板的字段、
单位、范围、枚举选项与影响说明，后续前端不得复制另一份默认和校验逻辑。

**返回值**：`Vec<u8>` - 编码后的 CRF 文件数据

**示例**：

```typescript
const frames: ImageData[] = [frame1, frame2, frame3];
const params: EncodeParams = {
  compressionType: 'golomb-rice',
  userMetadata: 'test sequence'
};

const crfData = await invoke<number[]>('encode_crf', { frames, params });
```

---

#### `decode_crf`

将 CRF 数据解码为图像序列。

```rust
#[tauri::command]
pub fn decode_crf(data: Vec<u8>) -> Result<DecodeResult, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `data` | number[] | CRF 文件数据 |

**返回值**：`DecodeResult`

```typescript
{
  metadata: CrfMetadata;    // 文件元数据
  frames: ImageData[];       // 解码后的帧数据
}
```

**示例**：

```typescript
const fileBytes = await readBinaryFile('test.crf');
const result = await invoke<DecodeResult>('decode_crf', { 
  data: Array.from(fileBytes) 
});

console.log(`解码了 ${result.frames.length} 帧`);
```

---

#### `get_crf_metadata`

获取 CRF 文件元数据（不解码帧数据）。

```rust
#[tauri::command]
pub fn get_crf_metadata(data: Vec<u8>) -> Result<CrfMetadata, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `data` | number[] | CRF 文件数据（或前64字节） |

**返回值**：`CrfMetadata`

```typescript
{
  version: [number, number];   // 版本号 [主版本, 次版本]
  frameCount: number;           // 帧总数
  width: number;                // 图像宽度
  height: number;               // 图像高度
  bitDepth: number;             // 像素位深
  colorFormat: string;          // 色彩格式
  compressionType: string;      // 压缩类型
  hasIndex: boolean;            // 是否包含帧索引
  userData: string;             // 用户自定义数据
  frames: FrameInfo[];          // 帧信息列表
}
```

---

### 图像处理命令

#### `load_image`

加载图像文件。

```rust
#[tauri::command]
pub fn load_image(path: String) -> Result<ImageData, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `path` | String | 图像文件路径 |

**返回值**：`ImageData`

---

#### `compute_residual`

计算两帧之间的差值（残差）。

```rust
#[tauri::command]
pub fn compute_residual(
    base: ImageData, 
    target: ImageData
) -> Result<ImageData, String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `base` | ImageData | 基准帧 |
| `target` | ImageData | 目标帧 |

**返回值**：`ImageData` - 残差帧（target - base）

**说明**：

- 两帧必须具有相同的宽高、位深和色彩格式
- 残差值可能为负数，存储为有符号整数

---

#### `save_image`

保存图像到文件。

```rust
#[tauri::command]
pub fn save_image(
    data: ImageData, 
    path: String, 
    format: String
) -> Result<(), String>
```

**参数**：

| 参数 | 类型 | 说明 |
| :--- | :--- | :--- |
| `data` | ImageData | 图像数据 |
| `path` | String | 保存路径 |
| `format` | String | 图像格式（'png', 'bmp', 'tiff'） |

---

## 数据结构定义

### ImageData

表示一帧图像数据。

```typescript
interface ImageData {
  width: number;      // 图像宽度（像素）
  height: number;     // 图像高度（像素）
  bitDepth: number;   // 像素位深（8, 10, 12, 16）
  colorFormat: ColorFormat;
  pixels: number[];   // 一维像素数组（行优先存储）
}

type ColorFormat = 
  | 'gray'     // 灰度
  | 'rgb'      // RGB
  | 'yuv444'   // YUV 4:4:4
  | 'yuv422'   // YUV 4:2:2
  | 'yuv420';  // YUV 4:2:0
```

**像素数组布局**：

- 灰度：`[Y0, Y1, Y2, ...]`
- RGB：`[R0, G0, B0, R1, G1, B1, ...]`
- YUV444：`[Y0, U0, V0, Y1, U1, V1, ...]`

### EncodeParams

编码参数配置。

```typescript
interface EncodeParams {
  compressionType: CompressionType;
  blockSize?: number;       // 默认 8
  userMetadata?: string;    // 最多 40 字节 UTF-8
}

type CompressionType = 
  | 'golomb-rice'   // Golomb-Rice 编码
  | 'exp-golomb'    // 指数哥伦布编码
  | 'transform';    // 变换 + 熵编码
```

### CrfMetadata

CRF 文件元数据。

```typescript
interface CrfMetadata {
  version: [number, number];   // [1, 0]
  frameCount: number;           // 2 ~ 50
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
  index: number;    // 帧序号（0-based）
  offset: number;   // 文件偏移量
  size: number;     // 帧数据大小（字节）
  type: string;     // 帧类型
}
```

### FileMetadata

文件元数据。

```typescript
interface FileMetadata {
  path: string;
  name: string;
  size: number;
  modified: string;
  type: 'image' | 'crf' | 'unknown';
}
```

### DecodeResult

解码结果。

```typescript
interface DecodeResult {
  metadata: CrfMetadata;
  frames: ImageData[];
}
```

---

## 错误处理

所有命令在失败时返回 `Err(String)`，错误信息为人类可读的描述。

### 常见错误类型

| 错误信息 | 说明 |
| :--- | :--- |
| `Invalid magic number` | 文件魔数不匹配，不是有效的 CRF 文件 |
| `Unsupported version: x.y` | 文件版本不支持 |
| `Frame count out of range` | 当前入口的帧数越界；批量入口为 2~50 帧，流式入口可超过 50 帧 |
| `Image dimensions mismatch` | 图像尺寸不一致 |
| `Bit depth not supported` | 不支持的位深 |
| `Invalid compression type` | 无效的压缩类型 |
| `CRC checksum failed` | 校验和验证失败 |
| `File not found` | 文件不存在 |
| `Permission denied` | 无权限访问文件 |

### 前端错误处理（历史规划，未实现）

> 无前端，以下示例仅为未来 UI 参考。

---

## 前端服务封装（历史规划，未实现）

> 项目当前**没有** `src/services/tauriCommands.ts` 或任何前端源码。以下为未来 UI
> 的可选封装形态，不代表已实现。

```typescript
// services/tauriCommands.ts
import { invoke } from '@tauri-apps/api/core';
import type { 
  FileMetadata, CrfMetadata, ImageData, 
  EncodeParams, DecodeResult 
} from '../types/crf';

export const CrfApi = {
  async openFile(path: string): Promise<FileMetadata> {
    return invoke('open_file', { path });
  },

  async encodeCrf(frames: ImageData[], params: EncodeParams): Promise<number[]> {
    return invoke('encode_crf', { frames, params });
  },

  async decodeCrf(data: number[]): Promise<DecodeResult> {
    return invoke('decode_crf', { data });
  },

  async getCrfMetadata(data: number[]): Promise<CrfMetadata> {
    return invoke('get_crf_metadata', { data });
  },

  async loadImage(path: string): Promise<ImageData> {
    return invoke('load_image', { path });
  },

  async computeResidual(base: ImageData, target: ImageData): Promise<ImageData> {
    return invoke('compute_residual', { base, target });
  },

  async saveImage(data: ImageData, path: string, format: string): Promise<void> {
    return invoke('save_image', { data, path, format });
  },
};
```
