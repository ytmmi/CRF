//! 码流格式常量（原 format/constants.rs，P4 架构迁移）
//!
//! 规划文档 §3.3。包含 CRF 码流格式的固定常量：
//! - 文件魔数、文件尾魔数
//! - 文件头/帧头/文件尾大小
//! - 帧数范围、版本号、用户数据长度限制
//!
//! **迁移说明（P4）**：本模块原位于 `format/constants.rs`，现迁移到
//! `core/bitstream/constants`。`format/constants.rs` 保留为 `pub use` 转发层。
//! 新代码请直接使用 `core::bitstream::constants`。

/// 文件魔数: "CRF\0"
pub const FILE_MAGIC: [u8; 4] = [0x43, 0x52, 0x46, 0x00];

/// 文件尾魔数: "CRF\xFF"
pub const FOOTER_MAGIC: [u8; 4] = [0x43, 0x52, 0x46, 0xFF];

/// 文件头大小（字节）
pub const HEADER_SIZE: usize = 64;

/// 帧头大小（字节）：frame_size(u32) + pixel_count(u32) + frame_type(u8) + coding_params(u8) + pred_mode(u8) + reference_type(u8)
///
/// v1.15 破坏式更新：新增第 6 字段 `reference_type`（0=golden / 1=previous /
/// 2=prev2 / 3=保留），替代 coding_params.bit7 的 golden 单 bit 语义，
/// 支持多参考帧（previous 之外的前前帧 prev2）。11 → 12 字节。
pub const FRAME_HEADER_SIZE: usize = 12;

/// 条带级自适应预测的条带高度（行数）
///
/// 32 行在"模式切换灵活性"与"条带头边信息/位流对齐开销"间取得平衡；
/// 编码端与解码端共用此常量，保证码流布局一致。
pub const BAND_HEIGHT: usize = 32;

/// 文件尾大小（字节）
pub const FOOTER_SIZE: usize = 8;

/// 最小帧数
pub const MIN_FRAMES: u16 = 2;

/// 最大帧数
pub const MAX_FRAMES: u16 = 65535;

/// 当前主版本号
pub const VERSION_MAJOR: u16 = 1;

/// 当前次版本号
///
/// v1.15（2026-09-07 破坏式更新）：帧头 11→12 字节（新增 reference_type 字段，
/// 多参考帧 prev2 信令）。旧文件（v1.14 及以下）不再兼容。
pub const VERSION_MINOR: u16 = 4;

/// 用户数据最大长度（字节）
pub const USER_DATA_MAX_LEN: usize = 40;
