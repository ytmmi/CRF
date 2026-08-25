//! P1 色度通道解耦验证（规划 §4.1 / §4.2）
//!
//! 锁定两项行为：
//! 1. 4:2:0 解耦——色度半分辨率由 `chroma_half_res` 参数独立决定，
//!    不再被 `step > 1` 阻断；q95（step=1）档位下半分辨率真实可用，
//!    且编解码往返保持文件自包含正确；
//! 2. 色度步长比例失效修复——Q × 130% 在小 Q 区间不再截断回亮度步长
//!    （单元边界见 format/quant.rs::tests），本模块验证其经完整管线
//!    （planar 三平面候选参与竞争）的端到端自洽。

use crate::crf::format::{ColorFormat, LossyTuning};
use crate::crf::{self, EncodeParams, ImageData, PredictionMode};

fn synthetic_sequence(frames: usize, w: u16, h: u16) -> Vec<ImageData> {
    let mut seq = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h as usize {
            for x in 0..w as usize {
                // 大面积平坦色块 + 局部渐变：典型赛璐璐上色的
                // 色度低频结构，planar/半分辨率路径的目标内容形态
                let band = (y / 8) as i32 * 30;
                let ramp = (x % 32) as i32;
                let r = (band + ramp).clamp(0, 255);
                let g = (band * 2 / 3 + 40 - f as i32 * 5).clamp(0, 255);
                let b = (200 - band - f as i32 * 3).clamp(0, 255);
                pixels.push(r);
                pixels.push(g);
                pixels.push(b);
            }
        }
        seq.push(ImageData {
            width: w,
            height: h,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
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

/// 还原序列（golden 固定基准 + chain 累加），返回逐帧像素
fn restore_sequence(result: &crf::DecodeResult) -> Vec<Vec<i32>> {
    let mut out = Vec::with_capacity(result.frames.len());
    let mut prev: Option<Vec<i32>> = None;
    let golden = result.frames[0].pixels.clone();
    for (i, dec) in result.frames.iter().enumerate() {
        let is_golden = result.frame_golden_refs.get(i).copied().unwrap_or(false);
        let restored = if i == 0 {
            dec.pixels.clone()
        } else if is_golden {
            golden.iter().zip(&dec.pixels).map(|(a, b)| a + b).collect()
        } else {
            match &prev {
                Some(p) => p.iter().zip(&dec.pixels).map(|(a, b)| a + b).collect(),
                None => dec.pixels.clone(),
            }
        };
        prev = Some(restored.clone());
        out.push(restored);
    }
    out
}

#[test]
fn p1_chroma_half_res_decoupled_from_step_q95() {
    // q95 档 step=1：旧实现此处半分辨率被 step>1 条件阻断；
    // 解耦后 chroma_half_res=true 应真实进入 planar 候选。
    let originals = synthetic_sequence(3, 64, 48);
    let params = EncodeParams {
        lossy_quality: Some(95),
        ..base_params() // 默认 LossyTuning：chroma_half_res=true
    };
    let encoded = crf::encode_sequence(&originals, &params).expect("q95 编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");

    assert_eq!(result.frames.len(), originals.len());
    let restored = restore_sequence(&result);
    assert_eq!(restored.len(), originals.len());
    for (i, px) in restored.iter().enumerate() {
        assert_eq!(px.len(), originals[i].pixels.len(), "第 {} 帧长度不一致", i);
    }
}

#[test]
fn p1_chroma_half_res_disabled_explicitly() {
    // 显式关闭半分辨率：参数独立可控性的另一面
    let originals = synthetic_sequence(3, 64, 48);
    let tuning = LossyTuning {
        chroma_half_res: false,
        ..Default::default()
    };
    let params = EncodeParams {
        lossy_quality: Some(75),
        lossy_tuning: Some(tuning),
        ..base_params()
    };
    let encoded = crf::encode_sequence(&originals, &params).expect("q75 编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");
    assert_eq!(result.frames.len(), originals.len());
}

#[test]
fn p1_chroma_step_effective_end_to_end() {
    // 色度步长失效修复后的端到端自洽：q90（step=2，chroma 升至 3）
    // 完整管线往返成功；误差有界（视觉无损语义不被破坏）
    let originals = synthetic_sequence(4, 56, 44);
    let params = EncodeParams {
        lossy_quality: Some(90),
        ..base_params()
    };
    let encoded = crf::encode_sequence(&originals, &params).expect("q90 编码失败");
    let result = crf::decode_from_bytes(&encoded).expect("解码失败");

    let restored = restore_sequence(&result);
    for (i, px) in restored.iter().enumerate() {
        let max_err = px
            .iter()
            .zip(&originals[i].pixels)
            .map(|(a, b)| (*a - *b).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_err <= 24,
            "第 {} 帧 max_err={} 超出 q90 视觉容差",
            i,
            max_err
        );
    }
}
