//! 容器层 —— 安全读取、CRC 验证、码流边界
//!
//! 规划文档 §5.2。职责：
//! - [`reader`]：bounded bytes/Read+Seek 统一输入；
//! - [`footer`]：Footer/CRC 验证。
//!
//! 容器层只负责字节和边界，不负责预测、DCT、量化或参考恢复。
//! 它应能在不加载完整图像的情况下解析元数据、索引和 payload 范围，
//! 并提供安全的 bounded slice/reader。
//!
//! **迁移状态（P2）**：`footer` 模块已提取（CRC 验证逻辑），
//! `reader` 模块定义了 bounded reader 类型。`decode_from_bytes` 和
//! `decode_from_file` 仍保留在 `decoder/mod.rs` 作为 facade，
//! 内部委托到本模块。解码器不再在 dispatcher 中执行时间参考恢复。
//!
//! 参考：[编解码器分层重构规划](../../../../docs/codec-architecture-refactor-plan.md) §5.2

pub mod footer;
pub mod reader;
