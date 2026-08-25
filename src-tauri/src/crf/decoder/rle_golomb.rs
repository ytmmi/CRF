use crate::crf::format::zigzag_decode;

/// RLE+Golomb 混合解码器
pub struct RleGolombDecoder<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
    k: u8,
}

impl<'a> RleGolombDecoder<'a> {
    pub fn new(data: &'a [u8], k: u8) -> Self {
        RleGolombDecoder {
            data,
            byte_pos: 0,
            bit_pos: 0,
            k,
        }
    }

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

    /// 解码单个 Golomb 值
    fn decode_golomb(&mut self) -> u32 {
        let mut q = 0u32;
        while self.read_bit() {
            q += 1;
        }
        let r = self.read_bits(self.k as u32);
        (q << self.k) | r
    }

    /// 解码指数哥伦布无符号整数
    ///
    /// 结构：[m 个 '0'] + [(m+1) 位二进制 (v+1)]，与编码器 encode_exp_golomb 对应。
    /// while 循环读到的首个 '1' 即 (v+1) 的最高位，再补读低 m 位后需减 1 还原 v。
    fn decode_exp_golomb(&mut self) -> u32 {
        let mut m = 0u32;
        while !self.read_bit() {
            m += 1;
            if m > 31 {
                // 防止损坏数据导致无限循环
                return 0;
            }
        }
        // 读取剩余 m 位低位数据，与隐含的最高位 1 组合为 (v+1)
        let mut value = 1u32;
        for _ in 0..m {
            value = (value << 1) | (self.read_bit() as u32);
        }
        value - 1
    }

    /// 解码有符号整数数组
    pub fn decode_signed_array(&mut self, count: usize) -> Vec<i32> {
        let mut output = Vec::with_capacity(count);
        let mut remaining = count;

        while remaining > 0 {
            let is_rle = self.read_bit();

            if is_rle {
                // 零值行程编码（exp-Golomb 行程长度）
                let run_length = self.decode_exp_golomb() as usize;
                let actual = run_length.min(remaining);
                output.resize(output.len() + actual, 0);
                remaining -= actual;
            } else {
                // 非零值编码
                let unsigned = self.decode_golomb();
                let signed = zigzag_decode(unsigned);
                output.push(signed);
                remaining -= 1;
            }
        }

        output
    }
}

/// 解码残差帧数据（RLE+Golomb 混合方式）
pub fn decode_frame_rle_golomb(data: &[u8], k: u8, pixel_count: usize) -> Vec<i32> {
    let mut decoder = RleGolombDecoder::new(data, k);
    decoder.decode_signed_array(pixel_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::encoder::rle_golomb::RleGolombEncoder;

    #[test]
    fn test_rle_golomb_roundtrip_zeros() {
        let values = vec![0i32; 1000];
        let mut encoder = RleGolombEncoder::new(0);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let decoded = decode_frame_rle_golomb(&encoded, 0, values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_roundtrip_mixed() {
        let values = vec![0, 0, 0, 1, 2, -1, 0, 0, 5, -3, 0, 0, 0, 0, 0, 10];
        let mut encoder = RleGolombEncoder::new(1);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let decoded = decode_frame_rle_golomb(&encoded, 1, values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_roundtrip_single() {
        let values = vec![42];
        let mut encoder = RleGolombEncoder::new(3);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let decoded = decode_frame_rle_golomb(&encoded, 3, values.len());
        assert_eq!(values, decoded);
    }
}
