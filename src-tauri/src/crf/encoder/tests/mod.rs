//! encoder 集成与路径竞争测试
//!
//! 按功能领域拆分为：
//! - [`roundtrip`]：基础编解码往返（golomb / banded / palette / planar）
//! - [`lossy`]：有损模式误差边界、golden 还原、噪声感知 A/B
//! - [`rct_bypass`]：v1.13 RCT 首帧自适应（RGB 直通 vs YCoCg-R 双路竞争）

mod lossy;
mod rct_bypass;
mod roundtrip;

use crate::crf::core::domain::{ColorFormat, ImageData};

/// 测试用 64 行条带高度常量（与 encoder/adaptive.rs 的 BAND_HEIGHT_ALT 同值）
pub(super) const BAND_HEIGHT_ALT_TEST: usize = 64;

pub(super) fn create_test_frames(count: usize, width: u16, height: u16) -> Vec<ImageData> {
    (0..count)
        .map(|i| {
            let pixels: Vec<i32> = (0..(width as usize * height as usize))
                .map(|j| ((i * 10 + j) % 256) as i32 - 128)
                .collect();
            ImageData {
                width,
                height,
                bit_depth: 8,
                color_format: ColorFormat::Gray,
                pixels,
            }
        })
        .collect()
}

/// 构造纯索引流的理论基线大小（直接 RLE+Golomb 编码原始索引）
pub(super) fn indices_only(pixels: &[i32]) -> Vec<i32> {
    use std::collections::HashMap;
    let mut map: HashMap<i32, i32> = HashMap::new();
    let mut order: Vec<i32> = Vec::new();
    pixels
        .iter()
        .map(|&v| {
            let next = map.len() as i32;
            *map.entry(v).or_insert_with(|| {
                order.push(v);
                next
            })
        })
        .collect()
}
