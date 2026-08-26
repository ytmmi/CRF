use crate::crf::error::CrfResult;
use crate::crf::core::entropy::scan::zigzag_encode;

/// 指数哥伦布编码器
///
/// 适用于数值动态范围较大的残差。
/// 编码结构：[m 个 '0'] + [(m+1) 位二进制表示 (v + 1)]
/// 其中 m = floor(log2(v + 1))
pub struct ExpGolombEncoder {
    /// 输出缓冲区
    buffer: Vec<u8>,
    /// 当前字节
    current_byte: u8,
    /// 当前位位置（0-7）
    bit_pos: u8,
}

impl ExpGolombEncoder {
    /// 创建新的编码器
    pub fn new() -> Self {
        ExpGolombEncoder {
            buffer: Vec::new(),
            current_byte: 0,
            bit_pos: 0,
        }
    }

    /// 写入单个比特
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

    /// 写入多个连续的比特
    fn write_bits(&mut self, bits: u32, count: u32) {
        for i in (0..count).rev() {
            self.write_bit((bits >> i) & 1 == 1);
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

    /// 编码非负整数
    ///
    /// v1.13 S6：[m 个 '0'] + [(m+1) 位 (v+1)] 合并单次写入——前导零天然
    /// 是高位填充，总位宽 2m+1 下值本身即低位对齐；m 超限分段保底
    ///（u32 值域内 m ≤ 31 → 总位 ≤ 63，仅 value 近满域时触发）。
    pub fn encode_value(&mut self, value: u32) {
        if value == 0 {
            // 特殊情况：0 编码为 "1"
            self.write_bit(true);
            return;
        }

        // 计算 m = floor(log2(value + 1))
        let m = 31 - (value + 1).leading_zeros();
        let total = 2 * m + 1;
        if total <= 57 {
            self.write_bits_msb((value + 1) as u64, total);
        } else {
            let mut left = m as u64;
            while left > 57 {
                self.write_bits_msb(0, 57);
                left -= 57;
            }
            self.write_bits_msb(0, left as u32);
            self.write_bits(value + 1, m + 1);
        }
    }

    /// 编码有符号整数（使用 Zigzag 映射）
    pub fn encode_signed(&mut self, value: i32) {
        let unsigned = zigzag_encode(value);
        self.encode_value(unsigned);
    }

    /// 编码数值数组
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn encode_array(&mut self, values: &[u32]) {
        for &value in values {
            self.encode_value(value);
        }
    }

    /// 编码有符号数值数组
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
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    #[allow(dead_code)] // 缓冲区大小查询：与 finish 对称的公共接口
    pub fn buffer_len(&self) -> usize {
        if self.bit_pos > 0 {
            self.buffer.len() + 1
        } else {
            self.buffer.len()
        }
    }
}

/// 编码残差帧数据（指数哥伦布方式）
pub fn encode_frame_exp_golomb(pixels: &[i32]) -> CrfResult<Vec<u8>> {
    let mut encoder = ExpGolombEncoder::new();
    encoder.encode_signed_array(pixels);
    Ok(encoder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::decoder::exp_golomb::ExpGolombDecoder;

    #[test]
    fn test_encode_zero() {
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_value(0);
        let bytes = encoder.finish();

        // 0 应编码为 "1" -> 10000000
        assert_eq!(bytes[0], 0x80);
    }

    #[test]
    fn test_encode_one() {
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_value(1);
        let bytes = encoder.finish();

        // 1: m=0, 编码为 "0" + "10" -> 01000000
        assert_eq!(bytes[0] >> 6, 0b01);
    }

    #[test]
    fn test_encode_two() {
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_value(2);
        let bytes = encoder.finish();

        // 标准指数哥伦布 ue(v): value=2 → value+1=3="11"(2位), m=floor(log2(3))=1
        // 编码为 "0"(m个前导0) + "11"(m+1位数据) -> 011xxxxx
        let bits = bytes_to_bits(&bytes);
        assert_eq!(bits[0], false); // 前导 0
        assert_eq!(bits[1], true); // 数据 11 的第一位
        assert_eq!(bits[2], true); // 数据 11 的第二位
    }

    #[test]
    fn test_encode_signed_array_roundtrip() {
        let original = vec![0, 1, -1, 2, -2, 100, -100, 1000, -1000];

        // 编码
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_signed_array(&original);
        let encoded = encoder.finish();

        // 解码验证
        let mut decoder = ExpGolombDecoder::new(&encoded);
        let decoded: Vec<i32> = (0..original.len())
            .map(|_| decoder.decode_signed())
            .collect();

        assert_eq!(original, decoded);
    }

    fn bytes_to_bits(bytes: &[u8]) -> Vec<bool> {
        let mut bits = Vec::new();
        for byte in bytes {
            for i in (0..8).rev() {
                bits.push((byte >> i) & 1 == 1);
            }
        }
        bits
    }
}
