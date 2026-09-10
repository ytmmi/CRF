//! 流式编码器测试

use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::encoder::streaming::StreamingEncoder;

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
        lossy: None,
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

/// q90 有损 V2 参数（默认 FirstFrameMode::MatchSequence → 首帧与序列同档
/// 量化，即**有损 golden**——正是 P0 闭环必须成立的条件）。
/// 显式 `referenceMode=golden`：streaming 未实现 P5.1 previous 参考竞争
/// （batch 默认 Hybrid 会逐帧 previous 竞争），闭环对齐验收限定在
/// 全 golden 语义（streaming 的能力边界）。
fn mk_lossy_params_q90() -> EncodeParams {
    let mut p = mk_params();
    p.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Golden)
            .build()
            .unwrap(),
    );
    p
}

/// P0 闭环（streaming 对齐）：**有损 golden** 下流式与批量输出逐字节一致。
/// 批量路径阶段 1c 以本地重建 G_hat 为差分基准（§9 P0 两阶段闭环）；本测试
/// 证明 streaming 首帧 push 的 G_hat 重建与之逐字节对称——若 streaming 仍以
/// 原始 frame0 为差分参考，有损首帧的量化误差会注入全部差分帧，产物必然
/// 与批量不一致。
#[test]
fn test_streaming_lossy_golden_closed_loop_matches_batch() {
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = (0..5).map(|i| make_frame(i * 2, w, h)).collect();
    let params = mk_lossy_params_q90();

    let streaming = {
        let mut enc = StreamingEncoder::new(&params).unwrap();
        for f in &frames {
            enc.push_frame(f).unwrap();
        }
        enc.finish().unwrap()
    };
    let batched = super::super::encode_sequence(&frames, &params).unwrap();

    // 前提确认：该配置确实产生有损 golden（首帧重建 ≠ 原始，闭环非平凡）
    let dec = crate::crf::decode_from_bytes(&batched).unwrap();
    assert!(
        dec.frames[0].pixels != frames[0].pixels,
        "q90 首帧应存在量化失真（有损 golden 前提）"
    );
    assert!(dec.header.flags.has_lossy_quant());

    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
    for i in 0..streaming.len() {
        if streaming[i] != batched[i] {
            let s = i.saturating_sub(12);
            panic!(
                "首个差异 @{}: streaming={} batched={}\n  s[..]={:02x?}\n  b[..]={:02x?}",
                i,
                streaming[i],
                batched[i],
                &streaming[s..(i + 12).min(streaming.len())],
                &batched[s..(i + 12).min(batched.len())],
            );
        }
    }
}

/// 解码端语义验证：有损 golden 下 streaming 产物还原式成立——
/// decoded[0] 是 G_hat（有损重建），decoded[i]（i≥1）是 golden 残差，
/// `G_hat + residual` 精确还原帧内容（残差本身可能另有量化误差，但
/// 参考基准必须与解码端一致——闭环的定义本身）。
#[test]
fn test_streaming_lossy_golden_decode_semantics() {
    let w = 24u16;
    let h = 20u16;
    let mut frames: Vec<ImageData> = (0..4).map(|i| make_frame(i * 3, w, h)).collect();
    // 首帧加非线性尖峰扰动：make_frame 是分段线性渐变，空间预测可完美
    // 吸收使 q90 量化零失真（闭环分支退化为平凡无损）；扰动保证首帧
    // 重建确有量化误差，测试真实覆盖有损 golden 闭环。
    for v in frames[0].pixels.iter_mut() {
        if *v > 100 {
            *v += 40;
        }
    }
    let params = mk_lossy_params_q90();

    let mut enc = StreamingEncoder::new(&params).unwrap();
    for f in &frames {
        enc.push_frame(f).unwrap();
    }
    let encoded = enc.finish().unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();

    // G_hat = decoded[0]（有损重建，≠ 原始 frame0）
    assert_ne!(dec.frames[0].pixels, frames[0].pixels);
    // 与批量解码结果逐像素一致（批量已由 §9 闭环测试锁定语义）
    let batched = super::super::encode_sequence(&frames, &params).unwrap();
    let dec_b = crate::crf::decode_from_bytes(&batched).unwrap();
    for (fa, fb) in dec.frames.iter().zip(dec_b.frames.iter()) {
        assert_eq!(
            fa.pixels, fb.pixels,
            "streaming 与批量解码结果必须逐像素一致"
        );
    }
}

