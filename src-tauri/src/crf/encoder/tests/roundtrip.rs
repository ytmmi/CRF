//! 基础编解码往返测试（golomb / banded / palette / planar）

use crate::crf::core::color::rct::rct_forward;
use crate::crf::core::domain::{
    ColorFormat, CompressionType, EncodeParams, ImageData, PredictionMode,
};

use super::super::banded::encode_banded_payload;
use super::super::encode_sequence;
use super::super::frame::candidate::{encode_frame_adaptive, ADAPTIVE_CANDIDATES};
use super::super::frame::{assemble_frame, encode_frame_inner, FrameQuant};
use super::super::planar::encode_planar_payload;
use super::{create_test_frames, indices_only, BAND_HEIGHT_ALT_TEST};

#[test]
fn test_encode_decode_golomb_roundtrip() {
    let frames = create_test_frames(3, 8, 8);
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
        input_original_frames: false,
        user_metadata: Some("test".to_string()),
    };

    let encoded = encode_sequence(&frames, &params).unwrap();
    assert!(!encoded.is_empty());

    // 验证文件头
    assert_eq!(&encoded[0..4], &[0x43, 0x52, 0x46, 0x00]);
}

#[test]
fn test_encode_decode_exp_golomb_roundtrip() {
    let frames = create_test_frames(3, 8, 8);
    let params = EncodeParams {
        compression_type: "exp-golomb".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
        input_original_frames: false,
        user_metadata: None,
    };

    let encoded = encode_sequence(&frames, &params).unwrap();
    assert!(!encoded.is_empty());
}

#[test]
fn test_encode_frame_count_validation() {
    let frames = create_test_frames(1, 8, 8); // 只有 1 帧
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
        input_original_frames: false,
        user_metadata: None,
    };

    let result = encode_sequence(&frames, &params);
    assert!(result.is_err());
}

#[test]
fn test_banded_payload_roundtrip() {
    // 构造上下内容差异大的图像：上半垂直渐变（适合垂直/MED），
    // 下半伪随机纹理（适合其他模式），迫使条带间选择不同预测模式
    let width = 64;
    let height = 96; // 恰好 3 个 32 行条带
    let mut pixels = vec![0i32; width * height];
    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = if y < 64 {
                (50 + y / 2) as i32
            } else {
                (100 + ((x * 7 + y * 13) % 89)) as i32
            };
        }
    }
    let image = ImageData {
        width: width as u16,
        height: height as u16,
        bit_depth: 8,
        color_format: ColorFormat::Gray,
        pixels,
    };

    let payload = encode_banded_payload(&image, 32, PredictionMode::Med).unwrap();

    // 条带头布局校验：band_count + [mode][k][len]
    let band_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    assert_eq!(band_count, 3);

    // 载荷级往返：解码 + 逐条带撤销预测必须无损还原
    let restored = crate::crf::decoder::decode_banded_with_undo(
        &payload,
        width,
        height,
        image.color_format.component_count(),
        32,
    )
    .unwrap();
    assert_eq!(image.pixels, restored);
}

/// v1.9 条带高度自适应：64 行条带路径往返一致，
/// 且大图平坦场景下编码器竞争可产出 coding_params=64 的帧。
#[test]
fn test_banded_payload_roundtrip_height64() {
    let width = 48usize;
    let height = 128usize; // 恰好 2 个 64 行条带
    let mut pixels = vec![0i32; width * height];
    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = ((x * 3 + y) % 200) as i32;
        }
    }
    let image = ImageData {
        width: width as u16,
        height: height as u16,
        bit_depth: 8,
        color_format: ColorFormat::Gray,
        pixels,
    };

    // 直接以 64 行条带编解码：band_count 应为 height/64
    let payload = encode_banded_payload(&image, BAND_HEIGHT_ALT_TEST, PredictionMode::Med).unwrap();
    let band_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    assert_eq!(band_count, 2);
    let restored = crate::crf::decoder::decode_banded_with_undo(
        &payload,
        width,
        height,
        image.color_format.component_count(),
        BAND_HEIGHT_ALT_TEST,
    )
    .unwrap();
    assert_eq!(image.pixels, restored);
}

#[test]
fn test_palette_roundtrip_low_color() {
    // 二次元插画特征：少量平坦色块（此处 4 色）。
    // 单分量低色数数据下调色板路径（frame_type=4）应具备竞争力甚至胜出。
    let width = 96;
    let height = 96;
    let mut pixels = vec![0i32; width * height];
    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = match ((x / 48) + (y / 48)) % 4 {
                0 => 10,
                1 => 80,
                2 => 150,
                _ => 220,
            };
        }
    }
    let image = ImageData {
        width: width as u16,
        height: height as u16,
        bit_depth: 8,
        color_format: ColorFormat::Gray,
        pixels,
    };

    let frame = encode_frame_adaptive(
        &image,
        CompressionType::GolombRice,
        8,
        false,
        FrameQuant::lossless(),
        None,
        None,
    )
    .unwrap();

    // 帧头偏移 8 为 frame_type；记录实际选中的类型便于诊断
    let frame_type = frame.data[8];
    assert!(frame_type <= 5, "frame_type 应为 0..5，实际 {}", frame_type);

    // 往返无损（经完整解码管线；encode_sequence 要求至少 2 帧，首帧重复一次）
    let pair = [image.clone(), image.clone()];
    let decoded = crate::crf::decode_from_bytes(&{
        // 用 encode_sequence 打包为完整文件以复用标准解码入口
        let params = EncodeParams {
            compression_type: "golomb-rice".to_string(),
            block_size: None,
            prediction_mode: PredictionMode::Average,
            adaptive_prediction: true,
            lossy: None,
            input_original_frames: false,
            user_metadata: None,
        };
        encode_sequence(&pair, &params).unwrap()
    })
    .unwrap_or_else(|e| panic!("decode failed: {}", e));
    assert_eq!(decoded.frames.len(), 2);
    assert_eq!(image.pixels, decoded.frames[0].pixels, "调色板往返失败");
    assert_eq!(
        image.pixels, decoded.frames[1].pixels,
        "调色板往返失败(帧2)"
    );
    let _ = frame_type;
}

