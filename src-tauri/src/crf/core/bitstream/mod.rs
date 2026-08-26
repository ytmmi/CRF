//! bitstream —— 格式契约与容器
//!
//! 规划文档 §3.3。包含：
//! - version.rs：bitstream version/feature bits
//! - header.rs：Header/Flags 编解码
//! - index.rs：FrameIndexEntry
//! - frame_header.rs：FrameHeader/coding_params
//! - footer.rs：Footer/CRC 语义
//! - reader.rs：bounded reader/bit reader
//! - writer.rs：bounded writer/bit writer
//! - payload_registry.rs：frame type 与 decoder handler 注册
//! - validate.rs：长度、偏移、保留位、兼容性
//!
//! 容器层只负责"字节和边界"，不负责预测、DCT、量化或参考恢复。
//! 它应能在不加载完整图像的情况下解析元数据、索引和 payload 范围，
//! 并提供安全的 bounded slice/reader。
//!
//! **P0 状态**：空骨架。现有 `format/header.rs`、`format/constants.rs`、
//! `format/types.rs` 的 `CrfHeader`/`FrameHeader`/`FrameIndexEntry`/`Flags`
//! 将在 P2 迁移到本模块。迁移期通过 re-export 保持兼容。

#![allow(dead_code)]

// P2 阶段迁入子模块（当前为空声明）
