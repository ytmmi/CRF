use crate::crf::format::zigzag_inverse;
use crate::crf::transform::dct4x4_inverse as hadamard_inverse;

use super::rle_golomb::RleGolombDecoder;

/// 解码单个块
fn decode_block(data: &[u8], offset: &mut usize, block_size: usize) -> Vec<i32> {
    if *offset + 1 >= data.len() {
        return vec![0; block_size * block_size];
    }

    // 读取 k 值
    let k = data[*offset];
    *offset += 1;

    // 读取块数据大小
    if *offset + 4 > data.len() {
        return vec![0; block_size * block_size];
    }
    let size = u32::from_le_bytes([
        data[*offset],
        data[*offset + 1],
        data[*offset + 2],
        data[*offset + 3],
    ]) as usize;
    *offset += 4;

    // 读取块数据
    if *offset + size > data.len() {
        return vec![0; block_size * block_size];
    }
    let block_data = &data[*offset..*offset + size];
    *offset += size;

    // RLE+Golomb 解码（匹配编码器格式）
    let mut decoder = RleGolombDecoder::new(block_data, k);
    let pixel_count = block_size * block_size;
    let scanned = decoder.decode_signed_array(pixel_count);

    // Zigzag 逆扫描
    let transformed = zigzag_inverse(&scanned, block_size);

    // 逆变换
    hadamard_inverse(&transformed)
}

/// 变换解码器
pub struct TransformDecoder<'a> {
    data: &'a [u8],
    offset: usize,
    block_size: usize,
    width: usize,
    height: usize,
}

impl<'a> TransformDecoder<'a> {
    /// 创建新的变换解码器
    pub fn new(data: &'a [u8], width: usize, height: usize) -> Self {
        let _offset = 0;
        let block_size = if !data.is_empty() {
            data[0] as usize
        } else {
            4
        };
        TransformDecoder {
            data,
            offset: 1, // 跳过块大小字节
            block_size,
            width,
            height,
        }
    }

    /// 解码整个帧
    pub fn decode_frame(&mut self) -> Vec<i32> {
        let mut pixels = vec![0i32; self.width * self.height];

        for by in (0..self.height).step_by(self.block_size) {
            for bx in (0..self.width).step_by(self.block_size) {
                let block = decode_block(self.data, &mut self.offset, self.block_size);

                for y in 0..self.block_size {
                    for x in 0..self.block_size {
                        let py = by + y;
                        let px = bx + x;
                        if py < self.height && px < self.width {
                            let block_idx = y * self.block_size + x;
                            if block_idx < block.len() {
                                pixels[py * self.width + px] = block[block_idx];
                            }
                        }
                    }
                }
            }
        }

        pixels
    }
}

/// 解码变换编码的帧数据
pub fn decode_frame_transform(data: &[u8], width: usize, height: usize) -> Vec<i32> {
    let mut decoder = TransformDecoder::new(data, width, height);
    decoder.decode_frame()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::encoder::transform::encode_frame_transform;

    #[test]
    fn test_transform_roundtrip() {
        let width = 8;
        let height = 8;
        let original: Vec<i32> = (0..64).map(|i| (i as i32) - 32).collect();
        let block_size = 4;

        // 编码
        let encoded = encode_frame_transform(&original, width, height, block_size).unwrap();

        // 解码
        let decoded = decode_frame_transform(&encoded, width, height);

        assert_eq!(original, decoded);
    }
}
