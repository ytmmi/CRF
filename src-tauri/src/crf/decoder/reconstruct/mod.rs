//! 重建层 —— 单帧解码重建（payload → 重建帧）
//!
//! 规划文档 §5.4。本层承载"帧管线重建"语义：给定帧头+payload 字节与文件头
//! 上下文，还原该帧的空间域像素。这是编码端本地闭环重建（G_hat）与解码端
//! 共享的纯重建契约——编码流程 §8.1 要求"local decode/reconstruct G_hat"，
//! encoder 通过本层获取本地重建，不得直接调用 decoder 的容器/session 层。
//!
//! **迁移状态（P2）**：当前为 `decoder/mod.rs::decode_frame` 的搬入 + 转发。
//! P4 将按 frame type 拆分为 payload handler（解码载荷）+ 逆预测/逆变换/色彩
//! 恢复（重建），本层保留纯重建编排。

use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::{
    CompressionType, FrameHeader, ImageData, PredictionMode,
};
use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::prediction::intra::undo_prediction;

use super::banded::decode_banded_with_undo;
use super::palette::decode_palette_payload;
use super::planar::decode_planar;
use super::{exp_golomb, golomb, intrabc, rle_cabac, rle_golomb, transform};

/// 单帧重建（帧管线重建入口）
///
/// 按 frame_header.frame_type 分流至对应熵解码路径；出口统一执行
/// 逆空间预测（frame_type=6 DCT 路径除外——该路径无空间预测语义）。
///
/// 返回**变换域/预测域出口的像素**（RCT 逆变换由容器/会话层统一执行，
/// 与 `decode_from_bytes` 出口语义一致）。
pub fn reconstruct_frame(data: &[u8], header: &CrfHeader) -> CrfResult<ImageData> {
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
        // 亮度/色度步长由载荷内 flags 信令（encode 端无条件写入），解码端
        // 完全从载荷读取，不依赖文件头 lossy_quant——旧缺陷：传
        // (q_step, q_step) 使 chroma_step != q_step 时色度反量化步长错误。
        let pixels = crate::crf::decoder::intra_transform::decode_intra_transform(
            frame_data, width, height, components,
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
