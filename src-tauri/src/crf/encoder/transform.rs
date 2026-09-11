use crate::crf::core::entropy::golomb::adaptive_k;
use crate::crf::core::entropy::scan::{zigzag_encode, zigzag_scan};
use crate::crf::error::{CrfError, CrfResult};

use super::rle_golomb::RleGolombEncoder;

pub use crate::crf::core::transform::dct4x4_forward as hadamard_forward;
/// 变换核：lifting 可逆整数 DCT 4×4（共享实现见 crf/transform_core.rs）。
/// 相比原 Hadamard 核，频域能量集中性对自然/插画图像更优，
/// 且 lifting 构造保证严格可逆。
/// 变换编码器
pub struct TransformEncoder {
    block_size: usize,
    encoded_data: Vec<u8>,
}

impl TransformEncoder {
    pub fn new(block_size: usize) -> CrfResult<Self> {
        if block_size != 4 && block_size != 8 {
            return Err(CrfError::InvalidBlockSize(block_size as u16));
        }
        Ok(TransformEncoder {
            block_size,
            encoded_data: Vec::new(),
        })
    }

    fn encode_block(&mut self, block: &[i32]) -> CrfResult<()> {
        // 对块进行 Hadamard 变换
        let transformed = if self.block_size == 4 {
            hadamard_forward(block)
        } else {
            // 8x8 块拆分为4个4x4子块分别变换
            let mut result = vec![0i32; 64];
            for by in (0..8).step_by(4) {
                for bx in (0..8).step_by(4) {
                    let mut sub_block = [0i32; 16];
                    for y in 0..4 {
                        for x in 0..4 {
                            sub_block[y * 4 + x] = block[(by + y) * 8 + (bx + x)];
                        }
                    }
                    let transformed = hadamard_forward(&sub_block);
                    for y in 0..4 {
                        for x in 0..4 {
                            result[(by + y) * 8 + (bx + x)] = transformed[y * 4 + x];
                        }
                    }
                }
            }
            result
        };

        // Zigzag 扫描
        let scanned = zigzag_scan(&transformed, self.block_size);

        // RLE+Golomb 混合编码（对变换后稀疏数据更高效）
        let unsigned_for_k: Vec<u32> = scanned.iter().map(|&v| zigzag_encode(v)).collect();
        let k = adaptive_k(&unsigned_for_k);

        self.encoded_data.push(k);

        // 使用 RLE+Golomb 编码（直接传入有符号值）
        let mut encoder = RleGolombEncoder::new(k);
        encoder.encode_signed_array(&scanned);
        let encoded_block = encoder.finish();

        let size = encoded_block.len() as u32;
        self.encoded_data.extend_from_slice(&size.to_le_bytes());
        self.encoded_data.extend_from_slice(&encoded_block);

        Ok(())
    }

    pub fn encode_frame(
        &mut self,
        pixels: &[i32],
        width: usize,
        height: usize,
    ) -> CrfResult<Vec<u8>> {
        self.encoded_data.clear();
        self.encoded_data.push(self.block_size as u8);

        let mut block = vec![0i32; self.block_size * self.block_size];

        for by in (0..height).step_by(self.block_size) {
            for bx in (0..width).step_by(self.block_size) {
                for y in 0..self.block_size {
                    for x in 0..self.block_size {
                        let py = by + y;
                        let px = bx + x;
                        block[y * self.block_size + x] = if py < height && px < width {
                            pixels[py * width + px]
                        } else {
                            0
                        };
                    }
                }
                self.encode_block(&block)?;
            }
        }

        Ok(self.encoded_data.clone())
    }
}

pub fn encode_frame_transform(
    pixels: &[i32],
    width: usize,
    height: usize,
    block_size: usize,
) -> CrfResult<Vec<u8>> {
    let mut encoder = TransformEncoder::new(block_size)?;
    encoder.encode_frame(pixels, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::core::transform::dct4x4_inverse as hadamard_inverse;

    #[test]
    fn test_hadamard_roundtrip() {
        let original = vec![
            10, 20, 30, 40, 50, 60, 70, 80, 15, 25, 35, 45, 55, 65, 75, 85,
        ];
        let transformed = hadamard_forward(&original);
        let restored = hadamard_inverse(&transformed);

        for (a, b) in original.iter().zip(restored.iter()) {
            assert_eq!(a, b);
        }
    }
}
