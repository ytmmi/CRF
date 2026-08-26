//! CRF 解码器
//!
//! 模块布局（P2 架构迁移）：
//! - [`container`]：容器层（bytes/file reader、CRC、header/index/footer）
//! - [`frame`]：帧分派层（frame_type 路由、FramePacket）
//! - 本文件：帧解码分流（frame_type 1-8）与序列/文件级解码入口
//! - `image_export`：解码帧落盘（PNG/BMP 轻量导出）
//! - `banded` / `planar` / `palette`：条带(2)/三平面(3)/调色板(4) 路径
//! - 熵解码器族：golomb / rle_golomb / exp_golomb / rle_cabac / transform
//!
//! **迁移状态**：`container` 和 `frame` 为 P2 新增分层骨架。
//! `decode_from_bytes` 和 `decode_from_file` 保持 facade 兼容，
//! 内部逐步委托到新模块。`decode_frame` 仍为帧分派+重建混合函数，
//! P4 拆分为纯分派+独立重建层。

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
pub mod rle_cabac;
pub mod rle_golomb;
pub mod transform;

#[cfg(test)]
mod tests;

use std::io::{Read, Seek, SeekFrom};

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::format::{
    undo_prediction, CompressionType, CrfHeader, DecodeResult, FrameHeader, FrameIndexEntry,
    ImageData, PredictionMode, FOOTER_SIZE, FRAME_HEADER_SIZE, HEADER_SIZE,
};

pub(crate) use banded::decode_banded_with_undo;
use palette::decode_palette_payload;
pub(crate) use planar::decode_planar;
// 保持与拆分前一致的对外 API（图像导出入口）
#[allow(unused_imports)]
pub use image_export::save_frame_as_image;

