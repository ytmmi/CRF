use crate::crf::error::CrfResult;
use crate::crf::core::entropy::golomb::{adaptive_k, block_adaptive_k};
use crate::crf::core::entropy::scan::zigzag_encode;

/// Golomb-Rice 编码器
///
/// 适用于残差数据集中在零附近的场景。
/// 编码结构：[q 个 '1'] + ['0'] + [k 位二进制 r]
/// 其中 q = v >> k, r = v & ((1 << k) - 1)
pub struct GolombEncoder {
    /// 编码参数 k
    k: u8,
    /// 输出缓冲区
    buffer: Vec<u8>,
    /// 当前字节
    current_byte: u8,
    /// 当前位位置（0-7）
    bit_pos: u8,
}

impl GolombEncoder {
    /// 创建新的编码器，指定 k 值
    pub fn new(k: u8) -> Self {
        GolombEncoder {
            k,
            buffer: Vec::new(),
            current_byte: 0,
            bit_pos: 0,
        }
    }

    /// 创建自适应 k 值的编码器
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn adaptive(values: &[u32]) -> Self {
        let k = adaptive_k(values);
        Self::new(k)
    }

    /// 获取当前的 k 值
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn k(&self) -> u8 {
        self.k
    }

    /// 写入单个比特
    #[allow(dead_code)] // 逐位写入参考实现：与批量位写路径对拍用
    fn write_bit(&mut self, bit: bool) {
        if bit {
            self.current_byte |= 1 << (7 - self.bit_pos);
        }
        self.bit_pos += 1;
        if self.bit_pos >= 8 {
            self.buffer.push(self.current_byte);
            self.current_byte = 0;
            self.bit_pos = 0;
        }
    }

    /// 写入多个连续的 1
    #[allow(dead_code)] // 逐位写入参考实现：与批量位写路径对拍用
    fn write_ones(&mut self, count: u32) {
        for _ in 0..count {
            self.write_bit(true);
        }
    }

    /// 批量位写入原语（v1.13 S6）：将 `bits` 的低 n 位以 MSB 先行序写入。
    ///
    /// 与逐位 write_bit 完全等价——大端位流拼接满足结合律，批量搬运
    /// 仅改变落盘节奏、不改变任何位序。n ≤ 57 保证移位不溢出 u64。
    fn write_bits_msb(&mut self, bits: u64, n: u32) {
        debug_assert!(n <= 57);
        debug_assert!(bits >> n == 0, "bits 超出 n 位宽度");
        let bit_pos = self.bit_pos as u32;
        let fill = 8 - bit_pos; // 当前字节剩余空位（1..=8）
        if n <= fill {
            // 单字节内完成
            self.current_byte |= ((bits << (fill - n)) & 0xFF) as u8;
            self.bit_pos += n as u8;
            if self.bit_pos == 8 {
                let byte = std::mem::replace(&mut self.current_byte, 0);
                self.buffer.push(byte);
                self.bit_pos = 0;
            }
            return;
        }
        // 跨字节：先以高位填满当前字节
        self.current_byte |= (bits >> (n - fill)) as u8;
        let byte = std::mem::replace(&mut self.current_byte, 0);
        self.buffer.push(byte);
        let rest = bits & ((1u64 << (n - fill)) - 1);
        let mut remain = n - fill;
        while remain >= 8 {
            remain -= 8;
            self.buffer.push((rest >> remain) as u8);
        }
        self.current_byte = if remain > 0 {
            ((rest & ((1u64 << remain) - 1)) << (8 - remain)) as u8
        } else {
            0
        };
        self.bit_pos = remain as u8;
    }

    /// 编码单个无符号整数
    ///
    /// v1.13 S6：[q 个 '1'][终止 '0'][k 位余数] 合并单次批量写入；
    /// 商超长时按批直写全 '1'，尾批与终止符+余数拼成单个 u64 落盘。
    pub fn encode_value(&mut self, value: u32) {
        const MAX_BATCH: u64 = 57;
        let k = self.k as u64;
        let r = (value & ((1u32 << self.k) - 1)) as u64;
        let mut q = (value >> self.k) as u64;

        let tail_room = MAX_BATCH - 1 - k; // 尾批 '1' 的最大数量
        while q > tail_room {
            self.write_bits_msb((1u64 << tail_room) - 1, tail_room as u32);
            q -= tail_room;
        }
        let bits: u64 = (((1u64 << q) - 1) << (1 + k)) | r;
        self.write_bits_msb(bits, q as u32 + 1 + k as u32);
    }

    /// 编码有符号整数（使用 Zigzag 映射）
    #[allow(dead_code)] // 有符号编码入口：测试与对称解码路径使用
    pub fn encode_signed(&mut self, value: i32) {
        let unsigned = zigzag_encode(value);
        self.encode_value(unsigned);
    }

    /// 编码数值数组
    pub fn encode_array(&mut self, values: &[u32]) {
        for &value in values {
            self.encode_value(value);
        }
    }

    /// 编码有符号数值数组
    #[allow(dead_code)] // 批量有符号编码：测试路径使用
    pub fn encode_signed_array(&mut self, values: &[i32]) {
        for &value in values {
            self.encode_signed(value);
        }
    }

    /// 完成编码，刷新缓冲区
    pub fn finish(mut self) -> Vec<u8> {
        // 如果有未完成的字节，添加到缓冲区
        if self.bit_pos > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }

