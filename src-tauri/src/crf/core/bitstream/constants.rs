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

/// 帧头大小（字节）：frame_size(u32) + pixel_count(u32) + frame_type(u8) + coding_params(u8) + pred_mode(u8) + reference_type(u8) + lic_a_num(u8) + lic_b(u8)
///
/// v1.15 破坏式更新：新增第 6 字段 `reference_type`（0=golden / 1=previous /
/// 2=prev2 / 3=保留），替代 coding_params.bit7 的 golden 单 bit 语义，
/// 支持多参考帧（previous 之外的前前帧 prev2）。11 → 12 字节。
/// v1.16 破坏式更新：新增第 7/8 字段 `lic_a_num` / `lic_b`（LIC 帧级乘加
/// 加权参考信令，0=未启用）。12 → 14 字节。
pub const FRAME_HEADER_SIZE: usize = 14;

/// 帧头中 `reference_type` 字段的字节偏移（0=golden / 1=previous / 2=prev2）
///
/// 注意：该字段**固定**在此偏移（v1.15 布局遗留），不随 `FRAME_HEADER_SIZE`
/// 变化——LIC 字段追加在末尾。写参考类型必须用本常量而非
/// `FRAME_HEADER_SIZE - 1`（后者在 v1.16 后会指向 lic_b）。
pub const REFERENCE_TYPE_OFFSET: usize = 11;

/// 帧头中 `pred_mode` 字段的字节偏移（固定偏移，v1.15 布局遗留）。
///
/// 旧代码用 `FRAME_HEADER_SIZE - 2` 定位（12B 帧头时恰好 =10），
/// v1.16 帧头 14B 后该算式会错位到 lic 字段——必须使用本常量。
pub const PRED_MODE_OFFSET: usize = 10;

/// 帧头中 `lic_a_num` 字段的字节偏移（0=未启用 LIC；80..=120 为 a=num/100）
pub const LIC_A_NUM_OFFSET: usize = 12;

/// 帧头中 `lic_b` 字段的字节偏移（i8 语义；仅 `lic_a_num` 非零时有效）
pub const LIC_B_OFFSET: usize = 13;

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
/// v1.16（2026-09-08 破坏式更新）：帧头 12→14 字节（新增 lic_a_num/lic_b 字段，
/// LIC 帧级乘加加权参考信令）。旧文件（v1.15 及以下）不再兼容。
pub const VERSION_MINOR: u16 = 5;

/// 用户数据最大长度（字节）
pub const USER_DATA_MAX_LEN: usize = 40;
