//! 流式编码器测试

use crate::crf::encoder::streaming::StreamingEncoder;
use crate::crf::format::{ColorFormat, EncodeParams, ImageData, PredictionMode};

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

fn mk_params() -> EncodeParams {
    EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Average,
        adaptive_prediction: true,
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: true,
        user_metadata: None,
    }
}

/// >50 帧：流式 API 解除一次性编码的 50 帧上限
#[test]
fn test_streaming_60_frames() {
    let w = 24u16;
    let h = 16u16;
    let frames: Vec<ImageData> = (0..60).map(|i| make_frame(i % 7, w, h)).collect();

    let mut enc = StreamingEncoder::new(&mk_params()).unwrap();
    for f in &frames {
        enc.push_frame(f).unwrap();
    }
    let encoded = enc.finish().unwrap();

    // 解码端按 header.frame_count 还原全部 60 帧
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    assert_eq!(dec.header.frame_count, 60);
    assert_eq!(dec.frames.len(), 60);

    // golden 首帧逐位一致；抽样帧 golden 差分还原正确
    assert_eq!(dec.frames[0].pixels, frames[0].pixels);
    let restored_33: Vec<i32> = dec.frames[0]
        .pixels
        .iter()
        .zip(dec.frames[33].pixels.iter())
        .map(|(a, b)| a + b)
        .collect();
    assert_eq!(restored_33, frames[33].pixels);
}

/// 流式输出与 encode_sequence 输出在无损模式下**逐字节一致**
/// （同参数、同帧序——格式兼容的直接证明）
#[test]
fn test_streaming_matches_batch_lossless() {
    let w = 16u16;
    let h = 16u16;
    let frames: Vec<ImageData> = (0..5).map(|i| make_frame(i * 2, w, h)).collect();

    let streaming = {
        let mut enc = StreamingEncoder::new(&mk_params()).unwrap();
        for f in &frames {
            enc.push_frame(f).unwrap();
        }
        enc.finish().unwrap()
    };
    let batched = super::super::encode_sequence(&frames, &mk_params()).unwrap();

    // 索引偏移布局完全一致 → 全文件逐字节一致
    if streaming.len() != batched.len() {
        panic!(
            "文件大小不一致: streaming={} batched={}",
            streaming.len(),
            batched.len()
        );
    }
    for i in 0..streaming.len() {
        if streaming[i] != batched[i] {
            let s = i.saturating_sub(12);
            panic!(
                "首个差异 @{}: streaming={} batched={}\n  s[..]={:02x?}\n  b[..]={:02x?}",
                i,
                streaming[i],
                batched[i],
                &streaming[s..(i + 12).min(streaming.len())],
                &batched[s..(i + 12).min(batched.len())]
            );
        }
    }
}

/// 少于 2 帧时 finish 报错（与 encode_sequence 语义一致）
#[test]
#[should_panic(expected = "FrameCountOutOfRange")]
fn test_streaming_min_frames() {
    let mut enc = StreamingEncoder::new(&mk_params()).unwrap();
    enc.push_frame(&make_frame(0, 8, 8)).unwrap();
    let _ = enc.finish().unwrap();
}

/// v1.13 RCT 首帧自适应：高饱和纯色内容（首帧 RGB 直通胜出、
/// flags.bit3=1）下流式与批量输出仍**逐字节一致**——两条编码路径
/// 的双路竞争实现对称的直接证明。
#[test]
fn test_streaming_matches_batch_first_frame_bypass() {
    let w = 32u16;
    let h = 24u16;
    let mut f0 = vec![0i32; w as usize * h as usize * 3];
    for px in f0.chunks_exact_mut(3) {
        px[0] = 200;
        px[1] = 0;
        px[2] = 100; // G=0 高饱和
    }
    let mut f1 = f0.clone();
    for px in f1.chunks_exact_mut(3).skip(w as usize * h as usize / 2) {
        px[0] = 50;
        px[2] = 220;
    }
    let frames = vec![
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f0,
        },
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f1,
        },
    ];

    let streaming = {
        let mut enc = StreamingEncoder::new(&mk_params()).unwrap();
        for f in &frames {
            enc.push_frame(f).unwrap();
        }
        enc.finish().unwrap()
    };
    let batched = super::super::encode_sequence(&frames, &mk_params()).unwrap();

    // 确认该内容确实触发首帧直通（保证测试覆盖目标分支）
    let dec = crate::crf::decode_from_bytes(&batched).unwrap();
    assert!(
        dec.header.flags.first_frame_no_rct(),
        "测试内容应使首帧直通胜出"
    );

    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
    for i in 0..streaming.len() {
        if streaming[i] != batched[i] {
            panic!(
                "首个差异 @{}: streaming={} batched={}",
                i, streaming[i], batched[i]
            );
        }
    }
}
