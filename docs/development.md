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
| Node.js | >= 18.0 | 前端运行时 |
| pnpm | >= 8.0 | 包管理器 |
| Rust | >= 1.75 | 后端语言 |
| Tauri CLI | >= 2.0 | Tauri 命令行工具 |

### Windows 环境

```powershell
# 安装 Rust（如果未安装）
winget install Rustlang.Rustup

# 安装 Visual Studio Build Tools
winget install Microsoft.VisualStudio.2022.BuildTools

# 安装 Tauri 依赖
cargo install tauri-cli

# 安装 Node.js（如果未安装）
winget install OpenJS.NodeJS.LTS
```

### macOS 环境

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 Xcode Command Line Tools
xcode-select --install

# 安装 Tauri CLI
cargo install tauri-cli

# 安装 Node.js（使用 nvm）
brew install nvm
nvm install 18
nvm use 18
```

### Linux 环境 (Ubuntu/Debian)

```bash
# 安装系统依赖
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  patchelf

# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 Tauri CLI
cargo install tauri-cli

# 安装 Node.js
curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
sudo apt install -y nodejs
```

### 克隆项目

```bash
git clone https://github.com/your-username/crf-viewer.git
cd crf-viewer
```

### 安装依赖

```bash
# 安装前端依赖
pnpm install

# 验证 Rust 环境
cargo check
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
+-- src/                          # 前端源码 (React)
|   +-- components/               # UI 组件
|   +-- hooks/                    # 自定义 Hooks
|   +-- services/                 # 前端服务
|   +-- stores/                   # 状态管理
|   +-- types/                    # TypeScript 类型定义
|   +-- utils/                    # 工具函数
+-- src-tauri/                    # Rust 后端
|   +-- src/
|   |   +-- commands/             # Tauri 命令
|   |   +-- crf/                  # CRF 格式处理
|   |   +-- image/                # 图像处理
|   |   +-- main.rs               # 入口
|   +-- Cargo.toml
|   +-- tauri.conf.json
+-- public/                       # 静态资源
+-- index.html
+-- package.json
+-- pnpm-lock.yaml
+-- tsconfig.json
+-- vite.config.ts
+-- .eslintrc.cjs
+-- .prettierrc
```

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

// 命令返回 Result<T, String>
#[tauri::command]
pub fn encode_crf(...) -> Result<Vec<u8>, String> {
    // 使用 ? 操作符
    let data = encode_inner(...).map_err(|e| e.to_string())?;
    Ok(data)
}
```

### TypeScript/React 代码规范

使用 ESLint + Prettier 格式化。

```bash
# 格式化代码
pnpm format

# 检查代码
pnpm lint
```

**命名规范**：

- 组件：PascalCase（如 `ImageViewer`、`SequencePanel`）
- 函数/变量：camelCase（如 `loadImage`、`frameCount`）
- 类型/接口：PascalCase（如 `ImageData`、`EncodeParams`）
- 常量：SCREAMING_SNAKE_CASE 或 camelCase

**组件规范**：

```tsx
// 使用函数组件和 Hooks
interface ImageViewerProps {
  image: ImageData;
  onFrameChange?: (index: number) => void;
}

export const ImageViewer: React.FC<ImageViewerProps> = ({ 
  image, 
  onFrameChange 
}) => {
  // Hooks 放在最前面
  const [scale, setScale] = useState(1);
  
  // 事件处理
  const handleZoom = useCallback((delta: number) => {
    setScale(prev => prev + delta);
  }, []);
  
  // 渲染
  return (
    <div className="image-viewer">
      {/* ... */}
    </div>
  );
};
```

---

## 构建流程

### 开发模式

```bash
# 启动开发服务器（前端热重载 + Rust 后端）
pnpm tauri dev
```

开发模式下：
- 前端代码修改后自动热重载
- Rust 代码修改后自动重新编译
- 应用窗口自动刷新

### 构建发布版本

```bash
# 构建当前平台的发布版本
pnpm tauri build
```

构建产物位于 `src-tauri/target/release/bundle/`：

| 平台 | 产物路径 |
| :--- | :--- |
| Windows | `msi/*.msi` / `nsis/*.exe` |
| macOS | `dmg/*.dmg` / `macos/*.app` |
| Linux | `deb/*.deb` / `appimage/*.AppImage` |

### 仅构建前端

```bash
# 构建前端资源（不打包 Tauri）
pnpm build
```

### 仅构建 Rust

```bash
# 构建 Rust 后端
cargo build --release
```

---

## 测试说明

### 前端测试

```bash
# 运行所有测试
pnpm test

# 运行测试并生成覆盖率报告
pnpm test:coverage

# 监听模式
pnpm test:watch
```

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
pnpm tauri build
cargo test
pnpm test
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
