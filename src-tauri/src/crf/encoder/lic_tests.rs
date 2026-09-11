//! LIC（局部照明补偿）端到端测试：信令往返、光照渐变收益、A/B 开关。
//!
//! 覆盖：
//! - FrameHeader LIC 字段字节往返（from_bytes ∘ write_bytes 恒等）
//! - 光照渐变序列端到端：编码端自动启用 LIC，解码还原逐位一致
//! - CRF_DISABLE_LIC=1 时 LIC 竞争完全关闭（无 LIC 帧、字节不劣化）
//! - batch 与 streaming 在 LIC 决策上逐字节一致

use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::domain::{ColorFormat, EncodeParams, FrameHeader, ImageData, PredictionMode};
use crate::crf::decoder::session::DecodeSession;
use crate::crf::encoder::sequence::encode_sequence;
use crate::crf::encoder::streaming::StreamingEncoder;

/// 构建光照渐变差分序列：frame_k = golden × a_k + b_k（三分量**一致的**整帧
/// 乘加——模拟真实场景变暗/提亮，LIC 全局 (a,b) 模型前提）+ 少量局部内容。
fn make_shading_frames(count: usize, w: u16, h: u16) -> Vec<ImageData> {
    let golden: Vec<i32> = {
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                let g: i32 = (x as i32 * 3 + y as i32 * 7) % 180 + 30;
                px.push(g); // R
                px.push(140 - (y as i32 % 90)); // G
                px.push((x as i32 % 200) + 20); // B
            }
        }
        px
    };
    (0..count)
        .map(|k| {
            // 每帧不同的系统性光照（a 在 0.92..0.98 渐变、b 逐步 +3）+
            // 少量局部内容变化（确认 LIC 不遮挡真实差分）
            let a_num = 92 + (k as i32 % 7); // 0.92..=0.98
            let b = k as i32 * 3;
            let px = golden
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let base = ((v * a_num) / 100) + b;
                    // 局部内容：每 13 个像素插入一个真实差分（非照明）
                    if i % 13 == 7 {
                        base + 5
                    } else {
                        base
                    }
                })
                .collect();
            ImageData {
                width: w,
                height: h,
                bit_depth: 8,
                color_format: ColorFormat::Rgb,
                pixels: px,
            }
        })
        .collect()
}

fn mk_params() -> EncodeParams {
    EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Average,
        adaptive_prediction: true,
        lossy: None,
        input_original_frames: true,
        user_metadata: None,
    }
}

/// 解析每帧 LIC 字段（lic_a_num, lic_b）与 reference_type。
fn frame_lic_fields(data: &[u8], frame_count: usize) -> Vec<(u8, u8)> {
    use crate::crf::core::bitstream::constants::{HEADER_SIZE, LIC_A_NUM_OFFSET, LIC_B_OFFSET};
    let frames_start = HEADER_SIZE + frame_count * 8;
    let mut out = Vec::new();
    let mut off = frames_start;
    for _ in 0..frame_count {
        let fs = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        out.push((data[off + LIC_A_NUM_OFFSET], data[off + LIC_B_OFFSET]));
        off += FRAME_HEADER_SIZE + fs;
    }
    out
}

/// FrameHeader LIC 字段字节往返恒等。
#[test]
fn test_frame_header_lic_roundtrip() {
    let mut h = FrameHeader::with_type(1234, 5678, 3, 2);
    h.pred_mode = 5;
    h.reference_type = 0;
    h.lic_a_num = 96;
    h.lic_b = 40; // b=+40
    assert!(h.lic_enabled());
    assert_eq!(h.lic_a(), 96);
    assert_eq!(h.lic_b(), 40);

    let mut buf = Vec::new();
    h.write_bytes(&mut buf).unwrap();
    assert_eq!(buf.len(), FRAME_HEADER_SIZE);
    let back = FrameHeader::from_bytes(&buf).unwrap();
    assert_eq!(back.frame_size, 1234);
    assert_eq!(back.pixel_count, 5678);
    assert_eq!(back.frame_type, 2);
    assert_eq!(back.coding_params, 3);
    assert_eq!(back.pred_mode, 5);
    assert_eq!(back.reference_type, 0);
    assert_eq!(back.lic_a_num, 96);
    assert_eq!(back.lic_b, 40);

    // 负 b 的 i8 存储往返
    h.lic_b = (-24i8) as u8;
    let mut buf2 = Vec::new();
    h.write_bytes(&mut buf2).unwrap();
    let back2 = FrameHeader::from_bytes(&buf2).unwrap();
    assert_eq!(back2.lic_b(), -24);

    // 默认（未启用）
    let d = FrameHeader::with_type(1, 1, 0, 0);
    assert!(!d.lic_enabled());
    assert_eq!(d.lic_a(), 0);
    assert_eq!(d.lic_b(), 0);
}

