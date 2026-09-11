# 贡献指南

感谢参与 CRF！提交代码前，请先阅读并遵守
[《CRF 项目开发、算法与构建标准》](docs/project-standards.md)（强制准入）。

## 快速开始

```bash
git clone https://github.com/ytmmi/CRF.git
cd CRF
cargo check --manifest-path src-tauri/Cargo.toml --workspace
```

- 工具链由仓库根 `rust-toolchain.toml` 锁定（当前 Rust 1.96.1）。
- 本项目为**纯 Rust CLI**：无 Node.js / pnpm / Tauri CLI / 前端依赖。

## 提交前门禁

按 `docs/project-standards.md` §9.2 的顺序在本地执行：

```powershell
pwsh scripts/check_file_lines.ps1          # 源码行数（>1000 硬失败，>=800 预警）
pwsh scripts/check_layer_dependencies.ps1  # 分层依赖方向
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --workspace
```

CI（`.github/workflows/ci.yml`）会在 push / PR 时执行相同门禁，请确保本地通过。

## 提交规范

使用 [Conventional Commits](https://www.conventionalcommits.org/)：

```text
<type>(<scope>): <description>
```

| 类型 | 说明 |
| :--- | :--- |
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档更新 |
| `style` | 代码格式（不影响功能） |
| `refactor` | 重构 |
| `test` | 测试相关 |
| `perf` | 性能优化 |
| `chore` | 构建 / 工具相关 |

示例：

```text
perf(crf): batched 批大小可覆盖（CRF_BATCH_FRAMES）用于调优/诊断(v0.3.3.18)
```

## 版本号

版本号采用四段式 `a.b.c.d`，每次 push 前必须递增，禁止同一版本号重复推送。
递增规则与四处同步位置见 [docs/project-standards.md §13](docs/project-standards.md)。

## 分支与流程

1. Fork 本仓库
2. 创建特性分支（`feature/*`、`fix/*`）
3. 提交更改
4. 推送到分支
5. 创建 Pull Request

## Code Review

所有 PR 需至少一名维护者审核，检查：代码风格、测试充分性、文档更新、性能与兼容性影响。

详细开发说明见 [docs/development.md](docs/development.md)。
