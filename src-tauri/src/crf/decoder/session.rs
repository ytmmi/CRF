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
        let mut prev2_refs: Vec<bool> = Vec::new();
        let mut lic_refs: Vec<(u8, u8)> = Vec::new();
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
            prev2_refs.push(packet.header.is_prev2_ref());
            // v1.16：LIC 参数（lic_a_num, lic_b 原字节；首帧恒 (0,0)）
            lic_refs.push((packet.header.lic_a_num, packet.header.lic_b));

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
                frame.pixels =
                    crate::crf::core::color::rct::rct_inverse(&frame.pixels, components)?;
            }
        }

        Ok(DecodeResult {
            header,
            frame_index,
            frames,
            frame_golden_refs: golden_refs,
            frame_prev2_refs: prev2_refs,
            frame_lic: lic_refs,
        })
    }

    /// 时间维参考还原（规划文档 §8.2：DecodeSession temporal restore）
    ///
    /// `decode_bytes` 返回的 `frames` 是逐帧重建的**残差/基准值**，
    /// 完整帧需要按参考类型叠加还原：
    /// - 首帧（i==0）：即自身；
    /// - golden 参考帧（reference_type=0）：固定基准 = 解码出的 frame0（G_hat）+ 残差；
    /// - previous 参考帧（reference_type=1）：前一还原帧 + 残差；
    /// - prev2 参考帧（reference_type=2，v1.15）：前前还原帧 + 残差（周期动作）。
    ///
    /// 这是**生产恢复逻辑的唯一实现**。测试与调用方不得各自复制
    /// 一份恢复公式（避免语义漂移）。
    pub fn restore_temporal(result: &DecodeResult) -> Vec<ImageData> {
        let mut out: Vec<ImageData> = Vec::with_capacity(result.frames.len());
        // 全 golden 架构的固定差分基准 = 文件自身解码出的 frame0
        let golden_base = &result.frames[0];
        for (i, frame) in result.frames.iter().enumerate() {
            let is_golden = result.frame_golden_refs.get(i).copied().unwrap_or(false);
            let is_prev2 = result.frame_prev2_refs.get(i).copied().unwrap_or(false);
            // v1.16：该帧的 LIC 加权参数（lic_a_num, lic_b 原字节）
            let lic = result.frame_lic.get(i).copied().unwrap_or((0, 0));
            let restored = if i == 0 {
                frame.clone()
            } else if is_golden {
                Self::restore_referenced(Some(golden_base), frame, lic)
            } else if is_prev2 {
                // prev2 = 前前还原帧（i-2）；i<2 时退化为 golden
                let base = if i >= 2 {
                    Some(&out[i - 2])
                } else {
                    Some(golden_base)
                };
                Self::restore_referenced(base, frame, lic)
            } else {
                let base = if i >= 1 {
                    Some(&out[i - 1])
                } else {
                    Some(golden_base)
                };
                Self::restore_referenced(base, frame, lic)
            };
            out.push(restored);
        }
        out
    }

    /// 单帧时间维还原（i>0）：参考叠加基准（golden/prev/prev2）+ 残差。
    /// `restore_temporal` 与 `decode_bytes_streaming` 共用，保证恢复公式唯一。
    ///
    /// v1.16 LIC：`lic=(lic_a_num, lic_b)` 非零时，参考基准先经乘加加权
    /// `LIC(base) = (lic_a_num·base)/100 + lic_b` 再叠加残差——与编码端
    /// `diff = frame − LIC(golden)` 严格对称。
    fn restore_referenced(
        base: Option<&ImageData>,
        frame: &ImageData,
        lic: (u8, u8),
    ) -> ImageData {
        let base_img = base.expect("restore base must be set");
        let (lic_a_num, lic_b) = lic;
        let weighted: Vec<i32> = if lic_a_num != 0 {
            crate::crf::core::illumination::apply_lic_weighted(&base_img.pixels, lic_a_num, lic_b)
        } else {
            base_img.pixels.clone()
        };
        let pixels = weighted
            .iter()
            .zip(&frame.pixels)
            .map(|(a, b)| a + b)
            .collect();
        ImageData {
            width: frame.width,
            height: frame.height,
            bit_depth: frame.bit_depth,
            color_format: frame.color_format,
            pixels,
        }
    }

    /// 通过统一帧包走分派层 → 重建层
    fn reconstruct_packet(packet: &FramePacket<'_>, header: &CrfHeader) -> CrfResult<ImageData> {
        // 组装帧头 + payload 完整字节（分派层契约）
        let mut buf = Vec::with_capacity(FRAME_HEADER_SIZE + packet.payload.len());
        packet.header.write_bytes(&mut buf)?;
        buf.extend_from_slice(packet.payload);
        dispatch_frame(&buf, header)
    }

    /// 流式解码 + 时间维还原：逐帧回调还原后的完整帧，不整体持有全部帧。
    ///
    /// 与 [`Self::decode_bytes`] + [`Self::restore_temporal`] 语义完全一致，
    /// 但内存为 O(golden + 单帧)，适用于 >50 帧大序列的逐帧质量度量。
    /// 回调接收 (帧序号, 还原后的完整帧)；返回 `Err` 即中止解码。
    pub fn decode_bytes_streaming(
        data: &[u8],
        mut on_frame: impl FnMut(usize, &ImageData) -> CrfResult<()>,
    ) -> CrfResult<()> {
        if data.len() < HEADER_SIZE {
            return Err(CrfError::InsufficientData {
                expected: HEADER_SIZE,
                actual: data.len(),
            });
        }

        let header = CrfHeader::from_bytes(data)?;
        header.validate()?;

        // 帧索引区（流式解码仅需跳过，逐帧顺序读取）
        let frames_start = if header.flags.has_index() {
            HEADER_SIZE + header.frame_count as usize * 8
        } else {
            HEADER_SIZE
        };

        let has_rct = header.flags.has_rct();
        let skip_first = header.flags.first_frame_no_rct();
        let components = header.color_format.component_count();

        // 时间维还原状态：golden 基准（还原后的 frame0）+ 前一/前前还原帧
        let mut golden_base: Option<ImageData> = None;
        let mut prev: Option<ImageData> = None;
        let mut prev2: Option<ImageData> = None;

        let mut current_offset = frames_start;
        for i in 0..header.frame_count as usize {
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
            let packet = FramePacket::new(fh, &frame_data[FRAME_HEADER_SIZE..], current_offset);
            let mut frame = Self::reconstruct_packet(&packet, &header)?;

            // 逆向色彩变换（与 decode_bytes 出口语义一致：frame0 直通时跳过）
            if has_rct && !(skip_first && i == 0) {
                frame.pixels =
                    crate::crf::core::color::rct::rct_inverse(&frame.pixels, components)?;
            }

            let is_golden = packet.header.is_golden_ref();
            let is_prev2 = packet.header.is_prev2_ref();
            // v1.16：LIC 参数（首帧恒 (0,0)，无加权）
            let lic = (packet.header.lic_a_num, packet.header.lic_b);
            let restored = if i == 0 {
                frame.clone()
            } else if is_golden {
                Self::restore_referenced(golden_base.as_ref(), &frame, lic)
            } else if is_prev2 {
                // prev2 = 前前还原帧（i-2）；i<2 时退化为 golden
                let base = if i >= 2 {
                    prev2.as_ref()
                } else {
                    golden_base.as_ref()
                };
                Self::restore_referenced(base, &frame, lic)
            } else {
                let base = if i >= 1 {
                    prev.as_ref()
                } else {
                    golden_base.as_ref()
                };
                Self::restore_referenced(base, &frame, lic)
            };

            on_frame(i, &restored)?;

            if i == 0 {
                golden_base = Some(restored.clone());
            }
            prev2 = prev.take();
            prev = Some(restored);
            current_offset = frame_end;
        }
        Ok(())
    }
}