/// 解码单帧
///
/// 按 frame_header.frame_type 分流至对应熵解码路径；出口统一执行
/// 逆空间预测（frame_type=6 DCT 路径除外——该路径无空间预测语义）。
pub fn decode_frame(data: &[u8], header: &CrfHeader) -> CrfResult<ImageData> {
    // 数据长度 = 像素数 × 分量数（RGB=3, Gray=1 等）
    let data_len =
        header.width as usize * header.height as usize * header.color_format.component_count();

    // 解析帧头
    let frame_header = FrameHeader::from_bytes(data)?;
    let frame_data = &data[FRAME_HEADER_SIZE..];

    let width = header.width as usize;
    let height = header.height as usize;
    let components = header.color_format.component_count();

    // 条带级自适应路径（frame_type=2）：每条带独立预测模式 + 独立 k，
    // 解码与撤销预测必须逐条带一体完成，不能套用整帧单一模式 undo。
    // 条带高度自 v1.9 起由帧头 coding_params 自描述（{32,64}；旧文件恒 32），
    // 非法值按损坏防护拒绝。
    if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 2 {
        const BAND_HEIGHT_DEFAULT: usize = 32;
        const BAND_HEIGHT_ALT: usize = 64;
        // mask golden 标志位（bit7）—— banded 条带高度占低 7 位（32/64），
        // bit7 由 is_golden_ref() 独立读取，与 golomb_k() 的 & 0x7F 同语义。
        // 既有缺陷：banded 帧此前从未在 PNG1000 胜出（type3×13 主导），
        // golden 标志位与条带高度共存于 coding_params 字节未暴露；
        // frame_type=8 接入改变竞争格局后 banded 胜出触发此 bug。
        let band_height = (frame_header.coding_params & 0x7F) as usize;
        if band_height != BAND_HEIGHT_DEFAULT && band_height != BAND_HEIGHT_ALT {
            return Err(CrfError::InvalidCodingParams(format!(
                "非法条带高度 {}（合法值 32/64）",
                band_height
            )));
        }
        let pixels = decode_banded_with_undo(frame_data, width, height, components, band_height)?;
        return Ok(ImageData {
            width: header.width,
            height: header.height,
            bit_depth: header.bit_depth,
            color_format: header.color_format,
            pixels,
        });
    }

    // 三平面打包路径（frame_type=3）：载荷含 3 个完整单分量子帧，
    // 逐个解码后交错回 [Y,Co,Cg] 布局（外层 RCT 逆变换照常进行）
    if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 3 {
        let pixels = decode_planar(frame_data, header)?;
        return Ok(ImageData {
            width: header.width,
            height: header.height,
            bit_depth: header.bit_depth,
            color_format: header.color_format,
            pixels,
        });
    }

    // 调色板路径（frame_type=4）：palette 表 + RLE 索引流
    if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 4 {
        // 调色板路径（frame_type=4）：palette 表 + 索引流。
        // v2（coding_params.bit0=1）为 copy-above token 化载荷
        // （AV1 palette 思路适配），v1 为旧格式兼容。
        let pixels =
            decode_palette_payload(frame_data, data_len, frame_header.coding_params, width)?;
        return Ok(ImageData {
            width: header.width,
            height: header.height,
            bit_depth: header.bit_depth,
            color_format: header.color_format,
            pixels,
        });
    }

    // 帧内块复制路径（frame_type=7，v1.11）：块级 COPY/PRED 决策 +
    // 三段式载荷。PRED 块的空间逆预测已在模块内部完成（邻居取自含
    // COPY 块的重建缓冲），输出即像素域——外层须跳过 undo_prediction
    // （语义同 frame_type=6）。
    if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 7 {
        let pixels = intrabc::decode_intrabc_payload(frame_data, width, height, components)?;
        return Ok(ImageData {
            width: header.width,
            height: header.height,
            bit_depth: header.bit_depth,
            color_format: header.color_format,
            pixels,
        });
    }

    // 预测后变换 + CABAC 系数编码（frame_type=8，v1.14）
    if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 8 {
        let q_step = header.lossy_quant.max(1);
        let pixels = crate::crf::decoder::intra_transform::decode_intra_transform(
            frame_data, width, height, components, q_step, q_step,
        )?;
        return Ok(ImageData {
            width: header.width,
            height: header.height,
            bit_depth: header.bit_depth,
            color_format: header.color_format,
            pixels,
        });
    }

    // 根据压缩类型解码
    let predicted_pixels = match header.compression_type {
        CompressionType::GolombRice => {
            if frame_header.frame_type == 1 {
                // RLE+Golomb 混合编码（残差帧或首帧竞争胜出者）
                rle_golomb::decode_frame_rle_golomb(frame_data, frame_header.golomb_k(), data_len)
            } else if frame_header.frame_type == 5 {
                // RLE+CABAC 算术编码（v2 梯度分级上下文）：载荷 [k u8][码流]。
                // stride = width×components，与编码端因果梯度推导一致。
                if frame_data.is_empty() {
                    return Err(CrfError::InsufficientData {
                        expected: 1,
                        actual: 0,
                    });
                }
                let stride = width * components;
                rle_cabac::decode_frame_rle_cabac(
                    &frame_data[1..],
                    frame_data[0],
                    data_len,
                    Some(stride),
                )
            } else if frame_header.frame_type == 6 {
                // DCT 量化路径：[k|标志 u8][cabac 码流] → 解码量化系数 →
                // 逆 DCT 还原空间域。分量感知对称还原（与
                // dct_quantize_interleaved_bs 配对）；本路径无空间预测，
                // 调用方须跳过 undo_prediction。
                // 码流从 offset 1 开始（[0] 是 k）——历史缺陷写成 [2..] 丢弃
                // 首字节使整个码流错位，解出与量化档位无关的垃圾系数。
                // DCT 系数流为非空间域数据，不启用梯度分级（stride=None，
                // 与编码端对称）。
                // 载荷首字节标志位（v1.9/v1.10/v1.12）：
                //   bit4 = Trellis 量化、bit5 = 宽度 8、bit6 = 感知量化矩阵、
                //   bit7 = 高度 8（v1.12 矩形形状；旧文件恒为 0，向后兼容）。
                // 四者仅作用于编码端步长/决策分配，残差值即反量化结果，解码端
                // 只需 mask 标志位取 k、并按 bit5/bit7 选择对称的逆变换几何。
                const QM_FLAG_BIT: u8 = 0x40;
                const BW8_FLAG_BIT: u8 = 0x20;
                const BH8_FLAG_BIT: u8 = 0x80;
                const TRELLIS_FLAG_BIT: u8 = 0x10;
                if frame_data.len() < 2 {
                    return Err(CrfError::InsufficientData {
                        expected: 2,
                        actual: 0,
                    });
                }
                let block_w = if frame_data[0] & BW8_FLAG_BIT != 0 {
                    8usize
                } else {
                    4usize
                };
                let block_h = if frame_data[0] & BH8_FLAG_BIT != 0 {
                    8usize
                } else {
                    4usize
                };
                let q_coeffs = rle_cabac::decode_frame_rle_cabac(
                    &frame_data[1..],
                    frame_data[0] & !(QM_FLAG_BIT | BW8_FLAG_BIT | BH8_FLAG_BIT | TRELLIS_FLAG_BIT),
                    data_len,
                    None,
                );
                crate::crf::core::transform::reconstruct::dct_dequantize_inverse_interleaved_bs(
                    &q_coeffs, width, height, components, block_w, block_h,
                )
            } else {
                // 标准 Golomb 或块级自适应k
                golomb::decode_frame_golomb(
                    frame_data,
                    frame_header.coding_params,
                    data_len,
                    width,
                    height,
                    components,
                )
            }
        }
        CompressionType::ExpGolomb => exp_golomb::decode_frame_exp_golomb(frame_data, data_len),
        CompressionType::Transform => transform::decode_frame_transform(frame_data, width, height),
    };

    // 确定本帧预测模式：帧头逐帧指定优先（自适应编码写入），
    // 0xFF=未指定时回退文件头全局设置（兼容旧格式）
    let pred_mode = if frame_header.pred_mode != PredictionMode::PRED_MODE_UNSET {
        PredictionMode::from_u8(frame_header.pred_mode)
    } else {
        header.prediction_mode
    };

    // 撤销帧内预测。
    // frame_type=6（DCT 变换域路径）编码端不做空间预测，逆变换输出
    // 已是空间域样本，必须跳过 undo——否则会以文件头全局模式对从未
    // 预测过的数据强行"撤销预测"，造成二次破坏（历史缺陷）。
    let pixels =
        if header.compression_type == CompressionType::GolombRice && frame_header.frame_type == 6 {
            predicted_pixels
        } else {
            undo_prediction(
                &predicted_pixels,
                header.width as usize,
                header.height as usize,
                header.color_format.component_count(),
                pred_mode,
            )
        };

    Ok(ImageData {
        width: header.width,
        height: header.height,
        bit_depth: header.bit_depth,
        color_format: header.color_format,
        pixels,
    })
}

