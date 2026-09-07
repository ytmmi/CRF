# 开发文档

> **强制准入**：所有后续实现、测试和构建必须先满足
> [《CRF 项目开发、算法与构建标准》](project-standards.md)。本页命令仅说明如何运行
> 工具，构建成功不能替代文件规模、单一职责、正确性和兼容性门禁。

## 目录

- [开发环境搭建](#开发环境搭建)
- [项目结构](#项目结构)
- [代码规范](#代码规范)
- [构建流程](#构建流程)
- [测试说明](#测试说明)
- [贡献指南](#贡献指南)

---

## 开发环境搭建

### 系统要求

| 依赖 | 版本要求 | 说明 |
| :--- | :--- | :--- |
| Rust | >= 1.75 | 唯一构建语言（纯 CLI，无前端） |
| Cargo | 随 Rust | 包管理器/构建工具 |

> 本项目为**纯 CLI**：无 Node.js / pnpm / Tauri CLI / 前端依赖。

### Windows 环境

```powershell
# 安装 Rust（如果未安装）
winget install Rustlang.Rustup

# 安装 Visual Studio Build Tools（Rust MSVC 工具链需要）
winget install Microsoft.VisualStudio.2022.BuildTools
```

### macOS 环境

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 Xcode Command Line Tools
xcode-select --install
```

### Linux 环境 (Ubuntu/Debian)

```bash
# 安装系统依赖
sudo apt update
sudo apt install -y build-essential pkg-config

# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 克隆项目

```bash
git clone https://github.com/your-username/crf-viewer.git
cd crf-viewer
```

### 安装依赖

```bash
# 验证 Rust 环境并拉取依赖（crates.io）
cargo check --manifest-path src-tauri/Cargo.toml --workspace
```

---

## 项目结构

```
crf-viewer/
+-- docs/                          # 文档目录
|   +-- architecture.md           # 技术架构文档
|   +-- api.md                    # API 接口文档
|   +-- crf-format.md             # CRF 格式规范
|   +-- development.md            # 开发指南
|   +-- project-standards.md      # 强制项目开发、算法与构建标准
|   +-- codec-architecture-refactor-plan.md # 编解码器分层重构规划
|   +-- performance-optimization-plan.md # CPU/GPU 性能规划
|   +-- user-guide.md             # 用户手册
+-- src-tauri/                    # Rust 源码（纯 CLI，无 Tauri）
|   +-- src/
|   |   +-- main.rs               # CLI 入口（参数解析/测试/基准/探针）
|   |   +-- cli_config.rs         # CLI 配置解析（有损 V2 flags / 专家配置）
|   |   +-- crf/                  # CRF 格式核心（codec/core/encoder/decoder/backend/performance）
|   |   +-- test/                 # 集成测试（批量/流式/探针）
|   +-- cuda-runtime/             # CUDA 旁路 DLL 运行时
|   +-- Cargo.toml
+-- test/                         # 端到端测试图像组
+-- scripts/                      # 辅助脚本
```

> 无 `src/` 前端、`package.json`、`vite.config.ts` 等前端文件。

---

## 代码规范

### Rust 代码规范

遵循 Rust 官方风格指南，使用 `rustfmt` 格式化。

```bash
# 格式化代码
cargo fmt

# 检查代码
cargo clippy -- -D warnings
```

**命名规范**：

- 结构体/枚举：PascalCase（如 `CrfHeader`、`CompressionType`）
- 函数/方法：snake_case（如 `encode_frame`、`parse_header`）
- 常量：SCREAMING_SNAKE_CASE（如 `FILE_MAGIC`、`HEADER_SIZE`）
- 模块：snake_case

**错误处理**：

```rust
// 使用自定义错误类型
#[derive(Debug, thiserror::Error)]
pub enum CrfError {
    #[error("Invalid magic number")]
    InvalidMagic,

    #[error("Unsupported version: {0}.{1}")]
    UnsupportedVersion(u16, u16),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// CLI/API 返回 Result<T, CrfError>
fn encode_crf(...) -> Result<Vec<u8>, CrfError> {
    // 使用 ? 操作符
    let data = encode_inner(...)?;
    Ok(data)
}
```

> 项目为纯 Rust：无 TypeScript/React 代码规范（历史规划中的前端规范章节已废弃）。

---

## 构建流程

### 开发模式

```bash
# 以 debug 构建并运行（默认启用 nvidia-cuda feature；无设备自动回退 CPU）
cargo run --manifest-path src-tauri/Cargo.toml -- --help
```

开发模式下：
- Rust 代码修改后重新 `cargo run` / `cargo test` 即可
- 无前端、无热重载、无应用窗口

### 构建发布版本

```bash
# 构建当前平台的发布版本（workspace 同时产出 EXE 与 CUDA DLL）
cargo build --manifest-path src-tauri/Cargo.toml --workspace --release --features nvidia-cuda
```

构建产物位于 `src-tauri/target/release/`：

```text
src-tauri/target/release/
  crf-viewer.exe
  crf_cuda.dll
```

`crf_cuda.dll` 是应用自己的 CUDA 适配旁路库，必须与 `crf-viewer.exe` 一起复制到安装
目录或压缩包；主程序找不到该 DLL、找不到 NVIDIA 驱动或 GPU 执行失败时，会自动回退 CPU。
`nvcuda.dll` 由 NVIDIA 驱动安装提供，不作为应用文件复制；目标机不需要 CUDA Toolkit。
发布前至少检查以下项目：

```powershell
Test-Path src-tauri/target/release/crf-viewer.exe
Test-Path src-tauri/target/release/crf_cuda.dll
$env:CRF_CUDA_DLL = (Resolve-Path src-tauri/target/release/crf_cuda.dll)
cargo test --manifest-path src-tauri/Cargo.toml --features nvidia-cuda `
  crf::backend::gpu::cuda::tests::cuda_diff_matches_scalar_when_driver_is_available -- --nocapture
```

发布包不能只复制 EXE；无论使用何种安装器，必须将 `crf_cuda.dll` 配置为与主 EXE
同目录的 sidecar/resource，并在安装后目录中复核文件存在。

---

## 测试说明

### Rust 测试

```bash
# 运行所有测试
cargo test

# 运行特定模块测试
cargo test crf::encoder

# 显示测试输出
cargo test -- --nocapture
```

### 集成测试

```bash
# 运行完整的构建和测试流程
cargo build --manifest-path src-tauri/Cargo.toml --workspace --release
cargo test --manifest-path src-tauri/Cargo.toml --workspace

# 端到端图像组测试（编码→解码→逐像素校验）
cargo run --manifest-path src-tauri/Cargo.toml -- --test test/png/1000
```

### 测试文件结构

```
tests/
+-- crf_encoder_test.rs      # 编码器测试
+-- crf_decoder_test.rs      # 解码器测试
+-- format_test.rs           # 格式解析测试
+-- fixtures/                # 测试数据
    +-- sample_8bit.crf
    +-- sample_16bit.crf
    +-- invalid_magic.crf
```

### 编写测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encode_decode_roundtrip() {
        // 准备测试数据
        let frames = create_test_frames(3, 100, 100);
        let params = EncodeParams::default();
        
        // 编码
        let encoded = encode_sequence(&frames, &params).unwrap();
        
        // 解码
        let decoded = decode_sequence(&encoded).unwrap();
        
        // 验证
        assert_eq!(frames.len(), decoded.len());
        for (original, restored) in frames.iter().zip(decoded.iter()) {
            assert_eq!(original.pixels, restored.pixels);
        }
    }
}
```

---

## 贡献指南

### 提交规范

使用 [Conventional Commits](https://www.conventionalcommits.org/) 规范：

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

**类型**：

| 类型 | 说明 |
| :--- | :--- |
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档更新 |
| `style` | 代码格式（不影响功能） |
| `refactor` | 重构 |
| `test` | 测试相关 |
| `chore` | 构建/工具相关 |

**示例**：

```
feat(crf): add Golomb-Rice encoder implementation

- Implement Golomb-Rice encoding for residual frames
- Add adaptive k parameter selection
- Add unit tests for encoder

Closes #12
```

### 分支规范

- `main` - 稳定分支，用于发布
- `develop` - 开发分支，功能集成
- `feature/*` - 功能分支
- `fix/*` - 修复分支
- `release/*` - 发布分支

### 提交流程

1. Fork 本仓库
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'feat: add amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 创建 Pull Request

### Code Review

所有 PR 需要至少一个维护者审核。审核检查：

- 代码风格是否符合规范
- 测试是否充分
- 文档是否更新
- 是否有潜在的性能问题
