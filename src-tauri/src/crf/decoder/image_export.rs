//! 解码帧的图像导出：PNG / BMP 落盘
//!
//! 自包含的轻量编码器（灰度语义），用于调试与产物落盘场景；
//! 生产级 RGB 导出走 test 模块的 save_frame_lossless（image-rs）。

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::domain::ImageData;

/// 将解码后的帧保存为图像文件
#[allow(dead_code)] // 调试落盘工具：生产导出走 test 模块 save_frame_lossless
pub fn save_frame_as_image(frame: &ImageData, path: &str, format: &str) -> CrfResult<()> {
    match format.to_lowercase().as_str() {
        "png" => {
            // 转换为 PNG 格式
            let png_data = encode_png(frame)?;
            std::fs::write(path, png_data)?;
        }
        "bmp" => {
            let bmp_data = encode_bmp(frame)?;
            std::fs::write(path, bmp_data)?;
        }
        _ => {
            return Err(CrfError::InvalidCodingParams(format!(
                "Unsupported format: {}",
                format
            )));
        }
    }

    Ok(())
}

/// 简单的 PNG 编码器（灰度图像）
#[allow(dead_code)] // 自包含调试编码器：无压缩 deflate，仅供产物落盘排查
fn encode_png(frame: &ImageData) -> CrfResult<Vec<u8>> {
    let width = frame.width as u32;
    let height = frame.height as u32;
    let bit_depth = frame.bit_depth;

    // 创建 PNG 文件头
    let mut png = Vec::new();

    // PNG 签名
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR 块
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(bit_depth); // bit depth
    ihdr.push(0); // color type: grayscale
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_png_chunk(&mut png, b"IHDR", &ihdr);

    // IDAT 块（简单的 uncompressed deflate）
    let mut raw_data = Vec::new();
    let bytes_per_sample = if bit_depth <= 8 { 1 } else { 2 };

    for y in 0..height {
        raw_data.push(0); // filter: none
        for x in 0..width {
            let idx = (y * width + x) as usize;
            if idx < frame.pixels.len() {
                let value = frame.pixels[idx].max(0).min((1 << bit_depth) - 1) as u16;
                if bytes_per_sample == 1 {
                    raw_data.push(value as u8);
                } else {
                    raw_data.push((value >> 8) as u8);
                    raw_data.push(value as u8);
                }
            }
        }
    }

    // 简单的 stored block（无压缩）
    let mut compressed = Vec::new();
    let mut i = 0;
    while i < raw_data.len() {
        let block_len = (raw_data.len() - i).min(65535);
        let is_last = i + block_len >= raw_data.len();
        compressed.push(if is_last { 1 } else { 0 }); // BFINAL
        compressed.extend_from_slice(&(block_len as u16).to_le_bytes());
        compressed.extend_from_slice(&(!block_len as u16).to_le_bytes());
        compressed.extend_from_slice(&raw_data[i..i + block_len]);
        i += block_len;
    }

    write_png_chunk(&mut png, b"IDAT", &compressed);

    // IEND 块
    write_png_chunk(&mut png, b"IEND", &[]);

    Ok(png)
}

/// 写入 PNG 块
#[allow(dead_code)] // 仅被本模块保留的调试编码器调用
fn write_png_chunk(png: &mut Vec<u8>, chunk_type: &[u8], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(chunk_type);
    png.extend_from_slice(data);

    // CRC32
    let mut crc_data = Vec::with_capacity(chunk_type.len() + data.len());
    crc_data.extend_from_slice(chunk_type);
    crc_data.extend_from_slice(data);
    let crc = crate::crf::checksum::crc32(&crc_data);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// 简单的 BMP 编码器（8位灰度）
#[allow(dead_code)] // 自包含调试编码器：仅 save_frame_as_image 调试路径使用
fn encode_bmp(frame: &ImageData) -> CrfResult<Vec<u8>> {
    let width = frame.width as i32;
    let height = frame.height as i32;
    let row_size = ((width * 3 + 3) / 4) * 4; // 对齐到 4 字节
    let pixel_data_size = row_size * height;
    let file_size = 54 + 1024 + pixel_data_size; // header + palette + pixels

    let mut bmp = Vec::with_capacity(file_size as usize);

    // 文件头
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&((54 + 1024_u32).to_le_bytes())); // pixel data offset

    // 信息头
    bmp.extend_from_slice(&40_u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&8u16.to_le_bytes()); // bit depth
    bmp.extend_from_slice(&[0u8; 24]); // compression, image size, etc.

    // 调色板（256 色灰度）
    for i in 0..=255u8 {
        bmp.extend_from_slice(&[i, i, i, 0]);
    }

    // 像素数据（从下到上）
    for y in (0..height).rev() {
        for x in 0..width {
            let idx = (y * width + x) as usize;
            let value = if idx < frame.pixels.len() {
                frame.pixels[idx].clamp(0, 255) as u8
            } else {
                0
            };
            bmp.push(value);
        }
        // 填充对齐
        let padding = (row_size - width * 3) as usize;
        bmp.resize(bmp.len() + padding, 0);
    }

    Ok(bmp)
}
