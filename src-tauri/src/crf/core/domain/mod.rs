//! domain —— 数据模型与不变量
//!
//! 规划文档 §3.1。包含不包含编码决策的公共领域模型：
//! - image.rs：ImagePlane/ImageFrame、尺寸、stride、位深
//! - pixel.rs：分量、范围、符号和饱和规则
//! - frame.rs：FrameKind、ReferenceKind、重建帧元数据
//! - tile.rs：TileRect、边界、邻域窗口
//! - coefficient.rs：CoefficientBlock、扫描位置、EOB 元数据
//! - error.rs：领域错误，不包含 Tauri 字符串
//!
//! **P0 状态**：空骨架。现有 `format/types.rs` 的 `ImageData`/`ColorFormat`/
//! `CompressionType`/`PredictionMode` 等类型将在 P3/P4 迁移到本模块。
//! 迁移期通过 `core::domain` re-export 保持外部路径兼容。

#![allow(dead_code)]

// P3/P4 阶段迁入子模块（当前为空声明，避免空文件警告）
