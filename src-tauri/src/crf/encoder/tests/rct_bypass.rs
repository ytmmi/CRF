//! v1.13 RCT 首帧自适应（首帧 YCoCg-R vs RGB 直通双路竞争）

use crate::crf::format::{
    ColorFormat, CompressionType, EncodeParams, ImageData, PredictionMode,
};

use super::super::encode_sequence;

fn mk_rgb_adaptive_params() -> EncodeParams {
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

/// 高饱和复杂场景（模拟换装差分插画）：G 通道全程恒零、R/B 为
/// 宽值域纹理（唯一色数远超 256，调色板候选自动放弃）——RGB 直通
/// 域 G 平面零行程极长，YCoCg-R 后三通道皆有值——直通必须胜出并
/// 置位 flags.bit3；解码端两条出口（bytes/file）均按标志跳过
/// frame0 的逆变换。
#[test]
fn test_rct_first_frame_bypass_pure_color() {
    let w = 96u16;
    let h = 96u16;
    // G=0 高饱和纹理：R/B 宽值域伪随机（确定性），确保调色板不接管
    let px_at = |x: u16, y: u16, sat: i32| -> [i32; 3] {
        let x = x as i32;
        let y = y as i32;
        [
            40 + (x * y * 7 % 180) + sat,
            0,
            (x * 13 + y * 29 % 170) as i32 % 200 + (sat * 2).min(55),
        ]
    };
    let mut f0 = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        for x in 0..w {
            let p = px_at(x, y, 0);
            f0.extend_from_slice(&p);
        }
    }
    // 右下象限换装差分：饱和度偏移（仍保持 G=0）
    let mut f1 = f0.clone();
    for y in (h / 2)..h {
        for x in (w / 2)..w {
            let idx = (y as usize * w as usize + x as usize) * 3;
            let p = px_at(x, y, 60);
            f1[idx] = p[0];
            f1[idx + 1] = p[1];
            f1[idx + 2] = p[2];
        }
    }
    let frames = vec![
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f0.clone(),
        },
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f1,
        },
    ];

    let encoded = encode_sequence(&frames, &mk_rgb_adaptive_params()).unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    assert!(dec.header.flags.has_rct(), "差分帧仍走 RCT");
    assert!(
        dec.header.flags.first_frame_no_rct(),
        "高饱和复杂场景首帧应以 RGB 直通存储"
    );
    // 首帧逐位一致（解码端跳过逆变换 → 即编码输入本身）
    assert_eq!(dec.frames[0].pixels, frames[0].pixels);
    // 差分帧 golden 还原逐位一致
    let restored: Vec<i32> = dec.frames[0]
        .pixels
        .iter()
        .zip(dec.frames[1].pixels.iter())
        .map(|(a, b)| a + b)
        .collect();
    assert_eq!(restored, frames[1].pixels);

    // decode_from_file 出口对称（Cursor 流式读取）
    let dec2 = crate::crf::decode_from_file(&mut std::io::Cursor::new(&encoded)).unwrap();
    assert_eq!(dec2.frames[0].pixels, frames[0].pixels);
    assert_eq!(dec2.frames[1].pixels, dec.frames[1].pixels);

    // 负向验证：手工清零 bit3 模拟旧语义解码——首帧被误做 rct_inverse，
    // 结果必然偏离原帧（证明解码端确实依赖该标志分流）
    let mut tampered = encoded.clone();
    tampered[17] &= !0x08;
    let dec_t = crate::crf::decode_from_bytes(&tampered).unwrap();
    assert_ne!(
        dec_t.frames[0].pixels, frames[0].pixels,
        "清零标志后首帧应被错误逆变换"
    );
}

/// 自然相关内容（R=G=B 灰度渐变）：通道完全相关，RCT 去相关后
/// Co/Cg 全程为零、大幅胜出——首帧必须保持 RCT 域存储（bit3=0）。
#[test]
fn test_rct_first_frame_natural_keeps_rct() {
    let w = 48u16;
    let h = 32u16;
    let make_gray_rgb = |shift: i32| -> ImageData {
        let mut px = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                let v = ((x as i32 + y as i32) * 3 + shift) % 256;
                px.push(v);
                px.push(v);
                px.push(v);
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
    let frames = vec![make_gray_rgb(0), make_gray_rgb(40), make_gray_rgb(90)];

    let encoded = encode_sequence(&frames, &mk_rgb_adaptive_params()).unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    assert!(
        !dec.header.flags.first_frame_no_rct(),
        "相关性强内容首帧 RCT 应胜出"
    );
    assert_eq!(dec.frames[0].pixels, frames[0].pixels);
    for i in 1..frames.len() {
        let restored: Vec<i32> = dec.frames[0]
            .pixels
            .iter()
            .zip(dec.frames[i].pixels.iter())
            .map(|(a, b)| a + b)
            .collect();
        assert_eq!(restored, frames[i].pixels);
    }
}

/// 有损模式冒烟：golden_lossless 默认 true 时首帧仍无损且双路竞争
/// 照常生效；差分帧量化管线不受影响（解码成功、首帧逐位一致）。
#[test]
fn test_rct_first_frame_lossy_smoke() {
    let w = 64u16;
    let h = 48u16;
    let mut f0 = vec![0i32; (w as usize) * (h as usize) * 3];
    for px in f0.chunks_exact_mut(3) {
        px[0] = 200;
        px[1] = 0;
        px[2] = 100;
    }
    let frames = vec![
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f0.clone(),
        },
        ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: f0,
        },
    ];
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Average,
        adaptive_prediction: true,
        lossy_quality: Some(75),
        lossy_tuning: None,
        input_original_frames: true,
        user_metadata: None,
    };
    let encoded = encode_sequence(&frames, &params).unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    // golden 首帧强制无损：无论哪条路胜出都逐位一致
    assert_eq!(dec.frames[0].pixels, frames[0].pixels);
}

/// 路径 C（预差分兼容输入）的首帧双路竞争：直通胜出时 flags.bit3
/// 置位且往返一致；preferred_mode 继承自胜出版本。
#[test]
fn test_rct_first_frame_bypass_path_c() {
    let w = 64u16;
    let h = 48u16;
    let mut f0 = vec![0i32; (w as usize) * (h as usize) * 3];
    for px in f0.chunks_exact_mut(3) {
        px[0] = 220;
        px[1] = 0;
        px[2] = 80;
    }
    // 预差分语义：frames[1..] 为相对前帧的残差（可为负值）
    let mut r1 = vec![0i32; (w as usize) * (h as usize) * 3];
    for px in r1.chunks_exact_mut(3) {
        px[2] = -30;
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
            pixels: r1,
        },
    ];
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Average,
        adaptive_prediction: true,
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: false, // 路径 C
        user_metadata: None,
    };

    let encoded = encode_sequence(&frames, &params).unwrap();
    let dec = crate::crf::decode_from_bytes(&encoded).unwrap();
    assert!(dec.header.flags.has_rct());
    assert!(
        dec.header.flags.first_frame_no_rct(),
        "路径 C 高饱和场景首帧直通同样应胜出"
    );
    assert_eq!(dec.frames[0].pixels, frames[0].pixels);
}