    /// 获取当前缓冲区大小（字节）
    #[allow(dead_code)] // 缓冲区大小查询：与 finish 对称的公共接口
    pub fn buffer_len(&self) -> usize {
        if self.bit_pos > 0 {
            self.buffer.len() + 1
        } else {
            self.buffer.len()
        }
    }
}

/// 编码残差帧数据（Golomb-Rice 方式）
///
/// 输入：残差帧像素数据（有符号整数）
/// 输出：编码后的字节数据
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn encode_frame_golomb(pixels: &[i32], k: u8) -> CrfResult<(Vec<u8>, u8)> {
    // 将有符号值映射为无符号值
    let unsigned_values: Vec<u32> = pixels.iter().map(|&v| zigzag_encode(v)).collect();

    // 编码
    let mut encoder = GolombEncoder::new(k);
    encoder.encode_array(&unsigned_values);
    let encoded = encoder.finish();

    Ok((encoded, k))
}

/// 编码残差帧数据（自适应 k 值）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn encode_frame_golomb_adaptive(pixels: &[i32]) -> CrfResult<(Vec<u8>, u8)> {
    // 将有符号值映射为无符号值
    let unsigned_values: Vec<u32> = pixels.iter().map(|&v| zigzag_encode(v)).collect();

    // 自适应选择 k
    let k = adaptive_k(&unsigned_values);

    // 编码
    let mut encoder = GolombEncoder::new(k);
    encoder.encode_array(&unsigned_values);
    let encoded = encoder.finish();

    Ok((encoded, k))
}

/// 编码帧数据（块级自适应 k 值）
///
/// 对每个 8x8 块使用独立的 k 值，适用于高熵帧（如原始帧）
pub fn encode_frame_golomb_block_adaptive(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
) -> CrfResult<(Vec<u8>, u8)> {
    // 将有符号值映射为无符号值
    let unsigned_values: Vec<u32> = pixels.iter().map(|&v| zigzag_encode(v)).collect();

    // 计算块级 k 值
    let k_values = block_adaptive_k(&unsigned_values, width, height, components, 8);

    // 编码：先写入 k 值表，再编码数据
    let mut buffer = Vec::new();

    // 写入块数（u32 LE，因为块数可能超过255）
    let block_count = k_values.len() as u32;
    buffer.extend_from_slice(&block_count.to_le_bytes());

    // 写入 k 值表（每个块 1 字节）
    buffer.extend_from_slice(&k_values);

    // 按块编码数据
    let block_size = 8;
    let mut encoded_data = Vec::new();
    let mut k_idx = 0;

    for by in (0..height).step_by(block_size) {
        for bx in (0..width).step_by(block_size) {
            let k = k_values.get(k_idx).copied().unwrap_or(0);
            k_idx += 1;

            // 收集块内像素
            let mut block_pixels = Vec::new();
            for y in by..std::cmp::min(by + block_size, height) {
                for x in bx..std::cmp::min(bx + block_size, width) {
                    for c in 0..components {
                        let idx = (y * width + x) * components + c;
                        if idx < unsigned_values.len() {
                            block_pixels.push(unsigned_values[idx]);
                        }
                    }
                }
            }

            // 用该块的 k 值编码
            let mut encoder = GolombEncoder::new(k);
            encoder.encode_array(&block_pixels);
            let block_encoded = encoder.finish();
            encoded_data.extend_from_slice(&block_encoded);
        }
    }

    // 合并：k 值表 + 编码数据
    buffer.extend_from_slice(&encoded_data);

    Ok((buffer, 0xFF)) // 返回 coding_params = 0xFF 表示块级 k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::decoder::golomb::GolombDecoder;

    #[test]
    fn test_encode_value() {
        let mut encoder = GolombEncoder::new(2);

        // 编码值 0: q=0, r=0 -> 0 + 00
        encoder.encode_value(0);
        let bits = finish_to_bits(encoder);

        // 编码值 5: q=1 (5>>2=1), r=1 (5&3=1) -> 10 + 01
        let mut encoder2 = GolombEncoder::new(2);
        encoder2.encode_value(5);
        let _bits2 = finish_to_bits(encoder2);

        // 验证第一个编码
        assert_eq!(bits[0], false); // 商 0 的前导 0
        assert_eq!(bits[1], false); // 余数位 1
        assert_eq!(bits[2], false); // 余数位 0
    }

    #[test]
    fn test_encode_signed() {
        let values = vec![0, 1, -1, 2, -2, 10, -10];
        let mut encoder = GolombEncoder::new(2);

        for &v in &values {
            encoder.encode_signed(v);
        }

        let encoded = encoder.finish();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_array_roundtrip() {
        let original = vec![0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let k = 2;

        // 编码
        let mut encoder = GolombEncoder::new(k);
        encoder.encode_array(&original);
        let encoded = encoder.finish();

        // 解码验证
        let mut decoder = GolombDecoder::new(&encoded, k);
        let decoded: Vec<u32> = (0..original.len())
            .map(|_| decoder.decode_value())
            .collect();

        assert_eq!(original, decoded);
    }

    fn finish_to_bits(encoder: GolombEncoder) -> Vec<bool> {
        let bytes = encoder.finish();
        let mut bits = Vec::new();
        for byte in &bytes {
            for i in (0..8).rev() {
                bits.push((byte >> i) & 1 == 1);
            }
        }
        bits
    }
}
