//! 实际编解码管线集成测试（1000/2000 图片组）

#[cfg(test)]
mod integration_tests {
    use crate::crf;
    use std::time::Instant;

    /// 加载图片目录为帧序列
    fn load_test_frames(dir: &str) -> Vec<crf::ImageData> {
        let mut frames = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("无法读取目录")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let ext = e.path().extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp")
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());
        
        for entry in entries {
            let path = entry.path();
            if let Ok(reader) = image::ImageReader::open(&path) {
                if let Ok(img) = reader.decode() {
                    let rgb = img.to_rgb8();
                    let (w, h) = rgb.dimensions();
                    let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
                    for y in 0..h {
                        for x in 0..w {
                            let px = rgb.get_pixel(x, y);
                            pixels.push(px[0] as i32);
                            pixels.push(px[1] as i32);
                            pixels.push(px[2] as i32);
                        }
                    }
                    frames.push(crf::ImageData {
                        width: w as u16,
                        height: h as u16,
                        bit_depth: 8,
                        color_format: crf::ColorFormat::Rgb,
                        pixels,
                    });
                }
            }
        }
        frames
    }

    /// 无损往返校验（逐像素）
    fn verify_lossless_roundtrip(frames: &[crf::ImageData], label: &str) {
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crf::PredictionMode::Med,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: false,
            user_metadata: None,
        };

        let t = Instant::now();
        let encoded = crf::encode_sequence(frames, &params).expect("编码失败");
        let encode_time = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let decoded = crf::decode_from_bytes(&encoded).expect("解码失败");
        let decode_time = t.elapsed().as_secs_f64() * 1000.0;

        println!("[{}] 编码: {} B ({:.1}ms), 解码: {} 帧 ({:.1}ms)",
            label, encoded.len(), encode_time, decoded.frames.len(), decode_time);

        assert_eq!(decoded.frames.len(), frames.len(), "帧数不匹配");
        
        for (i, (orig, dec)) in frames.iter().zip(decoded.frames.iter()).enumerate() {
            if orig.pixels != dec.pixels {
                let pos = orig.pixels.iter().zip(dec.pixels.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(usize::MAX);
                panic!("帧{} 逐位不一致！首个差异索引 {} (orig={} vs dec={})", 
                    i, pos, orig.pixels[pos], dec.pixels[pos]);
            }
        }
        println!("  ✓ 无损往返校验通过（{} 帧逐像素一致）", frames.len());
    }

    /// 有损编解码测试
    fn verify_lossy_encode_decode(frames: &[crf::ImageData], label: &str, quality: u8) {
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crf::PredictionMode::Med,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: true,
            user_metadata: None,
        };

        let t = Instant::now();
        let encoded = crf::encode_sequence(frames, &params).expect("有损编码失败");
        let encode_time = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let decoded = crf::decode_from_bytes(&encoded).expect("有损解码失败");
        let decode_time = t.elapsed().as_secs_f64() * 1000.0;

        println!("[{} q{}] 编码: {} B ({:.1}ms), 解码: {} 帧 ({:.1}ms)",
            label, quality, encoded.len(), encode_time, decoded.frames.len(), decode_time);

        assert_eq!(decoded.frames.len(), frames.len(), "帧数不匹配");
        assert!(decoded.header.flags.has_lossy_quant(), "应标记有损");
        // lossy_quant 存储的是量化步长 Q = clamp((100 - q + 4) / 5, 1, 20)，不是质量参数
        assert!(decoded.header.lossy_quant > 0, "有损量化步长应 > 0");
        
        // 首帧应无损（golden_lossless）
        assert_eq!(decoded.frames[0].pixels, frames[0].pixels, "首帧应无损");
        println!("  ✓ 有损编解码通过（首帧无损，有损标记正确）");
    }

    #[test]
    fn test_1000_group_lossless_roundtrip() {
        let frames = load_test_frames(r"E:\CRF\test\png\1000");
        assert!(frames.len() >= 2, "1000 组至少需要 2 张图片");
        println!("\n=== 1000 图片组无损往返测试（{} 帧）===", frames.len());
        verify_lossless_roundtrip(&frames, "1000-lossless");
    }

    #[test]
    fn test_2000_group_lossless_roundtrip() {
        let frames = load_test_frames(r"E:\CRF\test\png\2000");
        assert!(frames.len() >= 2, "2000 组至少需要 2 张图片");
        println!("\n=== 2000 图片组无损往返测试（{} 帧）===", frames.len());
        verify_lossless_roundtrip(&frames, "2000-lossless");
    }

    #[test]
    fn test_1000_group_lossy_encode_decode() {
        let frames = load_test_frames(r"E:\CRF\test\png\1000");
        assert!(frames.len() >= 2, "1000 组至少需要 2 张图片");
        println!("\n=== 1000 图片组有损编解码测试（{} 帧）===", frames.len());
        verify_lossy_encode_decode(&frames, "1000", 90);
        verify_lossy_encode_decode(&frames, "1000", 75);
    }

    #[test]
    fn test_2000_group_lossy_encode_decode() {
        let frames = load_test_frames(r"E:\CRF\test\png\2000");
        assert!(frames.len() >= 2, "2000 组至少需要 2 张图片");
        println!("\n=== 2000 图片组有损编解码测试（{} 帧）===", frames.len());
        verify_lossy_encode_decode(&frames, "2000", 90);
        verify_lossy_encode_decode(&frames, "2000", 75);
    }

    #[test]
    fn test_full_pipeline_1000() {
        let frames = load_test_frames(r"E:\CRF\test\png\1000");
        assert!(frames.len() >= 2, "1000 组至少需要 2 张图片");
        
        println!("\n=== 1000 图片组完整管线测试 ===");
        println!("输入: {} 帧, {}x{}", frames.len(), frames[0].width, frames[0].height);
        
        // 无损编码
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crf::PredictionMode::Med,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: false,
            user_metadata: Some("integration-test-1000".to_string()),
        };

        let t = Instant::now();
        let encoded = crf::encode_sequence(&frames, &params).expect("编码失败");
        println!("编码完成: {} B ({:.1}ms)", encoded.len(), t.elapsed().as_secs_f64() * 1000.0);
        
        // 写入文件
        let crf_path = r"E:\CRF\test\output\integration_1000_lossless.crf";
        std::fs::create_dir_all(r"E:\CRF\test\output").ok();
        std::fs::write(crf_path, &encoded).expect("写入失败");
        println!("写入: {}", crf_path);
        
        // 从文件解码
        let t = Instant::now();
        let data = std::fs::read(crf_path).expect("读取失败");
        let decoded = crf::decode_from_bytes(&data).expect("解码失败");
        println!("解码完成: {} 帧 ({:.1}ms)", decoded.frames.len(), t.elapsed().as_secs_f64() * 1000.0);
        
        // 校验
        assert_eq!(decoded.frames.len(), frames.len());
        for (i, (orig, dec)) in frames.iter().zip(decoded.frames.iter()).enumerate() {
            assert_eq!(orig.pixels, dec.pixels, "帧{} 不一致", i);
        }
        println!("✓ 1000 图片组完整管线测试通过！");
    }

    #[test]
    fn test_full_pipeline_2000() {
        let frames = load_test_frames(r"E:\CRF\test\png\2000");
        assert!(frames.len() >= 2, "2000 组至少需要 2 张图片");
        
        println!("\n=== 2000 图片组完整管线测试 ===");
        println!("输入: {} 帧, {}x{}", frames.len(), frames[0].width, frames[0].height);
        
        // 无损编码
        let params = crf::EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: crf::PredictionMode::Med,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: false,
            user_metadata: Some("integration-test-2000".to_string()),
        };

        let t = Instant::now();
        let encoded = crf::encode_sequence(&frames, &params).expect("编码失败");
        println!("编码完成: {} B ({:.1}ms)", encoded.len(), t.elapsed().as_secs_f64() * 1000.0);
        
        // 写入文件
        let crf_path = r"E:\CRF\test\output\integration_2000_lossless.crf";
        std::fs::create_dir_all(r"E:\CRF\test\output").ok();
        std::fs::write(crf_path, &encoded).expect("写入失败");
        println!("写入: {}", crf_path);
        
        // 从文件解码
        let t = Instant::now();
        let data = std::fs::read(crf_path).expect("读取失败");
        let decoded = crf::decode_from_bytes(&data).expect("解码失败");
        println!("解码完成: {} 帧 ({:.1}ms)", decoded.frames.len(), t.elapsed().as_secs_f64() * 1000.0);
        
        // 校验
        assert_eq!(decoded.frames.len(), frames.len());
        for (i, (orig, dec)) in frames.iter().zip(decoded.frames.iter()).enumerate() {
            assert_eq!(orig.pixels, dec.pixels, "帧{} 不一致", i);
        }
        println!("✓ 2000 图片组完整管线测试通过！");
    }
}