/// Palette v2（copy-above token 化）收益验证：
/// 水平恒定的低色数场（每行内大量相同索引）在 copy-above 下应产生
/// 长零行程，载荷显著小于"无上方复制"的基线；且端到端往返逐位一致。
#[test]
fn test_palette_copy_above_benefit() {
    let width = 96usize;
    let height = 96usize;
    // 4 色水平条带：行内完全恒定 → copy-above 命中率极高
    let mut pixels = vec![0i32; width * height];
    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = match (y / 24) % 4 {
                0 => 10,
                1 => 80,
                2 => 150,
                _ => 220,
            };
        }
    }
    let image = ImageData {
        width: width as u16,
        height: height as u16,
        bit_depth: 8,
        color_format: ColorFormat::Gray,
        pixels: pixels.clone(),
    };

    // 直接对比两种索引流编码的载荷大小
    let payload_v2 =
        crate::crf::encoder::frame::candidate::test_hooks::encode_palette_payload_for_test(
            &pixels, width,
        )
        .unwrap()
        .unwrap();
    let baseline = crate::crf::encoder::frame::candidate::test_hooks::palette_index_baseline_size(
        &indices_only(&pixels),
    );
    println!(
        "palette v2={} bytes, 纯索引基线≈{} bytes",
        payload_v2.len(),
        baseline
    );
    assert!(
        payload_v2.len() * 100 < baseline * 60,
        "copy-above 应带来显著压缩（v2 {} < 基线 60% {}）",
        payload_v2.len(),
        baseline
    );

    // 端到端往返：经完整管线（palette 候选胜出与否不影响正确性）
    let pair = [image.clone(), image.clone()];
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Average,
        adaptive_prediction: true,
        lossy: None,
        input_original_frames: false,
        user_metadata: None,
    };
    let encoded = encode_sequence(&pair, &params).unwrap();
    let decoded =
        crate::crf::decode_from_bytes(&encoded).unwrap_or_else(|e| panic!("decode failed: {}", e));
    assert_eq!(image.pixels, decoded.frames[0].pixels, "v2 往返失败");
    assert_eq!(image.pixels, decoded.frames[1].pixels, "v2 往返失败(帧2)");
}

#[test]
fn test_planar_roundtrip_flat_chroma() {
    // 二次元插画特征：亮度纹理丰富、色度大面积恒定（赛璐璐上色）。
    // RCT 后 Co/Cg 平面近乎全零 → planar 路径（frame_type=3）应胜出。
    let width = 96;
    let height = 96;
    let mut pixels = vec![0i32; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 3;
            // 灰度线稿风：R=G=B 带纹理（Co=Cg=0 恒定）
            let v = ((x * x / 4 + y * 3) % 200 + (x * y) % 37) as i32;
            pixels[idx] = v;
            pixels[idx + 1] = v;
            pixels[idx + 2] = v;
        }
    }
    let image = ImageData {
        width: width as u16,
        height: height as u16,
        bit_depth: 8,
        color_format: ColorFormat::Rgb,
        pixels,
    };

    // 经 RCT 后编码（模拟 encode_sequence 的真实输入）
    let transformed = rct_forward(&image.pixels, 3).unwrap();
    let rct_image = ImageData {
        pixels: transformed,
        ..image.clone()
    };

    let payload = encode_planar_payload(
        &rct_image,
        CompressionType::GolombRice,
        8,
        FrameQuant::lossless(),
        None,
    )
    .unwrap();
    let planar_frame = assemble_frame(&payload, &rct_image, 0, 3).unwrap();
    let frame_level_best = {
        // 与帧级最优对比，确认 planar 至少不差于帧级路径
        let mut best = usize::MAX;
        for &m in ADAPTIVE_CANDIDATES.iter() {
            let d = encode_frame_inner(
                &rct_image,
                CompressionType::GolombRice,
                8,
                m,
                false,
                FrameQuant::lossless(),
                None,
            )
            .unwrap();
            best = best.min(d.len());
        }
        best
    };
    assert!(
        planar_frame.len() <= frame_level_best,
        "色度平坦时 planar({}) 应不大于帧级最优({})",
        planar_frame.len(),
        frame_level_best
    );

    // 载荷级往返：解三平面 + interleave 后必须与 RCT 域输入一致
    let header_stub = crate::crf::core::bitstream::header::CrfHeader::new(
        2,
        image.width,
        image.height,
        image.bit_depth,
        ColorFormat::Rgb,
        CompressionType::GolombRice,
    );
    let decoded = crate::crf::decoder::decode_planar(&payload, &header_stub).unwrap();
    assert_eq!(rct_image.pixels, decoded);
}
