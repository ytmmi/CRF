use crate::crf::core::domain::PredictionMode;
use crate::crf::core::prediction::intra::undo_prediction_range;
use crate::crf::error::{CrfError, CrfResult};

use super::rle_golomb;
/// 解码条带级自适应帧载荷（frame_type=2）并逐条带撤销预测
///
/// 载荷布局与编码端 encode_banded_payload 对应：
/// [band_count u16 LE][逐条带: mode u8 + k u8 + data_len u32 LE + data]
///
/// `band_height`：预测条带高度，由帧头 coding_params 提供
/// （v1.9 起可为 64；旧文件恒为 32）。
///
/// 关键正确性约束：条带按光栅顺序处理，undo 时输出缓冲保留前序条带的
/// 已重建行，预测邻居（top/top_left/top_right 行）因此总是已重建数据。
pub(crate) fn decode_banded_with_undo(
    data: &[u8],
    width: usize,
    height: usize,
    components: usize,
    band_height: usize,
) -> CrfResult<Vec<i32>> {
    if data.len() < 2 {
        return Err(CrfError::InsufficientData {
            expected: 2,
            actual: data.len(),
        });
    }
    let band_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    let stride = width * components;

    let mut pixels = vec![0i32; height * stride];
    let mut residuals = vec![0i32; height * stride];
    let mut offset = 2;

    for b in 0..band_count {
        let y_start = b * band_height;
        if y_start >= height {
            break;
        }
        let y_end = (y_start + band_height).min(height);

        // 读取条带头 [mode][k][len]
        if offset + 6 > data.len() {
            return Err(CrfError::InsufficientData {
                expected: offset + 6,
                actual: data.len(),
            });
        }
        let mode = PredictionMode::from_u8(data[offset]);
        let k = data[offset + 1];
        let len = u32::from_le_bytes([
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
            data[offset + 5],
        ]) as usize;
        offset += 6;

        if offset + len > data.len() {
            return Err(CrfError::InsufficientData {
                expected: offset + len,
                actual: data.len(),
            });
        }
        let band_data = &data[offset..offset + len];
        offset += len;

        // 解码本条带残差并写入整帧残差缓冲的对应行段
        let band_pixels = (y_end - y_start) * stride;
        let band_res =
            rle_golomb::RleGolombDecoder::new(band_data, k).decode_signed_array(band_pixels);
        let start = y_start * stride;
        let stop = y_end * stride;
        residuals[start..stop].copy_from_slice(&band_res);

        // 撤销本条带预测（依赖 pixels 中前序条带已重建的上文）
        undo_prediction_range(
            &residuals,
            &mut pixels,
            width,
            components,
            mode,
            y_start,
            y_end,
        );
    }

    Ok(pixels)
}
