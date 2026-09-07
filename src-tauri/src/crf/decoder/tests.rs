//! decoder 集成测试（自原 mod.rs 迁移）

use crate::crf::core::domain::{ColorFormat, ImageData};
use crate::crf::decoder::decode_from_bytes;
use crate::crf::encoder;

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
    let params = crate::crf::core::domain::EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: crate::crf::core::domain::PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
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
    let params = crate::crf::core::domain::EncodeParams {
        compression_type: "exp-golomb".to_string(),
        block_size: None,
        prediction_mode: crate::crf::core::domain::PredictionMode::None,
        adaptive_prediction: false,
        lossy: None,
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
    use crate::crf::core::bitstream::header::CrfHeader;
    use crate::crf::core::domain::{CompressionType, PredictionMode};

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
        &crate::crf::core::domain::ImageData {
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

    let decoded = super::reconstruct::reconstruct_frame(&frame_buf, &header).unwrap();
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

    let params = crate::crf::core::domain::EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: crate::crf::core::domain::PredictionMode::Average,
        adaptive_prediction: true,
        lossy: None,
        input_original_frames: true,
        user_metadata: None,
    };

    let encoded = encoder::encode_sequence(&frames, &params).unwrap();
    let result = decode_from_bytes(&encoded).unwrap();
    assert_eq!(result.frames.len(), frames.len());

    // golden 链还原（生产恢复逻辑收敛于 DecodeSession::restore_temporal，规划 §8.2）
    let restored_all = crate::crf::decoder::session::DecodeSession::restore_temporal(&result);
    let mut max_err_all: i32 = 0;
    for (i, (orig, restored)) in frames.iter().zip(restored_all.iter()).enumerate() {
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
    }
    assert!(
        max_err_all <= 24,
        "q75 序列还原帧误差超界: max_err={}",
        max_err_all
    );
}

/// 缺陷回归（文件级）：含 frame_type=8 有损帧的完整 CRF 文件端到端往返。
///
/// 历史缺陷（optimization-review §24/§25）：有损档 frame_type=8 只有载荷级
/// 测试，缺端到端 CRF 文件级往返；且解码端曾以 `(q_step, q_step)` 反量化，
/// chroma_step != q_step（chroma_scale>1000 / 显式 chroma_step）时色度步长
/// 错误。修复后 type8 载荷自包含 luma/chroma 步长。
///
/// 本测试手动组装 2 帧文件：
/// - frame0 = type8 无损 golden（luma=1, chroma=1），RGB 直通（has_rct=false）；
/// - frame1 = type8 有损差分（luma=2, chroma=3）；
/// 走 decode_from_bytes → restore_temporal 全链路（文件头/索引/帧头/CRC/分派）。
#[test]
fn test_lossy_frame_type8_file_roundtrip() {
    use crate::crf::checksum::crc32;
    use crate::crf::core::bitstream::constants::{FOOTER_MAGIC, FRAME_HEADER_SIZE, HEADER_SIZE};
    use crate::crf::core::bitstream::header::CrfHeader;
    use crate::crf::core::domain::{CompressionType, Flags, PredictionMode};
    use crate::crf::encoder::intra_transform::encode_intra_transform_payload;

    let w = 32u16;
    let h = 24u16;
    let make_img = |shift: i32| -> ImageData {
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                px.push((((x as i32 * 5 + y as i32 * 3 + shift) % 200) + 28) as i32);
                px.push(((x as i32 * 7 - y as i32 * 2 + shift).rem_euclid(180) + 40) as i32);
                px.push(((x as i32 * 3 + y as i32 * 11) % 220 + 20) as i32);
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
    let img0 = make_img(0);
    let img1 = make_img(17);

    // frame0：type8 无损（q=1），RGB 直通域
    let frame0_payload =
        encode_intra_transform_payload(&img0, CompressionType::GolombRice, 1, 0, 1, 0)
            .expect("frame0 encode failed");
    let frame0_buf = crate::crf::encoder::assemble_frame(&frame0_payload, &img0, 0, 8).unwrap();

    // frame1：type8 有损差分（luma=2, chroma=3）；golden 参考位 bit7=1
    let diff: Vec<i32> = img1
        .pixels
        .iter()
        .zip(&img0.pixels)
        .map(|(a, b)| a - b)
        .collect();
    let diff_img = ImageData {
        width: w,
        height: h,
        bit_depth: 8,
        color_format: ColorFormat::Rgb,
        pixels: diff,
    };
    let frame1_payload =
        encode_intra_transform_payload(&diff_img, CompressionType::GolombRice, 2, 0, 3, 0)
            .expect("frame1 encode failed");
    let frame1_buf = crate::crf::encoder::assemble_frame(&frame1_payload, &diff_img, 0, 8).unwrap();

    // 文件头：2 帧、RGB、GolombRice、无 RCT（RGB 直通）、含帧索引。
    // v1.15 golden 参考由帧头 reference_type=0 表达（assemble_frame 默认），
    // coding_params 不再承载 golden 位。
    let mut header = CrfHeader::new(2, w, h, 8, ColorFormat::Rgb, CompressionType::GolombRice);
    header.flags.set_has_index(true);
    header.flags.set_has_rct(false);
    header.lossy_quant = 2; // 全局亮度步长（frame1 有损）

    // 组装文件：header + index(2×8B) + frames + CRC32 + footer
    let frames_start = HEADER_SIZE + 2 * 8;
    let mut out = Vec::new();
    header.write_bytes(&mut out).unwrap();
    out.extend_from_slice(&(frames_start as u32).to_le_bytes());
    out.extend_from_slice(&(frame0_buf.len() as u32).to_le_bytes());
    out.extend_from_slice(&((frames_start + frame0_buf.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(frame1_buf.len() as u32).to_le_bytes());
    out.extend_from_slice(&frame0_buf);
    out.extend_from_slice(&frame1_buf);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&FOOTER_MAGIC);

    // 端到端文件解码 + 时间维还原
    let result = decode_from_bytes(&out).expect("file decode failed");
    assert_eq!(result.frames.len(), 2);
    let restored = crate::crf::decoder::session::DecodeSession::restore_temporal(&result);

    // frame0：golden 无损基准，必须逐位一致
    assert_eq!(
        restored[0].pixels, img0.pixels,
        "frame0 无损 golden 必须逐位一致"
    );

    // frame1：golden 差分还原（luma=2 / chroma=3），误差受控
    let max_err = restored[1]
        .pixels
        .iter()
        .zip(&img1.pixels)
        .map(|(a, b)| (a - b).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        max_err < 100,
        "frame1 type8 有损还原 max_err={} 应 <100",
        max_err
    );

    // 载荷级复核：frame1 确实携带步长信令（chroma=3 != luma=2）
    assert_eq!(frame1_payload[0] & 0b110, 0b110, "type8 载荷必须自包含步长");
    assert_eq!(frame1_payload[1], 2, "luma_step 应为 2");
    assert_eq!(frame1_payload[2], 3, "chroma_step 应为 3");
}
