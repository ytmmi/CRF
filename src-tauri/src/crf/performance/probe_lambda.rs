//! RDOQ λ 敏感性探针（P4.6 前置验证，S1）
//!
//! 背景：Trellis 量化（frame_type=6 DCT 候选的第二轮再竞争）的 λ 硬编码为
//! 850/100·Q_pos²；而 DCT 在有损档（q95/q90/q75）全档零胜出（§11.1/§25/§26
//! 三次确认）。P4.6 规划「RDOQ λ 与 Q_target 联合标定」的收益被零胜出架空。
//!
//! 本探针回答：**DCT 零胜出的根因是否在 λ**。若在 λ 扫描范围内（×0.1~×4）
//! DCT+Trellis 的体积仍恒大于 adaptive 最终胜出者，则 λ 标定无法让 DCT
//! 翻盘 → P4.6 冻结；若某 λ 下 DCT 体积跌破胜出者，λ 标定值得投入。
//!
//! 结构事实（决定探针形态）：λ 只影响 Trellis 阶段，而 Trellis 只在
//! DCT 变体竞争的胜者为 QM（感知矩阵）变体时运行——若 best_dct 是 flat
//! 变体，λ 完全无介入机会，该帧的 λ 敏感性为 0。
//!
//! 方法：对 golden 差分帧（与路径 G 一致）做 RCT 后：
//! 1. 跑完整 `encode_frame_adaptive`（q90 生产参数）→ 胜出 frame_type 与体积；
//! 2. 复刻 DCT 6 变体竞争 → best_dct（形状/QM 标志/体积）；
//! 3. 若 best_dct 为 QM 变体，对 λ ∈ {85,212,425,850,1700,3400} 跑
//!    Trellis + CABAC，记录各 λ 体积。
//!
//! 不接入生产路径，仅由 `--probe-lambda <dir>` CLI 分派。

use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::color::rct;
use crate::crf::core::config::lossy_v2::{KernelLossyConfig, LossyOptionsV2, ResolveContext};
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};
use crate::crf::core::transform::qm::{
    DCT_PERCEPTUAL_QM, DCT_PERCEPTUAL_QM8, DCT_PERCEPTUAL_QM_TALL, DCT_PERCEPTUAL_QM_WIDE,
};
use crate::crf::core::transform::rdoq::{trellis_quantize_interleaved, DEFAULT_LAMBDA_NUM};
use crate::crf::encoder::dct_path::dct_quantize_interleaved_bs;
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::encoder::rle_cabac;
use crate::crf::performance::bench::load_frames;

/// λ 扫描点：DEFAULT_LAMBDA_NUM(850) × {0.1, 0.25, 0.5, 1, 2, 4}。
/// 3400 对应 optimization-review §五 待办「λ 两档竞争 850/3400」的上档。
const LAMBDA_SCANS: [i64; 6] = [85, 212, 425, DEFAULT_LAMBDA_NUM, 1700, 3400];

/// DCT 载荷标志位（与 candidate.rs 第七阶段一致）
const BW8_FLAG_BIT: u8 = 0x20;
const BH8_FLAG_BIT: u8 = 0x80;
const QM_FLAG_BIT: u8 = 0x40;
const TRELLIS_FLAG_BIT: u8 = 0x10;

