//! bitstream —— 格式契约与容器
//!
//! 规划文档 §3.3。包含：
//! - constants.rs：码流格式常量（原 format/constants.rs，P4 迁入）
//! - header.rs：CrfHeader 文件头（原 format/header.rs，P4 迁入）
//! - version.rs：bitstream version/feature bits
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
//! **迁移状态（P4）**：constants 与 header 模块已迁入
//! （原 format/constants.rs、format/header.rs）。旧文件不再保留。

/// constants：码流格式常量（P4 迁入，原 format/constants.rs）
pub mod constants;

/// header：文件头读写与校验（P4 迁入，原 format/header.rs）
pub mod header;
