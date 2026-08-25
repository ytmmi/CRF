use crate::crf::format::zigzag_decode;

/// 指数哥伦布解码器
pub struct ExpGolombDecoder<'a> {
    /// 编码数据
    data: &'a [u8],
    /// 当前字节位置
    byte_pos: usize,
    /// 当前位位置（0-7）
    bit_pos: u8,
}

impl<'a> ExpGolombDecoder<'a> {
    /// 创建新的解码器
    pub fn new(data: &'a [u8]) -> Self {
        ExpGolombDecoder {
            data,
            byte_pos: 0,
            bit_pos: 0,
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

    /// 解码非负整数
    pub fn decode_value(&mut self) -> u32 {
        // 读取前导 0 的个数（m）
        let mut m = 0u32;
        while !self.read_bit() {
            m += 1;
            if m > 30 {
                // 防止无限循环
                return 0;
            }
        }

        if m == 0 {
            // 特殊情况：值为 0
            return 0;
        }

        // 读取 m 位数据
        let remaining = self.read_bits(m);

        // 组合结果：(1 << m) + remaining - 1
        (1 << m) + remaining - 1
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

/// 解码残差帧数据（指数哥伦布方式）
pub fn decode_frame_exp_golomb(data: &[u8], pixel_count: usize) -> Vec<i32> {
    let mut decoder = ExpGolombDecoder::new(data);
    decoder.decode_signed_array(pixel_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::encoder::exp_golomb::ExpGolombEncoder;

    #[test]
    fn test_decode_zero() {
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_value(0);
        let encoded = encoder.finish();

        let mut decoder = ExpGolombDecoder::new(&encoded);
        let decoded = decoder.decode_value();
        assert_eq!(decoded, 0);
    }

    #[test]
    fn test_decode_one() {
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_value(1);
        let encoded = encoder.finish();

        let mut decoder = ExpGolombDecoder::new(&encoded);
        let decoded = decoder.decode_value();
        assert_eq!(decoded, 1);
    }

    #[test]
    fn test_decode_array_roundtrip() {
        let original = vec![0u32, 1, 2, 3, 4, 5, 10, 20, 50, 100, 1000];

        // 编码
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_array(&original);
        let encoded = encoder.finish();

        // 解码
        let mut decoder = ExpGolombDecoder::new(&encoded);
        let decoded = decoder.decode_array(original.len());
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_signed_array_roundtrip() {
        let original = vec![0, 1, -1, 2, -2, 10, -10, 100, -100, 1000, -1000];

        // 编码
        let mut encoder = ExpGolombEncoder::new();
        encoder.encode_signed_array(&original);
        let encoded = encoder.finish();

        // 解码
        let mut decoder = ExpGolombDecoder::new(&encoded);
        let decoded = decoder.decode_signed_array(original.len());
        assert_eq!(original, decoded);
    }
}
