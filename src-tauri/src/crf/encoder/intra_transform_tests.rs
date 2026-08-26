//! frame_type=8 往返测试：encoder→decoder 对称验证

#[cfg(test)]
mod tests {
    use crate::crf::encoder::intra_transform::encode_intra_transform_payload;
    use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};

    fn make_frame(w: u16, h: u16, seed: u64) -> ImageData {
        let mut s = seed;
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let base = ((x as i32 * 7 + y as i32 * 13) % 200) + 28;
                let n = ((s >> 33) % 13) as i32 - 6;
                px.push((base + n).clamp(0, 255));
                px.push((base * 2 / 3 + 40 + n).clamp(0, 255));
                px.push((200 - base + n).clamp(0, 255));
            }
        }
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: px,
        }
    }

    #[test]
    fn frame_type8_lossless_roundtrip() {
        // 无损（q_step=1 → 量化恒等 → 系数=残差，重建精确）
        let img = make_frame(32, 24, 0xDEAD_BEEF_CAFE);
        let payload = encode_intra_transform_payload(
            &img,
            CompressionType::GolombRice,
            1, // q_step=1 → 近无损
            0, // deadzone
            1, // chroma_step
            0, // chroma_bias
        )
        .expect("encode failed");

        let decoded = crate::crf::decoder::intra_transform::decode_intra_transform(
            &payload,
            img.width as usize,
            img.height as usize,
            3,
            1,
            1,
        )
        .expect("decode failed");

        // q_step=1 时 transform skip 路径残差精确重建；
        // DCT 路径 lifting 可逆 + Q=1 恒等 → 也精确
        // 但 DC 预测模式的"直通量化"在 Q=1 下 level=round(residual/1)=residual ✓
        assert_eq!(decoded.len(), img.pixels.len(), "解码长度不匹配");
        let max_err = decoded
            .iter()
            .zip(&img.pixels)
            .map(|(a, b)| (a - b).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(max_err <= 1, "q_step=1 往返 max_err={} 应 ≤1", max_err);
    }

    #[test]
    fn frame_type8_lossy_reconstruction() {
        // 有损（q_step=5）：解码成功且误差有界
        let img = make_frame(32, 24, 0xBADBEEF);
        let payload = encode_intra_transform_payload(&img, CompressionType::GolombRice, 5, 0, 5, 0)
            .expect("encode failed");

        let decoded = crate::crf::decoder::intra_transform::decode_intra_transform(
            &payload,
            img.width as usize,
            img.height as usize,
            3,
            5,
            5,
        )
        .expect("decode failed");

        let max_err = decoded
            .iter()
            .zip(&img.pixels)
            .map(|(a, b)| (a - b).unsigned_abs())
            .max()
            .unwrap_or(0);
        // q_step=5 → 最大误差 ≤ 5*64=320 理论上限，实际远小于
        assert!(max_err < 100, "q=5 往返 max_err={} 应 <100", max_err);
    }

    #[test]
    fn frame_type8_rct_domain_roundtrip() {
        // 复现路径 G 场景：RCT 域差分值（含负值，Co/Cg 范围 -256~255）
        // PNG1000 帧类型分布显示 type8 在帧7/12/13 胜出且像素不一致，
        // 首个差异固定在索引 419379（块 528,136）——疑似编解码不对称。
        let w = 32u16;
        let h = 32u16;
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        let mut s = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..(w as usize * h as usize) {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let y = ((s >> 33) as i32 % 256) - 128;
            let co = ((s >> 40) as i32 % 512) - 256;
            let cg = ((s >> 48) as i32 % 400) - 200;
            px.push(y);
            px.push(co);
            px.push(cg);
        }
        let img = ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: px,
        };
        let payload = encode_intra_transform_payload(
            &img,
            CompressionType::GolombRice,
            1,
            0,
            1,
            0,
        )
        .expect("encode failed");
        let decoded = crate::crf::decoder::intra_transform::decode_intra_transform(
            &payload,
            w as usize,
            h as usize,
            3,
            1,
            1,
        )
        .expect("decode failed");
        let max_err = decoded
            .iter()
            .zip(&img.pixels)
            .map(|(a, b)| (a - b).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert_eq!(max_err, 0, "RCT 域无损往返应有零误差，实际 max_err={}", max_err);
    }
}