/// 复刻 candidate.rs 第七阶段的 DCT 6 变体竞争（不含 Trellis 轮），
/// 返回胜出变体 (体积, 块宽, 块高, 是否 QM)。
fn dct_variant_best(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
) -> Result<Option<(usize, usize, usize, bool)>, String> {
    let variants: &[(usize, usize, bool)] = &[
        (4, 4, false),
        (8, 8, false),
        (4, 4, true),
        (8, 8, true),
        (8, 4, true),
        (4, 8, true),
    ];
    let mut best: Option<(usize, usize, usize, bool)> = None;
    for &(block_w, block_h, use_qm) in variants {
        let q_coeff = dct_quantize_interleaved_bs(
            pixels, width, height, components, q_step, block_w, block_h, use_qm, false,
        );
        let (payload, k) = rle_cabac::encode_frame_rle_cabac_adaptive(&q_coeff, None)
            .map_err(|e| e.to_string())?;
        let mut flag_byte = k;
        if block_w == 8 {
            flag_byte |= BW8_FLAG_BIT;
        }
        if block_h == 8 {
            flag_byte |= BH8_FLAG_BIT;
        }
        if use_qm {
            flag_byte |= QM_FLAG_BIT;
        }
        let len = FRAME_HEADER_SIZE + payload.len() + 1;
        let _ = flag_byte;
        if best.as_ref().is_none_or(|(sz, ..)| len < *sz) {
            best = Some((len, block_w, block_h, use_qm));
        }
    }
    Ok(best)
}

/// 指定 λ 下 Trellis + CABAC 的 DCT 载荷体积（candidate.rs Trellis 轮复刻）。
#[allow(clippy::too_many_arguments)] // 探针复刻生产 Trellis 轮，参数为算法固有维度
fn trellis_payload_len(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    q_step: u8,
    block_w: usize,
    block_h: usize,
    table: &[u32],
    lambda_num: i64,
) -> Result<usize, String> {
    let t_coeff = trellis_quantize_interleaved(
        pixels, width, height, components, q_step, block_w, block_h, table, lambda_num,
    );
    let (payload, k) =
        rle_cabac::encode_frame_rle_cabac_adaptive(&t_coeff, None).map_err(|e| e.to_string())?;
    let flag_byte = k
        | TRELLIS_FLAG_BIT
        | QM_FLAG_BIT
        | if block_w == 8 { BW8_FLAG_BIT } else { 0 }
        | if block_h == 8 { BH8_FLAG_BIT } else { 0 };
    let _ = flag_byte;
    Ok(FRAME_HEADER_SIZE + payload.len() + 1)
}

