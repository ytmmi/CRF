//! CRF 解码器
//!
//! 模块布局（P2 架构迁移）：
//! - [`container`]：容器层（bytes/file reader、CRC、header/index/footer）
//! - [`frame`]：帧分派层（frame_type 路由、FramePacket）
//! - [`reconstruct`]：重建层（单帧重建，encoder 本地闭环 G_hat 同入口）
//! - [`session`]：解码会话（序列解码编排、时间参考恢复）
//! - 本文件：仅保留对外公共入口（`decode_from_bytes` / `decode_from_file`）
//! - `image_export`：解码帧落盘（PNG/BMP 轻量导出）
//!
//! **迁移状态（P2）**：容器读取、帧分派、单帧重建、会话编排均已下沉到
//! 对应分层；本文件只保留公共 API 兼容转发。

pub(crate) mod banded;
pub mod coeff_cabac;
/// container：容器层（P2 架构迁移，bounded reader/CRC/footer 验证）
pub mod container;
pub mod exp_golomb;
/// frame：帧分派层（P2 架构迁移，frame_type 路由/FramePacket）
pub mod frame;
pub mod golomb;
pub(crate) mod image_export;
pub mod intra_transform;
pub(crate) mod intrabc;
pub(crate) mod palette;
pub(crate) mod planar;
/// reconstruct：重建层（P2 架构迁移，单帧重建入口）
pub mod reconstruct;
pub mod rle_cabac;
pub mod rle_golomb;
/// session：解码会话（P2 架构迁移，序列解码编排）
pub mod session;
pub mod transform;

#[cfg(test)]
mod tests;

use std::io::{Read, Seek, SeekFrom};

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::bitstream::constants::{FOOTER_SIZE, HEADER_SIZE};
use crate::crf::core::domain::DecodeResult;

// 测试/对称 API 路径依赖的转发（encoder 测试与集成测试使用）
#[allow(unused_imports)]
pub(crate) use banded::decode_banded_with_undo;
#[allow(unused_imports)]
pub(crate) use planar::decode_planar;
// 保持与拆分前一致的对外 API（图像导出入口）
#[allow(unused_imports)]
pub use image_export::save_frame_as_image;

/// 从字节数据解码 CRF 文件
///
/// 解析文件头 → 帧索引 → 逐帧解码 → 出口统一执行 RCT 逆向色彩变换。
/// frame_golden_refs 记录各帧是否为 golden 差分帧（供调用方做时间维还原：
/// golden 帧 = 首帧 + 差分；链式帧 = 前一还原帧 + 差分）。
///
/// **P2 架构迁移**：编排逻辑已下沉到 [`session::DecodeSession::decode_bytes`]，
/// 本函数保留为兼容转发。
pub fn decode_from_bytes(data: &[u8]) -> CrfResult<DecodeResult> {
    session::DecodeSession::decode_bytes(data)
}

/// 从文件解码 CRF 文件（流式读取 + CRC32 文件尾校验）
///
/// **P2 架构迁移**：文件读取 + CRC 验证后，编排交给
/// [`session::DecodeSession::decode_bytes`]，消除重复容器逻辑。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn decode_from_file(reader: &mut (impl Read + Seek)) -> CrfResult<DecodeResult> {
    // 获取文件大小
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    if file_size < HEADER_SIZE as u64 {
        return Err(CrfError::InsufficientData {
            expected: HEADER_SIZE,
            actual: file_size as usize,
        });
    }

    // 验证 CRC32（如果存在文件尾），使用 container::footer（P2 容器层拆分）
    if file_size >= (HEADER_SIZE + FOOTER_SIZE) as u64 {
        if let Err(e) = container::footer::verify_file_crc(reader) {
            return Err(e);
        }
    }

    // 读入全部字节后交给会话层编排（解码语义与 decode_bytes 完全一致）
    reader.seek(SeekFrom::Start(0))?;
    let mut data = Vec::with_capacity(file_size as usize);
    reader.read_to_end(&mut data)?;
    session::DecodeSession::decode_bytes(&data)
}
