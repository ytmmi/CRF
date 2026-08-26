use crate::crf::core::entropy::scan::zigzag_decode;

/// Golomb-Rice 解码器
pub struct GolombDecoder<'a> {
    /// 编码数据
    data: &'a [u8],
    /// 当前字节位置
    byte_pos: usize,
    /// 当前位位置（0-7）
    bit_pos: u8,
    /// 解码参数 k
    k: u8,
}

impl<'a> GolombDecoder<'a> {
    /// 创建新的解码器
    pub fn new(data: &'a [u8], k: u8) -> Self {
        GolombDecoder {
            data,
            byte_pos: 0,
            bit_pos: 0,
            k,
        }
    }

    /// 读取单个比特
    fn read_bit(&mut self) -> bool {
        if self.byte_pos >= self.data.len() {
            return false;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1 == 1;
        self.bit_pos += 1;
        if self.bit_pos >= 8 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
        bit
    }

    /// 读取多个比特并返回 u32
    fn read_bits(&mut self, count: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..count {
            value <<= 1;
            if self.read_bit() {
                value |= 1;
            }
        }
        value
    }

    /// 解码单个无符号整数
    pub fn decode_value(&mut self) -> u32 {
        // 读取商（连续的 1 的个数）
        let mut q = 0u32;
        while self.read_bit() {
            q += 1;
        }

        // 读取余数（k 位）
        let r = self.read_bits(self.k as u32);

        // 组合结果
        (q << self.k) | r
    }

    /// 解码有符号整数
    pub fn decode_signed(&mut self) -> i32 {
        let unsigned = self.decode_value();
        zigzag_decode(unsigned)
    }

    /// 解码指定数量的值
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn decode_array(&mut self, count: usize) -> Vec<u32> {
        (0..count).map(|_| self.decode_value()).collect()
    }

    /// 解码指定数量的有符号值
    pub fn decode_signed_array(&mut self, count: usize) -> Vec<i32> {
        (0..count).map(|_| self.decode_signed()).collect()
    }

    /// 检查是否还有数据
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn has_remaining(&self) -> bool {
        self.byte_pos < self.data.len()
    }
}

/// 解码残差帧数据（Golomb-Rice 方式）
pub fn decode_frame_golomb(
    data: &[u8],
    k: u8,
    pixel_count: usize,
    width: usize,
    height: usize,
    components: usize,
) -> Vec<i32> {
    if k == 0xFF {
        // 块级自适应 k 解码
        decode_frame_golomb_block_adaptive(data, pixel_count, width, height, components)
    } else {
        // 全局 k 解码
        let mut decoder = GolombDecoder::new(data, k);
        decoder.decode_signed_array(pixel_count)
    }
}

/// 解码块级自适应 k 编码的帧数据
///
/// 数据格式：[块数(u32 LE)] + [k值表(u8) × 块数] + [编码数据...]
/// 每个块的编码数据是独立的 Golomb-Rice 流
fn decode_frame_golomb_block_adaptive(
    data: &[u8],
    pixel_count: usize,
    width: usize,
    height: usize,
    components: usize,
) -> Vec<i32> {
    if data.len() < 4 {
        return vec![0; pixel_count];
    }

    // 读取块数（u32 LE）
    let block_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;

    // 读取 k 值表
    let k_start = 4;
    let k_end = k_start + block_count;
    if k_end > data.len() {
        return vec![0; pixel_count];
    }

    let k_values: Vec<u8> = data[k_start..k_end].to_vec();

    // 编码数据从 k_end 开始
    let encoded_data = &data[k_end..];

    // 按块解码
    let block_size = 8;
    let mut output = vec![0i32; pixel_count];
    let mut data_offset: usize = 0;
    let mut k_idx = 0;

    for by in (0..height).step_by(block_size) {
        for bx in (0..width).step_by(block_size) {
            let k = k_values.get(k_idx).copied().unwrap_or(0);
            k_idx += 1;

            // 计算块内实际尺寸
            let bw = std::cmp::min(block_size, width - bx);
            let bh = std::cmp::min(block_size, height - by);
            let block_pixel_count = bw * bh * components;

            // 用该块的 k 值解码
            let mut decoder = GolombDecoder::new(&encoded_data[data_offset..], k);
            let block_values = decoder.decode_signed_array(block_pixel_count);
            // 对齐到下一个字节边界（Golomb是bit-packed，解码可能停在字节中间）
            data_offset += if decoder.bit_pos > 0 {
                decoder.byte_pos + 1
            } else {
                decoder.byte_pos
            };

            // 将解码值写入正确位置
            let mut val_idx = 0;
            for y in by..by + bh {
                for x in bx..bx + bw {
                    for c in 0..components {
                        let out_idx = (y * width + x) * components + c;
                        if out_idx < output.len() && val_idx < block_values.len() {
                            output[out_idx] = block_values[val_idx];
                            val_idx += 1;
                        }
                    }
                }
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::encoder::golomb::GolombEncoder;

    #[test]
    fn test_decode_single_value() {
        // 编码值 5，k=2: q=1, r=1 -> "10" + "01"
        let mut encoder = GolombEncoder::new(2);
        encoder.encode_value(5);
        let encoded = encoder.finish();

        let mut decoder = GolombDecoder::new(&encoded, 2);
        let decoded = decoder.decode_value();
        assert_eq!(decoded, 5);
    }

    #[test]
    fn test_decode_array_roundtrip() {
        let original = vec![0u32, 1, 2, 3, 4, 5, 10, 20, 50, 100];
        let k = 2;

        // 编码
        let mut encoder = GolombEncoder::new(k);
        encoder.encode_array(&original);
        let encoded = encoder.finish();

        // 解码
        let mut decoder = GolombDecoder::new(&encoded, k);
        let decoded = decoder.decode_array(original.len());
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_signed_array_roundtrip() {
        let original = vec![0, 1, -1, 2, -2, 10, -10, 100, -100];
        let k = 3;

        // 编码
        let mut encoder = GolombEncoder::new(k);
        encoder.encode_signed_array(&original);
        let encoded = encoder.finish();

        // 解码
        let mut decoder = GolombDecoder::new(&encoded, k);
        let decoded = decoder.decode_signed_array(original.len());
        assert_eq!(original, decoded);
    }
}
