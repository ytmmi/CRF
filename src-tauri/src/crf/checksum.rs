use std::io::{Read, Seek, SeekFrom};

use super::error::{CrfError, CrfResult};
use crate::crf::core::bitstream::constants::FOOTER_SIZE;

/// CRC32 查找表
#[allow(dead_code)] // 标量查表参考实现：与 crc32fast 硬件路径逐位对拍用
const CRC32_TABLE: [u32; 256] = generate_crc32_table();

/// 生成 CRC32 查找表（编译时计算）
#[allow(dead_code)] // 标量查表参考实现：与 crc32fast 硬件路径逐位对拍用
const fn generate_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// 计算数据的 CRC32 校验和
///
/// 使用 crc32fast（SSE4.2 PCLMULQDQ 硬件加速路径自动选择），
/// 多项式/初值与查表实现完全一致，产物位兼容。
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// 计算文件内容的 CRC32（不包括文件尾）
#[allow(dead_code)] // 文件级校验 API：与流内 CRC 校验对称保留
pub fn crc32_file(reader: &mut (impl Read + Seek)) -> CrfResult<u32> {
    // 获取文件大小
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    // 计算除文件尾外的所有内容
    let data_size = file_size - FOOTER_SIZE as u64;
    let mut buffer = vec![0u8; data_size as usize];
    reader.read_exact(&mut buffer)?;

    Ok(crc32(&buffer))
}

/// 验证文件 CRC32 校验和
#[allow(dead_code)] // 文件级校验 API：与流内 CRC 校验对称保留
pub fn verify_crc32(reader: &mut (impl Read + Seek)) -> CrfResult<bool> {
    // 获取文件大小
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    if file_size < FOOTER_SIZE as u64 + 4 {
        return Err(CrfError::InsufficientData {
            expected: FOOTER_SIZE + 4,
            actual: file_size as usize,
        });
    }

    // 读取文件内容（不含文件尾）
    let data_size = file_size - FOOTER_SIZE as u64;
    let mut buffer = vec![0u8; data_size as usize];
    reader.read_exact(&mut buffer)?;

    // 计算 CRC32
    let computed_crc = crc32(&buffer);

    // 读取文件中存储的 CRC32
    let mut crc_bytes = [0u8; 4];
    reader.read_exact(&mut crc_bytes)?;
    let stored_crc = u32::from_le_bytes(crc_bytes);

    Ok(computed_crc == stored_crc)
}

/// 读取文件尾中的 CRC32
#[allow(dead_code)] // footer 解析 API：与 encoder 写入端对称保留
pub fn read_footer_crc(reader: &mut (impl Read + Seek)) -> CrfResult<u32> {
    let file_size = reader.seek(SeekFrom::End(0))?;

    if file_size < FOOTER_SIZE as u64 {
        return Err(CrfError::InsufficientData {
            expected: FOOTER_SIZE,
            actual: file_size as usize,
        });
    }

    // 跳转到 CRC32 位置（文件尾前 8 字节）
    reader.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;

    let mut crc_bytes = [0u8; 4];
    reader.read_exact(&mut crc_bytes)?;

    Ok(u32::from_le_bytes(crc_bytes))
}

/// 读取文件尾中的魔数
#[allow(dead_code)] // footer 解析 API：与 encoder 写入端对称保留
pub fn read_footer_magic(reader: &mut (impl Read + Seek)) -> CrfResult<[u8; 4]> {
    let file_size = reader.seek(SeekFrom::End(0))?;

    if file_size < 4 {
        return Err(CrfError::InsufficientData {
            expected: 4,
            actual: file_size as usize,
        });
    }

    // 跳转到文件尾魔数位置
    reader.seek(SeekFrom::End(-4))?;

    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;

    Ok(magic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_empty() {
        assert_eq!(crc32(&[]), 0x00000000);
    }

    #[test]
    fn test_crc32_known_values() {
        // 测试已知的 CRC32 值
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b"Hello, World!"), 0xEC4AC3D0);
    }

    #[test]
    fn test_zigzag_encode_decode() {
        use crate::crf::core::entropy::scan::{zigzag_decode, zigzag_encode};

        let test_cases = [0, 1, -1, 2, -2, 100, -100, 1000, -1000];
        for &val in &test_cases {
            let encoded = zigzag_encode(val);
            let decoded = zigzag_decode(encoded);
            assert_eq!(val, decoded, "Failed for value {}", val);
        }
    }
}
