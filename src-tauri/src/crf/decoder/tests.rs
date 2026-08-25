//! decoder 集成测试（自原 mod.rs 迁移）

use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder;
use crate::crf::format::{ColorFormat, ImageData};

fn create_test_frames(count: usize, width: u16, height: u16) -> Vec<ImageData> {
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

#[test]
fn test_decode_golomb_roundtrip() {
    let frames = create_test_frames(3, 8, 8);
    let params = crate::crf::format::EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: crate::crf::format::PredictionMode::None,
        adaptive_prediction: false,
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: false,
        user_metadata: None,
    };

    let encoded = encoder::encode_sequence(&frames, &params).unwrap();
    let result = decode_from_bytes(&encoded).unwrap();

    assert_eq!(result.frames.len(), frames.len());
    for (original, decoded) in frames.iter().zip(result.frames.iter()) {
        assert_eq!(original.pixels, decoded.pixels);
    }
}

#[test]
fn test_decode_exp_golomb_roundtrip() {
    let frames = create_test_frames(3, 8, 8);
    let params = crate::crf::format::EncodeParams {
        compression_type: "exp-golomb".to_string(),
        block_size: None,
        prediction_mode: crate::crf::format::PredictionMode::None,
        adaptive_prediction: false,
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: false,
        user_metadata: None,
    };

    let encoded = encoder::encode_sequence(&frames, &params).unwrap();
    let result = decode_from_bytes(&encoded).unwrap();

    assert_eq!(result.frames.len(), frames.len());
    for (original, decoded) in frames.iter().zip(result.frames.iter()) {
        assert_eq!(original.pixels, decoded.pixels);
    }
}

/// frame_type=6（DCT 变换域路径）编解码对称性回归。
///
/// 历史缺陷：①编码端把三分量交织数据当单平面做 DCT（仅前 1/3 样本
/// 有效，色度整体湮灭）；②解码端码流切片偏移错误（丢弃首字节）；
/// ③解码端对无预测语义的本路径多余执行 undo_prediction。
/// 任一缺陷存在时本测试必然失败。
#[test]
fn test_lossy_dct_frame6_symmetry() {
    use crate::crf::format::{CompressionType, CrfHeader, PredictionMode};

    let w = 32usize;
    let h = 32usize;
    // 平滑渐变三分量数据：DCT 高频归零，保证该候选具备竞争力
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            px.push(((x * 4 + y * 2) % 256) as i32);
            px.push(80i32 - x as i32);
            px.push(y as i32 - 60);
        }
    }

    let q_step = 5u8;
    let quant = crate::crf::encoder::dct_path::dct_quantize_interleaved(&px, w, h, 3, q_step);
    // 载荷 v3：[k u8][flags][(ma_tree 头)][cabac 码流]
    let (payload, k) =
        crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive(&quant, None).unwrap();
    let mut full_payload = Vec::with_capacity(payload.len() + 1);
    full_payload.push(k);
    full_payload.extend_from_slice(&payload);
    let frame_buf = crate::crf::encoder::assemble_frame(
        &full_payload,
        &crate::crf::format::ImageData {
            width: w as u16,
            height: h as u16,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: px.clone(),
        },
        k,
        6,
    )
    .unwrap();

    // 文件头全局模式设为 Average：历史缺陷下 PRED_MODE_UNSET 会回退到它
    let mut header = CrfHeader::new(
        2,
        w as u16,
        h as u16,
        8,
        ColorFormat::Rgb,
        CompressionType::GolombRice,
    );
    header.prediction_mode = PredictionMode::Average;

    let decoded = super::decode_frame(&frame_buf, &header).unwrap();
    assert_eq!(decoded.pixels.len(), px.len());

    let mut max_err: i32 = 0;
    let mut sq_sum = 0.0f64;
    for (a, b) in px.iter().zip(decoded.pixels.iter()) {
        let d = (a - b).abs();
        max_err = max_err.max(d);
        sq_sum += (d * d) as f64;
    }
    let mse = sq_sum / px.len() as f64;
    let psnr = if mse > 0.0 {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    } else {
        f64::INFINITY
    };
    assert!(
        psnr > 30.0 && max_err <= 16,
        "frame_type=6 Q={} 对称重建劣化: max_err={} PSNR={:.2}dB",
        q_step,
        max_err,
        psnr
    );
}

/// 有损 golden 序列端到端：RGB + 自适应 + q75 + 原始帧输入。
///
/// 契约：①首帧为 golden 无损基准，逐位一致；
/// ②其余帧闭环量化误差受控（不随档位出现毁灭性失真）。
#[test]
fn test_lossy_golden_sequence_roundtrip() {
    let w = 16u16;
    let h = 16u16;
    // 4 帧：首帧基准 + 带位移的差分内容（保证残差非退化）
    let make_frame = |shift: i32| -> ImageData {
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                px.push((((x as i32 + shift * 3) % 200) + 40));
                px.push(120 - ((y as i32 + shift) % 80));
                px.push(((x as i32 * 2 + y as i32 + shift * 5) % 180) + 30);
            }
        }
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: px,
        }
    };
    let frames: Vec<ImageData> = (0..4).map(make_frame).collect();

    let params = crate::crf::format::EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: crate::crf::format::PredictionMode::Average,
        adaptive_prediction: true,
        lossy_quality: Some(75),
        lossy_tuning: None,
        input_original_frames: true,
        user_metadata: None,
    };

    let encoded = encoder::encode_sequence(&frames, &params).unwrap();
    let result = decode_from_bytes(&encoded).unwrap();
    assert_eq!(result.frames.len(), frames.len());

    // golden 链还原（与集成测试 verify_crf_against_pngs 同逻辑）
    let golden_refs = result.frame_golden_refs.clone();
    let mut restored_prev: Option<ImageData> = None;
    let mut max_err_all: i32 = 0;
    for (i, (orig, dec)) in frames.iter().zip(result.frames.iter()).enumerate() {
        let is_golden = golden_refs.get(i).copied().unwrap_or(false);
        let restored = match (&restored_prev, is_golden) {
            (None, _) => dec.clone(),
            (Some(_), true) => {
                // golden 帧：以首帧（无损基准）相加还原
                let first = &result.frames[0];
                ImageData {
                    width: dec.width,
                    height: dec.height,
                    bit_depth: dec.bit_depth,
                    color_format: dec.color_format,
                    pixels: first
                        .pixels
                        .iter()
                        .zip(dec.pixels.iter())
                        .map(|(a, b)| a + b)
                        .collect(),
                }
            }
            (Some(prev), false) => ImageData {
                width: dec.width,
                height: dec.height,
                bit_depth: dec.bit_depth,
                color_format: dec.color_format,
                pixels: prev
                    .pixels
                    .iter()
                    .zip(dec.pixels.iter())
                    .map(|(a, b)| a + b)
                    .collect(),
            },
        };
        if i == 0 {
            // 契约①：首帧逐位一致（golden 基准强制无损）
            assert_eq!(
                orig.pixels, restored.pixels,
                "首帧为 golden 无损基准，必须逐位一致"
            );
        } else {
            // 契约②：q75(Q=5) 闭环误差受控
            let frame_max = orig
                .pixels
                .iter()
                .zip(restored.pixels.iter())
                .map(|(a, b)| (a - b).abs())
                .max()
                .unwrap_or(0);
            max_err_all = max_err_all.max(frame_max);
        }
        restored_prev = Some(restored);
    }
    assert!(
        max_err_all <= 24,
        "q75 序列还原帧误差超界: max_err={}",
        max_err_all
    );
}
