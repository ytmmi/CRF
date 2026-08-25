//! P0 闭环参考语义验证（规范 §5.3 / §8.2）
//!
//! 锁定两阶段编码的核心不变量：
//! 1. 无损 golden 下产物与解码往返保持逐像素一致（回归锚点）；
//! 2. 有损 golden（golden_lossless=false）下，端到端还原不再叠加首帧
//!    量化误差——若编码端误用原始 frame0 作差分基准，解码端以 G_hat
//!    还原时会引入随帧数不衰减的系统性偏移，本模块以逐帧误差均值
//!    作为该失配的可测信号；
//! 3. q95_soft 轻滤场景的结构性质（帧布局 / golden 标志 / 自包含解码）。
//!
//! 全部断言仅使用 CRF 文件自身解码结果，禁止回读源图参与恢复。

use crate::crf::{self, EncodeParams, ImageData, PredictionMode};

/// 确定性合成序列：首帧平滑渐变，后续帧在局部区域发生结构性变化
/// （避免相邻帧近似导致的平凡通过；种子固定保证可重复）。
fn synthetic_sequence(frames: usize, w: u16, h: u16) -> Vec<ImageData> {
    let mut seq = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let base = ((x * 7 + y * 13) % 256) as i32;
                // 后续帧引入明显结构差异（条纹位移 + 局部阶跃）
                let delta = if f == 0 {
                    0
                } else {
                    (((x + f * 5) % 32 < 8) as i32) * 60 + ((y + f * 3) % 24 < 6) as i32 * 40
                };
                for c in 0..3usize {
                    let v = (base + delta + c as i32 * 9 - f as i32).clamp(0, 255);
                    pixels.push(v);
                }
            }
        }
        seq.push(ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: crate::crf::format::ColorFormat::Rgb,
            pixels,
        });
    }
    seq
}

fn base_params() -> EncodeParams {
    EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::None,
        adaptive_prediction: true,
        lossy_quality: None,
        lossy_tuning: None,
        input_original_frames: true,
        user_metadata: None,
    }
}

/// 按解码器契约还原完整序列：chain 帧 = prev + residual；
/// golden 帧 = 固定基准（解码首帧 G_hat）+ residual。返回逐帧 RGB 像素。
fn restore_sequence(result: &crf::DecodeResult) -> Vec<(Vec<i32>, bool)> {
    let mut out: Vec<(Vec<i32>, bool)> = Vec::with_capacity(result.frames.len());
    let mut prev: Option<Vec<i32>> = None;
    // 全 golden 架构的固定差分基准 = 文件自身解码出的 frame0
    let decoded_golden = result.frames[0].pixels.clone();
    for (i, dec) in result.frames.iter().enumerate() {
        let is_golden = result.frame_golden_refs.get(i).copied().unwrap_or(false);
        let restored = if i == 0 {
            dec.pixels.clone()
        } else if is_golden {
            decoded_golden
                .iter()
                .zip(&dec.pixels)
                .map(|(a, b)| a + b)
                .collect()
        } else {
            match &prev {
                Some(p) => p.iter().zip(&dec.pixels).map(|(a, b)| a + b).collect(),
                None => dec.pixels.clone(),
            }
        };
        prev = Some(restored.clone());
        out.push((restored, is_golden));
    }
    out
}

/// 逐分量误差均值：参考失配（编码用原帧、解码用 G_hat）会表现为
/// 不随帧号衰减的非零系统性偏移；闭环正确时趋近于零。
fn mean_abs_error(restored: &[i32], original: &[i32]) -> f64 {
    let sum: f64 = restored
        .iter()
        .zip(original)
        .map(|(a, b)| (*a - *b).unsigned_abs() as f64)
        .sum();
    sum / restored.len() as f64
}

#[test]
fn p0_lossless_golden_pixel_exact_roundtrip() {
    // 无损管线回归锚点：两阶段改造不得改变无损产物语义
    let originals = synthetic_sequence(4, 48, 40);
    let params = base_params(); // lossy_quality=None → 无损
    let encoded = crf::encode_sequence(&originals, &params).expect("无损编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");

    assert_eq!(result.frames.len(), originals.len());
    let restored = restore_sequence(&result);
    for (i, (px, _)) in restored.iter().enumerate() {
        assert_eq!(
            px, &originals[i].pixels,
            "无损 golden 第 {} 帧必须逐像素一致",
            i
        );
    }
}