/// 运行探针。`dir` 为图像组目录。
pub fn run(dir: &str) -> Result<(), String> {
    let frames = load_frames(dir)?;
    if frames.len() < 2 {
        return Err(format!("{dir}: 至少需要 2 帧"));
    }
    // q90 生产参数：与 CLI `--lossy-quality 90` 完全一致（V2 resolved）
    let options = LossyOptionsV2::builder_preset(9000)
        .build()
        .map_err(|e| e.to_string())?;
    let ctx = ResolveContext {
        components: Some(3),
        frame_count: Some(frames.len()),
    };
    let tuning = KernelLossyConfig::from_options(Some(&options), ctx).map_err(|e| e.to_string())?;
    // 差分帧（i≥1、非 anchor）的 FrameQuant：与 fq_for_index 语义一致
    let fq = FrameQuant {
        step: tuning.global_step,
        bias: tuning.deadzone_bias,
        chroma_step: tuning.chroma_step(tuning.global_step),
        chroma_bias: tuning.chroma_deadzone_bias,
        chroma_half_res: tuning.chroma_half_res,
        q1_matrix_scale: false,
    };
    let width = frames[0].width as usize;
    let height = frames[0].height as usize;
    let components = 3usize;
    println!("=== RDOQ lambda sensitivity probe (S1) ===");
    println!(
        "group: {dir}  ({}x{}, {} 帧)\nq90 参数: step={}, bias={:+}, chroma_step={}, chroma_half_res={}\n",
        width,
        height,
        frames.len(),
        fq.step,
        fq.bias,
        fq.chroma_step,
        fq.chroma_half_res,
    );

    let mut dct_wins = 0usize;
    let mut flat_best = 0usize;
    let mut qm_best = 0usize;
    let mut wins_under_scan: [usize; LAMBDA_SCANS.len()] = [0; LAMBDA_SCANS.len()];
    let mut dct_total = 0usize;

    for i in 1..frames.len() {
        let diff: Vec<i32> = frames[i]
            .pixels
            .iter()
            .zip(&frames[0].pixels)
            .map(|(a, b)| a - b)
            .collect();
        let ycocg = rct::rct_forward(&diff, components).map_err(|e| e.to_string())?;
        let eff = ImageData {
            width: frames[0].width,
            height: frames[0].height,
            bit_depth: 8,
            color_format: ColorFormat::Rgb,
            pixels: ycocg,
        };
        // 1. 完整 adaptive（生产 λ=850）
        let out =
            encode_frame_adaptive(&eff, CompressionType::GolombRice, 8, false, fq, None, None)
                .map_err(|e| e.to_string())?;
        let winner_type = out.data.get(8).copied().unwrap_or(0);
        let winner_len = out.data.len();
        if winner_type == 6 {
            dct_wins += 1;
        }

        // 2. DCT 变体竞争（复刻第七阶段）
        let best_dct = dct_variant_best(&eff.pixels, width, height, components, fq.step)?;
        let Some((dct_len, block_w, block_h, use_qm)) = best_dct else {
            println!(
                "frame {:>3}: winner=type{winner_type} len={winner_len}  dct=<skip>\n",
                i,
            );
            continue;
        };
        dct_total += 1;

        if !use_qm {
            // flat 变体胜出 → Trellis 无介入机会，λ 敏感性为 0
            flat_best += 1;
            println!(
                "frame {:>3}: winner=type{winner_type} len={winner_len}  best_dct={block_w}x{block_h} flat len={dct_len}  (Trellis 无介入)",
                i
            );
        } else {
            qm_best += 1;
            let table: &[u32] = match (block_w, block_h) {
                (8, 8) => &DCT_PERCEPTUAL_QM8,
                (8, 4) => &DCT_PERCEPTUAL_QM_WIDE,
                (4, 8) => &DCT_PERCEPTUAL_QM_TALL,
                _ => &DCT_PERCEPTUAL_QM,
            };
            let mut scan_lens = [0usize; LAMBDA_SCANS.len()];
            for (si, &lambda_num) in LAMBDA_SCANS.iter().enumerate() {
                scan_lens[si] = trellis_payload_len(
                    &eff.pixels,
                    width,
                    height,
                    components,
                    fq.step,
                    block_w,
                    block_h,
                    table,
                    lambda_num,
                )?;
                if scan_lens[si] < winner_len {
                    wins_under_scan[si] += 1;
                }
            }
            let t_str: Vec<String> = scan_lens.iter().map(|l| l.to_string()).collect();
            println!(
                "frame {:>3}: winner=type{winner_type} len={winner_len}  best_dct={block_w}x{block_h} QM len={dct_len}  trellis[{}]",
                i,
                t_str.join(","),
            );
        }
    }

    println!("\n--- 汇总 ---");
    println!("差分帧总数: {}", frames.len() - 1);
    println!("DCT 胜出帧: {dct_wins}");
    println!("DCT 参与竞争帧（dct_variant_best 有结果）: {dct_total}");
    println!("best_dct 为 flat（Trellis 无介入、λ 无关）: {flat_best}");
    println!("best_dct 为 QM（Trellis 有介入机会）: {qm_best}");
    println!("\n各 λ 下 DCT(Trellis) 体积 < adaptive 胜出体积的帧数:");
    for (si, &lambda_num) in LAMBDA_SCANS.iter().enumerate() {
        println!(
            "  λ={lambda_num:>4} (×{:.2}): {}/{}",
            lambda_num as f64 / DEFAULT_LAMBDA_NUM as f64,
            wins_under_scan[si],
            qm_best,
        );
    }
    println!("\n判定参考：");
    println!("  任一 λ 下胜出帧数 > 0 ⟹ λ 标定可能让 DCT 翻盘，P4.6 值得投入；");
    println!("  全部 λ 下胜出帧数 = 0 ⟹ DCT 零胜出根因不在 λ，P4.6 冻结。");
    Ok(())
}