/// 有损 golden + 首帧质量偏移（QualityOffset）：first_frame_step ≠
/// global_step 的显式档位路径同样闭环一致。
#[test]
fn test_streaming_lossy_golden_quality_offset_matches_batch() {
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = (0..4).map(|i| make_frame(i, w, h)).collect();
    let mut params = mk_lossy_params_q90();
    params.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Golden)
            .first_frame_offset(-500) // 首帧更高质量（q95 档），与序列档位分离
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
    let batched = super::super::encode_sequence(&frames, &params).unwrap();
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

/// P5.1 streaming + previous 模式：逐帧 previous 参考竞争与批量路径逐字节一致。
/// previous 模式下 streaming 编码器应使用前一帧重建作为差分参考，
/// 与批量路径 sequence.rs 第 512-588 行的 previous 竞争逻辑对齐。
#[test]
fn test_streaming_previous_matches_batch() {
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
    let batched = super::super::encode_sequence(&frames, &params).unwrap();

    // 验证编码结果逐字节一致
    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
    for i in 0..streaming.len() {
        if streaming[i] != batched[i] {
            let s = i.saturating_sub(12);
            panic!(
                "首个差异 @{}: streaming={} batched={}\n  s[..]={:02x?}\n  b[..]={:02x?}",
                i,
                streaming[i],
                batched[i],
                &streaming[s..(i + 12).min(streaming.len())],
                &batched[s..(i + 12).min(batched.len())],
            );
        }
    }

    // 验证解码端语义正确
    let dec_streaming = crate::crf::decode_from_bytes(&streaming).unwrap();
    let dec_batched = crate::crf::decode_from_bytes(&batched).unwrap();
    for (fa, fb) in dec_streaming.frames.iter().zip(dec_batched.frames.iter()) {
        assert_eq!(
            fa.pixels, fb.pixels,
            "streaming 与 batch 解码结果必须逐像素一致"
        );
    }
}

/// P5.1 streaming + hybrid 模式：混合参考（golden + previous 竞争）与批量路径逐字节一致。
/// hybrid 模式下编码器会在 golden 和 previous 之间选择更小的码字，
/// 流式编码器应实现相同的竞争逻辑。
#[test]
fn test_streaming_hybrid_matches_batch() {
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = (0..5).map(|i| make_frame(i * 2, w, h)).collect();
    let mut params = mk_lossy_params_q90();
    params.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Hybrid)
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
    let batched = super::super::encode_sequence(&frames, &params).unwrap();

    // 验证编码结果逐字节一致
    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
    for i in 0..streaming.len() {
        if streaming[i] != batched[i] {
            let s = i.saturating_sub(12);
            panic!(
                "首个差异 @{}: streaming={} batched={}\n  s[..]={:02x?}\n  b[..]={:02x?}",
                i,
                streaming[i],
                batched[i],
                &streaming[s..(i + 12).min(streaming.len())],
                &batched[s..(i + 12).min(batched.len())],
            );
        }
    }

    // 验证解码端语义正确
    let dec_streaming = crate::crf::decode_from_bytes(&streaming).unwrap();
    let dec_batched = crate::crf::decode_from_bytes(&batched).unwrap();
    for (fa, fb) in dec_streaming.frames.iter().zip(dec_batched.frames.iter()) {
        assert_eq!(
            fa.pixels, fb.pixels,
            "streaming 与 batch 解码结果必须逐像素一致"
        );
    }
}