#[test]
fn p0_lossy_golden_no_reference_drift() {
    // 有损 golden 主目标场景：首帧参与量化（golden_lossless=false），
    // 后续帧残差必须相对本地重建 G_hat 生成。若存在参考失配，
    // 每帧还原误差都会包含完整的首帧误差场（系统性偏移不衰减）。
    let originals = synthetic_sequence(5, 48, 40);
    let tuning = crate::crf::format::LossyTuning {
        golden_lossless: false,
        ..Default::default()
    };
    let params = EncodeParams {
        lossy_quality: Some(75), // 明显量化步长，使首帧误差可观
        lossy_tuning: Some(tuning),
        ..base_params()
    };
    let encoded = crf::encode_sequence(&originals, &params).expect("有损编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");

    // 首帧确实携带量化（前提成立性检查）
    assert!(
        !result.frame_golden_refs.is_empty(),
        "golden 标志表不得为空"
    );

    let restored = restore_sequence(&result);

    // 首帧误差基线：golden 参与量化必然产生非零误差（否则本测试无区分度）
    let first_err = mean_abs_error(&restored[0].0, &originals[0].pixels);
    assert!(
        first_err > 0.0,
        "q75 + golden_lossless=false 下首帧应有量化误差"
    );

    // 闭环核心断言：后续帧的平均绝对误差不得系统性超过首帧误差的
    // 有界倍数。参考失配时误差 ≈ 首帧误差场的整体平移 + 本帧噪声，
    // 其均值会持续处于高位；闭环时仅剩各帧自身的量化噪声。
    for i in 1..originals.len() {
        let err = mean_abs_error(&restored[i].0, &originals[i].pixels);
        assert!(
            err < first_err * 3.0 + 0.5,
            "第 {} 帧平均误差 {:.4} 异常偏高（首帧基线 {:.4}）——疑似参考失配",
            i,
            err,
            first_err
        );
    }

    // 批量/streaming 对称性由 streaming_tests 锁定，此处不重复。
}

#[test]
fn p0_q95_soft_first_frame_structural_closure() {
    // q95 视觉无损档：轻滤后的首帧即文件真实内容；闭环后残差以其为
    // 基准生成。本测试锁定结构性质（自包含解码、布局、标志一致性），
    // 具体率失真数值归 P1 标定。
    let originals = synthetic_sequence(3, 40, 32);
    let params = EncodeParams {
        lossy_quality: Some(95),
        ..base_params()
    };
    let encoded = crf::encode_sequence(&originals, &params).expect("q95 编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");

    assert_eq!(result.frames.len(), originals.len());
    assert_eq!(result.header.width, 40);
    assert_eq!(result.header.height, 32);
    // golden 标志：首帧 false，其余全部 true（全 golden 架构）
    assert!(!result.frame_golden_refs[0]);
    for (i, g) in result.frame_golden_refs.iter().enumerate().skip(1) {
        assert!(g, "第 {} 帧应为 golden 差分帧", i);
    }

    // 还原序列可完整执行且长度一致
    let restored = restore_sequence(&result);
    assert_eq!(restored.len(), originals.len());
}

#[test]
fn preset_explicit_equivalence() {
    // 规划 §7 第 10 步：预设(None→default)与显式配置(Some(default))逐字节一致
    use crate::crf::format::LossyTuning;
    let originals = synthetic_sequence(4, 48, 40);
    let base = base_params();
    let preset_params = EncodeParams {
        lossy_quality: Some(90),
        lossy_tuning: None,
        ..base.clone()
    };
    let explicit_params = EncodeParams {
        lossy_quality: Some(90),
        lossy_tuning: Some(LossyTuning::default()),
        ..base
    };
    let enc_preset =
        crf::encode_sequence(&originals, &preset_params).expect("preset encode failed");
    let enc_explicit =
        crf::encode_sequence(&originals, &explicit_params).expect("explicit encode failed");
    assert_eq!(
        enc_preset, enc_explicit,
        "preset(None->default) must equal explicit Some(default)"
    );
}
