//! 诊断测试：比较 streaming 和 batch 在 previous 模式下的逐帧输出

#[cfg(test)]
mod debug_tests {
    use crate::crf::encoder::streaming::StreamingEncoder;
    use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};

    fn make_frame(shift: i32, width: u16, height: u16) -> ImageData {
        let mut px = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            for x in 0..width {
                px.push((((x as i32 + shift * 3) % 200) + 30));
                px.push((100 - ((y as i32 + shift) % 70)) + 20);
                px.push(((x as i32 + y as i32 + shift) % 150) + 50);
            }
        }
        ImageData {
            width,
            height,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: px,
        }
    }

    fn mk_lossy_params_q90() -> EncodeParams {
        EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: PredictionMode::Average,
            adaptive_prediction: true,
            lossy: Some(
                crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
                    .build()
                    .unwrap(),
            ),
            input_original_frames: true,
            user_metadata: None,
        }
    }

    #[test]
    fn debug_previous_mode() {
        let w = 32u16;
        let h = 24u16;
        let frames: Vec<ImageData> = (0..5).map(|i| make_frame(i * 2, w, h)).collect();
        let mut params = mk_lossy_params_q90();
        params.lossy = Some(
            crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
                .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Previous)
                .build()
                .unwrap(),
        );

        let streaming = {
            let mut enc = StreamingEncoder::new(&params).unwrap();
            for f in &frames {
                enc.push_frame(f).unwrap();
            }
            enc.finish().unwrap()
        };
        let batched = crate::crf::encode_sequence(&frames, &params).unwrap();

        println!("=== 文件大小 ===");
        println!("streaming: {} bytes", streaming.len());
        println!("batched:   {} bytes", batched.len());

        // 比较头部 (64 字节)
        println!("\n=== 文件头 (前 64 字节) ===");
        for i in 0..64.min(streaming.len()).min(batched.len()) {
            if streaming[i] != batched[i] {
                println!("  差异 @{}: streaming={:02x} batched={:02x}", i, streaming[i], batched[i]);
            }
        }

        // 比较帧索引
        let header_size = 64;
        let frame_count = 5usize;
        let index_size = frame_count * 8;
        println!("\n=== 帧索引 ===");
        for i in 0..index_size {
            let si = header_size + i;
            let bi = header_size + i;
            if si < streaming.len() && bi < batched.len() {
                if streaming[si] != batched[bi] {
                    println!("  差异 @{}: streaming={:02x} batched={:02x}", i, streaming[si], batched[bi]);
                }
            }
        }

        // 解析帧偏移和大小
        println!("\n=== 帧布局 ===");
        for i in 0..frame_count {
            let offset = header_size + i * 8;
            if offset + 8 <= streaming.len() && offset + 8 <= batched.len() {
                let s_off = u32::from_le_bytes([streaming[offset], streaming[offset+1], streaming[offset+2], streaming[offset+3]]);
                let s_size = u32::from_le_bytes([streaming[offset+4], streaming[offset+5], streaming[offset+6], streaming[offset+7]]);
                let b_off = u32::from_le_bytes([batched[offset], batched[offset+1], batched[offset+2], batched[offset+3]]);
                let b_size = u32::from_le_bytes([batched[offset+4], batched[offset+5], batched[offset+6], batched[offset+7]]);
                println!("  帧 {}: streaming offset={} size={} | batched offset={} size={}", i, s_off, s_size, b_off, b_size);
            }
        }

        // 找到首个差异
        let mut first_diff = None;
        for i in 0..streaming.len().min(batched.len()) {
            if streaming[i] != batched[i] {
                first_diff = Some(i);
                break;
            }
        }
        if let Some(pos) = first_diff {
            let start = pos.saturating_sub(16);
            let end = (pos + 32).min(streaming.len()).min(batched.len());
            println!("\n=== 首个差异 @{} ===", pos);
            println!("streaming[{}..{}]: {:02x?}", start, end, &streaming[start..end]);
            println!("batched[{}..{}]:   {:02x?}", start, end, &batched[start..end]);
        } else {
            println!("\n=== 无差异! ===");
        }
    }
}
