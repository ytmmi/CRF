use std::fmt;
use std::io;

/// CRF 格式错误类型
#[derive(Debug)]
pub enum CrfError {
    /// 文件魔数无效，不是 CRF 文件
    InvalidMagic,
    /// 版本不支持
    UnsupportedVersion(u16, u16),
    /// 帧数超出有效范围（2-50）
    FrameCountOutOfRange(u16),
    /// 图像尺寸不匹配
    ImageDimensionsMismatch {
        expected: (u16, u16),
        actual: (u16, u16),
    },
    /// 位深不支持
    UnsupportedBitDepth(u8),
    /// 色彩格式不支持
    UnsupportedColorFormat(u8),
    /// 压缩类型不支持
    UnsupportedCompressionType(u8),
    /// CRC 校验失败
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    CrcChecksumFailed { expected: u32, actual: u32 },
    /// 数据不足，无法解析
    InsufficientData { expected: usize, actual: usize },
    /// 编码参数无效
    InvalidCodingParams(String),
    /// 变换块大小无效
    InvalidBlockSize(u16),
    /// IO 错误
    Io(io::Error),
}

impl fmt::Display for CrfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrfError::InvalidMagic => write!(f, "Invalid magic number, not a CRF file"),
            CrfError::UnsupportedVersion(major, minor) => {
                write!(f, "Unsupported version: {}.{}", major, minor)
            }
            CrfError::FrameCountOutOfRange(count) => {
                write!(f, "Frame count {} out of range (2-50)", count)
            }
            CrfError::ImageDimensionsMismatch { expected, actual } => {
                write!(
                    f,
                    "Image dimensions mismatch: expected {}x{}, got {}x{}",
                    expected.0, expected.1, actual.0, actual.1
                )
            }
            CrfError::UnsupportedBitDepth(depth) => {
                write!(f, "Unsupported bit depth: {}", depth)
            }
            CrfError::UnsupportedColorFormat(format) => {
                write!(f, "Unsupported color format: {}", format)
            }
            CrfError::UnsupportedCompressionType(compression) => {
                write!(f, "Unsupported compression type: {}", compression)
            }
            CrfError::CrcChecksumFailed { expected, actual } => {
                write!(
                    f,
                    "CRC checksum failed: expected 0x{:08X}, got 0x{:08X}",
                    expected, actual
                )
            }
            CrfError::InsufficientData { expected, actual } => {
                write!(
                    f,
                    "Insufficient data: expected {} bytes, got {} bytes",
                    expected, actual
                )
            }
            CrfError::InvalidCodingParams(msg) => {
                write!(f, "Invalid coding parameters: {}", msg)
            }
            CrfError::InvalidBlockSize(size) => {
                write!(f, "Invalid block size: {}", size)
            }
            CrfError::Io(err) => write!(f, "IO error: {}", err),
        }
    }
}

impl std::error::Error for CrfError {}

impl From<io::Error> for CrfError {
    fn from(err: io::Error) -> Self {
        CrfError::Io(err)
    }
}

/// 便捷的结果类型
pub type CrfResult<T> = Result<T, CrfError>;