/// 光照渐变序列端到端：LIC 自动启用 + 解码逐位一致（无损）。
#[test]
fn test_lic_shading_sequence_roundtrip() {
    let frames = make_shading_frames(4, 32, 24);
    let encoded = encode_sequence(&frames, &mk_params()).unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();

    // LIC 必须真实启用（光照渐变内容；至少一半差分帧命中）
    let lic_fields = frame_lic_fields(&encoded, frames.len());
    let enabled_diff_frames = lic_fields.iter().skip(1).filter(|(a, _)| *a != 0).count();
    assert!(
        enabled_diff_frames >= 2,
        "光照渐变差分帧应自动启用 LIC，实际启用 {enabled_diff_frames}/{} 帧: {lic_fields:?}",
        frames.len() - 1
    );
    // 首帧恒不启用
    assert_eq!(lic_fields[0].0, 0, "首帧不得携带 LIC 信令");
    // DecodeResult 收集一致
    assert_eq!(dec.frame_lic, lic_fields);

    // 无损逐位还原（生产逻辑：LIC(首帧) + 残差）
    let restored = DecodeSession::restore_temporal(&dec);
    assert_eq!(restored.len(), frames.len());
    for (i, (r, o)) in restored.iter().zip(&frames).enumerate() {
        assert_eq!(
            r.pixels,
            o.pixels,
            "第 {i} 帧（lic={:?}）无损还原必须逐位一致",
            lic_fields.get(i)
        );
    }
}

/// CRF_DISABLE_LIC=1：LIC 竞争完全关闭（逃生门 / A/B 验证）。
///
/// 注意：环境变量是进程级全局状态，Rust 测试并行执行时会互相污染
/// （设置窗口会跨越其他测试的编码窗口），因此本测试不做 env 写入，
/// 关闭语义由 CLI 层进程级 A/B 验证（文档 §43）承担。这里仅固定默认
/// 开启语义——lic_globally_enabled 只在外部显式注入 1 时关闭。
#[test]
fn test_lic_globally_enabled_default() {
    // 进程环境在本文件路径下不应出现 CRF_DISABLE_LIC=1（A/B 在 CLI 层做）；
    // 此处断言默认开启，防止误关闭影响其他 LIC 测试。
    assert!(std::env::var("CRF_DISABLE_LIC")
        .map(|v| v != "1")
        .unwrap_or(true));
    assert!(crate::crf::encoder::sequence_tools::lic_globally_enabled());
}

/// batch 与 streaming 在 LIC 决策上逐字节一致（含启用帧）。
#[test]
fn test_lic_batch_streaming_identical() {
    let frames = make_shading_frames(5, 28, 20);
    let params = mk_params();

    let batched = encode_sequence(&frames, &params).unwrap();
    let streaming: Vec<u8> = {
        let mut enc = StreamingEncoder::new(&params).unwrap();
        for f in &frames {
            enc.push_frame(f).unwrap();
        }
        enc.finish().unwrap()
    };
    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
    for i in 0..streaming.len() {
        assert_eq!(
            streaming[i], batched[i],
            "batch/streaming 首个差异 @{i}（LIC 决策必须逐字节一致）"
        );
    }
}

/// 无乘加光照收益的内容（纯局部差分）不得产生 LIC 帧——字节竞争的
/// 单调不劣化特性：无收益帧自动退化为恒等 (a,b)=(100,0) 不启用。
#[test]
fn test_lic_no_benefit_content_no_lic_frames() {
    // 高色数插画式局部差分：每帧仅替换一个局部区域，无系统性乘加偏移
    let w = 32u16;
    let h = 24u16;
    let golden: Vec<i32> = {
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                px.push((x as i32 * 5 + y as i32 * 11) % 256);
                px.push((x as i32 * 7 + y as i32 * 3) % 256);
                px.push((x as i32 * 13 + y as i32 * 17) % 256);
            }
        }
        px
    };
    let frames: Vec<ImageData> = (0..4)
        .map(|k| {
            let mut px = golden.clone();
            // 局部替换：仅中心一块（8×8）变化
            for y in 8..16 {
                for x in 8..16 {
                    let p = (y * w as usize + x) * 3;
                    let shift = k * 5;
                    px[p] = (px[p] + 40 + shift) % 256;
                    px[p + 1] = (px[p + 1] + 30 + shift) % 256;
                    px[p + 2] = (px[p + 2] + 20 + shift) % 256;
                }
            }
            ImageData {
                width: w,
                height: h,
                bit_depth: 8,
                color_format: ColorFormat::Rgb,
                pixels: px,
            }
        })
        .collect();
    let encoded = encode_sequence(&frames, &mk_params()).unwrap();
    let lic_fields = frame_lic_fields(&encoded, frames.len());
    assert!(
        lic_fields.iter().skip(1).all(|(a, _)| *a == 0),
        "无乘加收益内容不得启用 LIC: {lic_fields:?}"
    );
    // 往返仍逐位一致
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    let restored = DecodeSession::restore_temporal(&dec);
    for (i, (r, o)) in restored.iter().zip(&frames).enumerate() {
        assert_eq!(r.pixels, o.pixels, "第 {i} 帧必须逐位一致");
    }
}

/// 类型锚点：保证 frame_lic 访问路径可编译（防止结构演进漏改）。
#[allow(dead_code)]
fn _decode_result_lic_anchor(result: &crate::crf::core::domain::DecodeResult) -> Option<(u8, u8)> {
    result.frame_lic.first().copied()
}