/// 从字节数据解码 CRF 文件
///
/// 解析文件头 → 帧索引 → 逐帧解码 → 出口统一执行 RCT 逆向色彩变换。
/// frame_golden_refs 记录各帧是否为 golden 差分帧（供调用方做时间维还原：
/// golden 帧 = 首帧 + 差分；链式帧 = 前一还原帧 + 差分）。
pub fn decode_from_bytes(data: &[u8]) -> CrfResult<DecodeResult> {
    if data.len() < HEADER_SIZE {
        return Err(CrfError::InsufficientData {
            expected: HEADER_SIZE,
            actual: data.len(),
        });
    }

    // 解析文件头
    let header = CrfHeader::from_bytes(data)?;
    header.validate()?;

    // 解析帧索引
    let mut frame_index = Vec::new();
    let mut golden_refs: Vec<bool> = Vec::new();
    let mut offset = HEADER_SIZE;

    if header.flags.has_index() {
        for _ in 0..header.frame_count {
            if offset + 8 > data.len() {
                return Err(CrfError::InsufficientData {
                    expected: offset + 8,
                    actual: data.len(),
                });
            }
            let entry = FrameIndexEntry::from_bytes(&data[offset..offset + 8])?;
            frame_index.push(entry);
            offset += 8;
        }
    }

    // 解码帧数据
    let mut frames = Vec::with_capacity(header.frame_count as usize);
    let frames_start = if header.flags.has_index() {
        HEADER_SIZE + header.frame_count as usize * 8
    } else {
        HEADER_SIZE
    };

    let mut current_offset = frames_start;
    for _ in 0..header.frame_count as usize {
        if current_offset >= data.len() {
            return Err(CrfError::InsufficientData {
                expected: current_offset + 1,
                actual: data.len(),
            });
        }

        // 读取帧头以获取帧大小
        if current_offset + FRAME_HEADER_SIZE > data.len() {
            return Err(CrfError::InsufficientData {
                expected: current_offset + FRAME_HEADER_SIZE,
                actual: data.len(),
            });
        }

        let frame_size = u32::from_le_bytes([
            data[current_offset],
            data[current_offset + 1],
            data[current_offset + 2],
            data[current_offset + 3],
        ]) as usize;

        let frame_end = current_offset + FRAME_HEADER_SIZE + frame_size;
        if frame_end > data.len() {
            return Err(CrfError::InsufficientData {
                expected: frame_end,
                actual: data.len(),
            });
        }

        let frame_data = &data[current_offset..frame_end];
        let fh = FrameHeader::from_bytes(frame_data)?;
        let frame = decode_frame(frame_data, &header)?;
        frames.push(frame);
        golden_refs.push(fh.is_golden_ref());

        current_offset = frame_end;
    }

    // 逆向色彩变换：若编码端启用了 YCoCg-R，出口处还原 RGB 域。
    // v1.13 RCT 首帧自适应：first_frame_no_rct=1 时 frame0 以 RGB 直通
    // 形式存储（编码端首帧双路竞争的胜出者），须跳过其逆变换——
    // 差分帧不受影响（其差分基准是首帧像素值而非存储格式）。
    if header.flags.has_rct() {
        let skip_first = header.flags.first_frame_no_rct();
        let components = header.color_format.component_count();
        for (i, frame) in frames.iter_mut().enumerate() {
            if skip_first && i == 0 {
                continue;
            }
            frame.pixels = crate::crf::format::rct_inverse(&frame.pixels, components)?;
        }
    }

    Ok(DecodeResult {
        header,
        frame_index,
        frames,
        frame_golden_refs: golden_refs,
    })
}

