//! Footer/CRC 验证
//!
//! 从 `decoder/mod.rs` 提取的 CRC 文件尾验证逻辑（P2）。
//! 编码端写入 `[CRC32 u32][FOOTER_MAGIC u32]` 共 8 字节，
//! 解码端据此验证文件完整性。
//!
//! 现有 `crate::crf::checksum::verify_crc32` 提供基本校验，
//! 本模块提供带详细错误信息的验证函数。

use std::io::{Read, Seek, SeekFrom};

use crate::crf::checksum::{crc32, verify_crc32};
use crate::crf::error::{CrfError, CrfResult};
use crate::crf::format::FOOTER_SIZE;

/// 验证文件 CRC32 校验和，失败时返回详细的错误信息
///
/// 从 `decode_from_file` 提取（P2 容器层拆分）。返回 `Ok(())` 表示校验通过；
/// 失败时返回 `CrcChecksumFailed` 错误，包含期望值与实际值。
///
/// 若文件过小（不含文件尾），返回 `InsufficientData` 而非静默跳过。
pub fn verify_file_crc(reader: &mut (impl Read + Seek)) -> CrfResult<()> {
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    if file_size < FOOTER_SIZE as u64 {
        return Err(CrfError::InsufficientData {
            expected: FOOTER_SIZE,
            actual: file_size as usize,
        });
    }

    // 重新定位到文件头
    reader.seek(SeekFrom::Start(0))?;

    // 使用 checksum 模块的 verify_crc32
    let crc_valid = verify_crc32(reader)?;
    if !crc_valid {
        // 读取期望的 CRC 值
        reader.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let mut crc_buf = [0u8; 4];
        reader.read_exact(&mut crc_buf)?;
        let expected_crc = u32::from_le_bytes(crc_buf);

        // 重新计算实际 CRC
        reader.seek(SeekFrom::Start(0))?;
        let mut file_data = vec![0u8; (file_size - FOOTER_SIZE as u64) as usize];
        reader.read_exact(&mut file_data)?;
        let actual_crc = crc32(&file_data);

        return Err(CrfError::CrcChecksumFailed {
            expected: expected_crc,
            actual: actual_crc,
        });
    }

    Ok(())
}