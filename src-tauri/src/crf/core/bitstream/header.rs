//! 文件头结构（原 format/header.rs，P4 架构迁移）
//!
//! 规划文档 §3.3 / §6.3。CRF 文件头（magic/version/frame_count/尺寸/位深/
//! 色彩格式/压缩类型/标志/块大小/预测模式/量化步长/用户数据）的读写与校验。
//! 容器层契约：只负责字节和边界，不负责预测、DCT、量化或参考恢复。

use std::io::Write;

use crate::crf::core::bitstream::constants::*;
use crate::crf::core::domain::{ColorFormat, CompressionType, Flags, PredictionMode};
use crate::crf::error::{CrfError, CrfResult};

/// 文件头结构
#[derive(Debug, Clone)]
pub struct CrfHeader {
    /// 主版本号
    pub version_major: u16,
    /// 次版本号
    pub version_minor: u16,
    /// 帧总数
    pub frame_count: u16,
    /// 图像宽度
    pub width: u16,
    /// 图像高度
    pub height: u16,
    /// 像素位深
    pub bit_depth: u8,
    /// 色彩格式
    pub color_format: ColorFormat,
    /// 压缩类型
    pub compression_type: CompressionType,
    /// 标志位
    pub flags: Flags,
    /// 变换块大小
    pub block_size: u16,
    /// 帧内预测模式
    pub prediction_mode: PredictionMode,
    /// 有损量化步长（0=无损；1..=255 为残差死区量化步长，见 flags.bit2）
    pub lossy_quant: u8,
    /// 用户自定义数据
    pub user_data: String,
}

impl CrfHeader {
    /// 创建新的文件头
    pub fn new(
        frame_count: u16,
        width: u16,
        height: u16,
        bit_depth: u8,
        color_format: ColorFormat,
        compression_type: CompressionType,
    ) -> Self {
        CrfHeader {
            version_major: VERSION_MAJOR,
            version_minor: VERSION_MINOR,
            frame_count,
            width,
            height,
            bit_depth,
            color_format,
            compression_type,
            flags: Flags::new(),
            block_size: 8,
            prediction_mode: PredictionMode::None,
            lossy_quant: 0,
            user_data: String::new(),
        }
    }

    /// 从字节缓冲区解析文件头
    pub fn from_bytes(data: &[u8]) -> CrfResult<Self> {
        if data.len() < HEADER_SIZE {
            return Err(CrfError::InsufficientData {
                expected: HEADER_SIZE,
                actual: data.len(),
            });
        }

        // 验证魔数
        if data[0..4] != FILE_MAGIC {
            return Err(CrfError::InvalidMagic);
        }

        let version_major = u16::from_le_bytes([data[4], data[5]]);
        let version_minor = u16::from_le_bytes([data[6], data[7]]);

        // 验证版本
        if version_major != VERSION_MAJOR {
            return Err(CrfError::UnsupportedVersion(version_major, version_minor));
        }

        let frame_count = u16::from_le_bytes([data[8], data[9]]);
        if !(MIN_FRAMES..=MAX_FRAMES).contains(&frame_count) {
            return Err(CrfError::FrameCountOutOfRange(frame_count));
        }

        let width = u16::from_le_bytes([data[10], data[11]]);
        let height = u16::from_le_bytes([data[12], data[13]]);
        let bit_depth = data[14];
        let color_format = ColorFormat::from_u8(data[15])?;
        let compression_type = CompressionType::from_u8(data[16])?;
        let flags = Flags::from_u8(data[17]);
        let block_size = u16::from_le_bytes([data[18], data[19]]);

        // 帧内预测模式（使用reserved字段的第一个字节）
        let prediction_mode = PredictionMode::from_u8(data[20]);

        // 有损量化步长（0=无损；flags.bit2 应与之一致）
        let lossy_quant = data[21];

        // 解析用户数据（最多 40 字节，以 NULL 结尾）
        let user_data_bytes = &data[24..64];
        let end = user_data_bytes.iter().position(|&b| b == 0).unwrap_or(40);
        let user_data = String::from_utf8_lossy(&user_data_bytes[..end]).to_string();

        Ok(CrfHeader {
            version_major,
            version_minor,
            frame_count,
            width,
            height,
            bit_depth,
            color_format,
            compression_type,
            flags,
            block_size,
            prediction_mode,
            lossy_quant,
            user_data,
        })
    }

    /// 将文件头写入字节缓冲区
    pub fn write_bytes(&self, writer: &mut impl Write) -> CrfResult<()> {
        let mut buffer = [0u8; HEADER_SIZE];

        // 魔数
        buffer[0..4].copy_from_slice(&FILE_MAGIC);

        // 版本号
        buffer[4..6].copy_from_slice(&self.version_major.to_le_bytes());
        buffer[6..8].copy_from_slice(&self.version_minor.to_le_bytes());

        // 帧数
        buffer[8..10].copy_from_slice(&self.frame_count.to_le_bytes());

        // 尺寸
        buffer[10..12].copy_from_slice(&self.width.to_le_bytes());
        buffer[12..14].copy_from_slice(&self.height.to_le_bytes());

        // 位深
        buffer[14] = self.bit_depth;

        // 色彩格式
        buffer[15] = self.color_format as u8;

        // 压缩类型
        buffer[16] = self.compression_type as u8;

        // 标志位
        buffer[17] = self.flags.as_u8();

        // 变换块大小
        buffer[18..20].copy_from_slice(&self.block_size.to_le_bytes());

        // 帧内预测模式（使用reserved字段的第一个字节）
        buffer[20] = self.prediction_mode as u8;

        // 有损量化步长（0=无损）
        buffer[21] = self.lossy_quant;

        // 用户数据
        let user_data_bytes = self.user_data.as_bytes();
        let len = user_data_bytes.len().min(USER_DATA_MAX_LEN);
        buffer[24..24 + len].copy_from_slice(&user_data_bytes[..len]);

        writer.write_all(&buffer)?;
        Ok(())
    }

    /// 验证文件头参数
    pub fn validate(&self) -> CrfResult<()> {
        if !(MIN_FRAMES..=MAX_FRAMES).contains(&self.frame_count) {
            return Err(CrfError::FrameCountOutOfRange(self.frame_count));
        }

        if self.width == 0 || self.height == 0 {
            return Err(CrfError::InvalidCodingParams(
                "Width and height must be greater than 0".to_string(),
            ));
        }

        match self.bit_depth {
            8 | 10 | 12 | 16 => {}
            _ => return Err(CrfError::UnsupportedBitDepth(self.bit_depth)),
        }

        if self.compression_type == CompressionType::Transform
            && (self.block_size == 0 || self.block_size > 64)
        {
            return Err(CrfError::InvalidBlockSize(self.block_size));
        }

        Ok(())
    }
}