/// 从文件解码 CRF 文件（流式读取 + CRC32 文件尾校验）
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

    // 读取文件头
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)?;
    let header = CrfHeader::from_bytes(&header_buf)?;
    header.validate()?;

    // 验证 CRC32（如果存在文件尾）
    if file_size >= (HEADER_SIZE + FOOTER_SIZE) as u64 {
        // 使用 container::footer 的验证函数（P2 容器层拆分）
        if let Err(e) = container::footer::verify_file_crc(reader) {
            return Err(e);
        }
    }

    // 重新定位到文件头之后
    reader.seek(SeekFrom::Start(HEADER_SIZE as u64))?;

    // 读取帧索引
    let mut frame_index = Vec::new();
    if header.flags.has_index() {
        for _ in 0..header.frame_count {
            let mut entry_buf = [0u8; 8];
            reader.read_exact(&mut entry_buf)?;
            let entry = FrameIndexEntry::from_bytes(&entry_buf)?;
            frame_index.push(entry);
        }
    }

    // 读取并解码帧数据
    let mut frames = Vec::with_capacity(header.frame_count as usize);
    let mut golden_refs: Vec<bool> = Vec::new();
    for _ in 0..header.frame_count {
        // 读取帧头
        let mut frame_header_buf = [0u8; FRAME_HEADER_SIZE];
        reader.read_exact(&mut frame_header_buf)?;
        let frame_header = FrameHeader::from_bytes(&frame_header_buf)?;

        // 读取帧数据
        let mut frame_data = vec![0u8; frame_header.frame_size as usize];
        reader.read_exact(&mut frame_data)?;

        // 解码帧
        let frame_buf = [frame_header_buf.to_vec(), frame_data].concat();
        let frame = decode_frame(&frame_buf, &header)?;
        frames.push(frame);
        golden_refs.push(frame_header.is_golden_ref());
    }

    // 逆向色彩变换：与 decode_from_bytes 保持一致
    //（v1.13：first_frame_no_rct=1 时跳过 frame0，见上方说明）
    if header.flags.has_rct() {
        let skip_first = header.flags.first_frame_no_rct();
        let components = header.color_format.component_count();
        for (i, frame) in frames.iter_mut().enumerate() {
            if skip_first && i == 0 {
                continue;
            }
            frame.pixels = crate::crf::format::rct_inverse(&frame.pixels, components)?;
        }
    }

    Ok(DecodeResult {
        header,
        frame_index,
        frames,
        frame_golden_refs: golden_refs,
    })
}