/// P5.1 streaming + previous + 首帧质量偏移：多参数组合下的闭环一致性。
#[test]
fn test_streaming_previous_quality_offset_matches_batch() {
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = (0..4).map(|i| make_frame(i, w, h)).collect();
    let mut params = mk_lossy_params_q90();
    params.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(crate::crf::core::config::lossy_v2::ReferenceModeV2::Previous)
            .first_frame_offset(-500) // 首帧更高质量
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
    let batched = super::super::encode_sequence(&frames, &params).unwrap();
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

/// v1.15 多参考帧（prev2）：构造 prev2 独胜序列——帧 i 与帧 i-2 相同
/// （prev2 差分≈0），但与首帧（golden）和前一帧（previous）均差异大。
/// 序列 A,B,C,B,D,B：frame3=B 与 frame1=B 相同（prev2 差分 0），
/// 与 frame0=A（golden）、frame2=C（previous）差异大 → prev2 必胜。
/// Hybrid 模式产物应含 reference_type=2 帧；往返还原正确。
#[test]
fn test_prev2_used_in_hybrid_roundtrip() {
    use crate::crf::core::config::lossy_v2::{ReferenceModeV2, SceneCutModeV2};
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = [0i32, 100, 200, 100, 50, 100]
        .iter()
        .map(|&shift| make_frame(shift, w, h))
        .collect();
    let mut params = mk_lossy_params_q90();
    let mut lossy = crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
        .reference_mode(ReferenceModeV2::Hybrid)
        .build()
        .unwrap();
    // 关闭场景切换检测（周期交替的内容差异均值可能超默认阈值，阻断链式候选）
    lossy.temporal.scene_cut = SceneCutModeV2::Off;
    params.lossy = Some(lossy);

    let encoded = super::super::encode_sequence(&frames, &params).unwrap();

    // 解析每帧 reference_type：应存在 prev2（=2）帧（至少 frame3）
    use crate::crf::core::bitstream::constants::{
        FRAME_HEADER_SIZE, HEADER_SIZE, REFERENCE_TYPE_OFFSET,
    };
    let fcount = u16::from_le_bytes([encoded[8], encoded[9]]) as usize;
    let frames_start = HEADER_SIZE + fcount * 8;
    let mut off = frames_start;
    let mut ref_types = Vec::new();
    for _ in 0..fcount {
        let fs = u32::from_le_bytes(encoded[off..off + 4].try_into().unwrap()) as usize;
        ref_types.push(encoded[off + REFERENCE_TYPE_OFFSET]);
        off += FRAME_HEADER_SIZE + fs;
    }
    assert!(
        ref_types.iter().any(|&r| r == 2),
        "周期内容下 prev2 候选应胜出，参考类型分布: {:?}",
        ref_types
    );

    // 往返还原：有损下误差受控（prev2 还原公式正确）。注意高值内容帧
    // （shift=100 的 B 内容）在 q90 有损下的固有误差 ~80-86，与 prev2 无关
    // （帧1 非 prev2 同样 86）；prev2 帧（3/5）误差与 golden 帧同量级。
    let result = crate::crf::decode_from_bytes(&encoded).unwrap();
    let restored = crate::crf::decoder::session::DecodeSession::restore_temporal(&result);
    assert_eq!(restored.len(), frames.len());
    for (i, (r, o)) in restored.iter().zip(&frames).enumerate() {
        let max_err = r
            .pixels
            .iter()
            .zip(&o.pixels)
            .map(|(a, b)| (a - b).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_err < 128,
            "第 {} 帧（prev2={}）往返 max_err={} 应 <128",
            i,
            result.frame_prev2_refs.get(i).copied().unwrap_or(false),
            max_err
        );
    }
    // prev2 标志表与参考类型一致
    for (i, &rt) in ref_types.iter().enumerate() {
        assert_eq!(
            result.frame_prev2_refs[i],
            rt == 2,
            "帧 {} prev2 标志应与 reference_type 一致",
            i
        );
    }
}

/// prev2 的 batch/streaming 一致性：周期内容 + Hybrid，两条路径逐字节一致。
#[test]
fn test_streaming_prev2_matches_batch() {
    use crate::crf::core::config::lossy_v2::ReferenceModeV2;
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = [0i32, 100, 0, 100, 0, 100, 0]
        .iter()
        .map(|&shift| make_frame(shift, w, h))
        .collect();
    let mut params = mk_lossy_params_q90();
    params.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(ReferenceModeV2::Hybrid)
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
    let batched = super::super::encode_sequence(&frames, &params).unwrap();

    assert_eq!(streaming.len(), batched.len(), "文件大小不一致");
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

/// Previous 模式保持纯 previous 链：产物不得出现 reference_type=2 帧。
#[test]
fn test_prev2_not_used_in_previous_mode() {
    use crate::crf::core::config::lossy_v2::ReferenceModeV2;
    let w = 32u16;
    let h = 24u16;
    let frames: Vec<ImageData> = [0i32, 100, 0, 100, 0]
        .iter()
        .map(|&shift| make_frame(shift, w, h))
        .collect();
    let mut params = mk_lossy_params_q90();
    params.lossy = Some(
        crate::crf::core::config::lossy_v2::LossyOptionsV2::builder_preset(9000)
            .reference_mode(ReferenceModeV2::Previous)
            .build()
            .unwrap(),
    );

    let encoded = super::super::encode_sequence(&frames, &params).unwrap();
    use crate::crf::core::bitstream::constants::{
        FRAME_HEADER_SIZE, HEADER_SIZE, REFERENCE_TYPE_OFFSET,
    };
    let fcount = u16::from_le_bytes([encoded[8], encoded[9]]) as usize;
    let frames_start = HEADER_SIZE + fcount * 8;
    let mut off = frames_start;
    for _ in 0..fcount {
        let fs = u32::from_le_bytes(encoded[off..off + 4].try_into().unwrap()) as usize;
        let rt = encoded[off + REFERENCE_TYPE_OFFSET];
        assert_ne!(
            rt, 2,
            "Previous 模式不得使用 prev2 参考（reference_type=2）"
        );
        off += FRAME_HEADER_SIZE + fs;
    }
}
