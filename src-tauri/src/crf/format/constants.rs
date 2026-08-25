/// 文件魔数: "CRF\0"
pub const FILE_MAGIC: [u8; 4] = [0x43, 0x52, 0x46, 0x00];

/// 文件尾魔数: "CRF\xFF"
pub const FOOTER_MAGIC: [u8; 4] = [0x43, 0x52, 0x46, 0xFF];

/// 文件头大小（字节）
pub const HEADER_SIZE: usize = 64;

/// 帧头大小（字节）：frame_size(u32) + pixel_count(u32) + frame_type(u8) + coding_params(u8) + pred_mode(u8)
pub const FRAME_HEADER_SIZE: usize = 11;

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
pub const VERSION_MINOR: u16 = 3;

/// 用户数据最大长度（字节）
pub const USER_DATA_MAX_LEN: usize = 40;
