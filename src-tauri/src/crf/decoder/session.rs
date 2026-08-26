//! DecodeSession —— 序列解码生命周期
//!
//! 规划文档 §5.5。负责：
//! - 读取并验证 header/index/footer（委托 container 层）；
//! - 建立帧范围和版本能力；
//! - 逐帧调用 FrameDispatcher → Reconstruction；
//! - 管理 reconstructed golden/previous reference；
//! - 输出 `DecodeResult` 或元数据。
//!
//! 它不包含 Golomb、CABAC、DCT 或 RGB 变换数学实现。
//!
//! **迁移状态（P2）**：当前为 `decoder/mod.rs::decode_from_bytes` 编排逻辑的
//! 搬入 + 转发。P4 将把时间参考恢复（golden/previous 还原）从调用方/测试
//! 收敛到本会话，并支持随机访问。

use crate::crf::core::bitstream::constants::{FRAME_HEADER_SIZE, HEADER_SIZE};
use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::{DecodeResult, FrameHeader, FrameIndexEntry, ImageData};
use crate::crf::error::{CrfError, CrfResult};

use super::frame::dispatcher::dispatch_frame;
use super::frame::packet::FramePacket;

/// 解码会话
///
/// 一次性持有解码请求的容器上下文。当前实现为无状态编排；
/// P4 将持有 `ReferenceState` 以支持时间参考恢复。
pub struct DecodeSession;

impl DecodeSession {
    /// 从完整码流字节解码（`decode_from_bytes` 的编排实现）
    pub fn decode_bytes(data: &[u8]) -> CrfResult<DecodeResult> {
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
            // 构建统一帧包（容器 → 分派层契约，规划文档 §7.2）
            let packet = FramePacket::new(fh, &frame_data[FRAME_HEADER_SIZE..], current_offset);
            let frame = Self::reconstruct_packet(&packet, &header)?;
            frames.push(frame);
            golden_refs.push(packet.header.is_golden_ref());

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
                frame.pixels = crate::crf::core::color::rct::rct_inverse(&frame.pixels, components)?;
            }
        }

        Ok(DecodeResult {
            header,
            frame_index,
            frames,
            frame_golden_refs: golden_refs,
        })
    }

    /// 时间维参考还原（规划文档 §8.2：DecodeSession temporal restore）
    ///
    /// `decode_bytes` 返回的 `frames` 是逐帧重建的**残差/基准值**，
    /// 完整帧需要按 `frame_golden_refs` 参考模式叠加还原：
    /// - 首帧（i==0）：即自身；
    /// - golden 参考帧（is_golden）：固定基准 = 解码出的 frame0（G_hat）+ 残差；
    /// - 链式帧（非 golden）：前一还原帧 + 残差。
    ///
    /// 这是**生产恢复逻辑的唯一实现**。测试与调用方不得各自复制
    /// 一份恢复公式（避免语义漂移）。
    pub fn restore_temporal(result: &DecodeResult) -> Vec<ImageData> {
        let mut out: Vec<ImageData> = Vec::with_capacity(result.frames.len());
        // 全 golden 架构的固定差分基准 = 文件自身解码出的 frame0
        let golden_base = &result.frames[0];
        let mut prev: Option<&ImageData> = None;
        for (i, frame) in result.frames.iter().enumerate() {
            let is_golden = result.frame_golden_refs.get(i).copied().unwrap_or(false);
            let pixels = if i == 0 {
                frame.pixels.clone()
            } else if is_golden {
                golden_base
                    .pixels
                    .iter()
                    .zip(&frame.pixels)
                    .map(|(a, b)| a + b)
                    .collect()
            } else {
                match prev {
                    Some(p) => p
                        .pixels
                        .iter()
                        .zip(&frame.pixels)
                        .map(|(a, b)| a + b)
                        .collect(),
                    None => frame.pixels.clone(),
                }
            };
            out.push(ImageData {
                width: frame.width,
                height: frame.height,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels,
            });
            prev = out.last();
        }
        out
    }

    /// 通过统一帧包走分派层 → 重建层
    fn reconstruct_packet(
        packet: &FramePacket<'_>,
        header: &CrfHeader,
    ) -> CrfResult<ImageData> {
        // 组装帧头 + payload 完整字节（分派层契约）
        let mut buf = Vec::with_capacity(FRAME_HEADER_SIZE + packet.payload.len());
        packet.header.write_bytes(&mut buf)?;
        buf.extend_from_slice(packet.payload);
        dispatch_frame(&buf, header)
    }
}
