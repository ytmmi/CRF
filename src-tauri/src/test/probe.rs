//! 超块先导探针与残差分布分析

use crate::crf;

use super::{collect_png_paths, load_frame_sequence};

/// 运行超块分区先导探针：条带列方向二分收益测量（第四批 #1 先导验证）
///
/// 对图像组每帧构造与路径 G 相同的 RCT 域编码输入（首帧 = rct_forward(原图)，
/// 差分帧 = rct_forward(frame − frame0)），在 {32, 64} 两档条带高度下测量
/// 「整条带 vs 列方向二分」的熵编码字节差。纯编码端本地测量，不产出码流。
pub fn run_probe_split_tests(png_dir: &str) {
    use crate::crf::encoder::banded::probe_band_split_savings;

    println!("=== 超块先导探针：条带列方向二分收益测量 ===");
    println!("图像组: {}\n", png_dir);
    let paths = collect_png_paths(png_dir);
    if paths.len() < 2 {
        eprintln!("需要至少 2 张图片，实际 {}", paths.len());
        return;
    }
    let frames = load_frame_sequence(&paths);
    let components = frames[0].color_format.component_count();

    for band_height in [32usize, 64usize] {
        let mut total_full = 0usize;
        let mut total_split = 0usize;
        let mut total_bands = 0usize;
        let mut won_bands = 0usize;
        println!("--- 条带高度 {} 行 ---", band_height);
        for (i, frame) in frames.iter().enumerate() {
            // 与路径 G 一致的 RCT 域编码输入
            let diff_rgb: Vec<i32> = if i == 0 {
                frame.pixels.clone()
            } else {
                frame
                    .pixels
                    .iter()
                    .zip(frames[0].pixels.iter())
                    .map(|(a, b)| a - b)
                    .collect()
            };
            let eff =
                crate::crf::core::color::rct::rct_forward(&diff_rgb, components).expect("RCT 失败");
            let img = crf::ImageData {
                width: frame.width,
                height: frame.height,
                bit_depth: frame.bit_depth,
                color_format: frame.color_format,
                pixels: eff,
            };
            let probe = probe_band_split_savings(&img, band_height)
                .unwrap_or_else(|e| panic!("探针失败 帧{}: {}", i, e));
            let saved = probe.full_bytes.saturating_sub(probe.split_bytes);
            let pct = if probe.full_bytes > 0 {
                saved as f64 / probe.full_bytes as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  帧{:2}: 整带 {:9} B | 二分 {:9} B | 节省 {:8} B ({:5.2}%) | 二分胜出 {}/{} 条带",
                i,
                probe.full_bytes,
                probe.split_bytes,
                saved,
                pct,
                probe.bands_won,
                probe.band_count
            );
            total_full += probe.full_bytes;
            total_split += probe.split_bytes;
            won_bands += probe.bands_won;
            total_bands += probe.band_count;
        }
        let saved_total = total_full.saturating_sub(total_split);
        let pct_total = if total_full > 0 {
            saved_total as f64 / total_full as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  汇总: banded 载荷 {:9} → {:9} B，节省 {:.2}%（二分胜出 {}/{} 条带）",
            total_full, total_split, pct_total, won_bands, total_bands
        );
    }
    println!("\n=== 探针完成 ===");
}

/// 分析差分数据分布
#[allow(dead_code)] // 预留探针辅助，待探针入口接线
pub(crate) fn analyze_residual_distribution(residuals: &[crf::ImageData]) {
    println!("--- 差分数据分布分析 ---");

    let mut total: u64 = 0;
    let mut zero_count: u64 = 0;
    let mut small_count: u64 = 0;
    let mut medium_count: u64 = 0;
    let mut large_count: u64 = 0;
    let mut min_val = i32::MAX;
    let mut max_val = i32::MIN;
    let mut sum: i64 = 0;
    let mut sum_sq: i64 = 0;

    for frame in residuals {
        for &v in &frame.pixels {
            total += 1;
            sum += v as i64;
            sum_sq += (v as i64) * (v as i64);

            if v < min_val {
                min_val = v;
            }
            if v > max_val {
                max_val = v;
            }

            if v == 0 {
                zero_count += 1;
            }
            if v.abs() <= 1 {
                small_count += 1;
            }
            if v.abs() <= 10 {
                medium_count += 1;
            }
            if v.abs() > 100 {
                large_count += 1;
            }
        }
    }

    let mean_val = sum as f64 / total as f64;
    let variance = sum_sq as f64 / total as f64 - mean_val * mean_val;
    let std_dev = variance.sqrt();

    println!("总像素值数量: {}", total);
    println!("差分值范围: [{}, {}]", min_val, max_val);
    println!("平均值: {:.2}, 标准差: {:.2}", mean_val, std_dev);
    println!("分布统计:");
    println!(
        "  零值占比: {:.2}%",
        zero_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值≤1占比: {:.2}%",
        small_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值≤10占比: {:.2}%",
        medium_count as f64 / total as f64 * 100.0
    );
    println!(
        "  绝对值>100占比: {:.4}%",
        large_count as f64 / total as f64 * 100.0
    );
    println!();
}

/// 实验探针：无损路径放开 DCT(Q=1) 候选的价值验证
///
/// 比较「空间域 Med 预测 + RLE」vs「DCT(Q=1 恒等) + CABAC」的载荷大小，
/// 覆盖三类合成内容（平滑渐变 / 斜线图案 / 随机纹理）。
#[test]
fn probe_lossless_dct_candidate_value() {
    use crate::crf::encoder::rle_cabac::encode_frame_rle_cabac_adaptive;
    use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;

    let cases: [(&str, Vec<i32>, usize, usize); 3] = [
        (
            "平滑渐变",
            {
                let mut v = Vec::with_capacity(128 * 128);
                for y in 0..128usize {
                    for x in 0..128usize {
                        v.push((50 + x / 2 + y / 4) as i32);
                    }
                }
                v
            },
            128,
            128,
        ),
        (
            "斜线图案",
            {
                let mut v = vec![0i32; 128 * 128];
                for y in 0..128usize {
                    for x in 0..128usize {
                        v[y * 128 + x] = (((x + y) % 32) * 7) as i32;
                    }
                }
                v
            },
            128,
            128,
        ),
        (
            "随机纹理",
            {
                let mut state = 0x12345678u64;
                (0..128 * 128)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        ((state >> 33) % 251) as i32 - 125
                    })
                    .collect()
            },
            128,
            128,
        ),
    ];

    for (name, px, w, h) in cases {
        let med_res = crf::apply_prediction(&px, w, h, 1, crf::PredictionMode::Med);
        let (rle_buf, _) = encode_frame_rle_golomb_adaptive(&med_res).unwrap();
        let rle_bytes = rle_buf.len();

        let coeffs4 = crate::crf::encoder::dct_path::dct_quantize_interleaved_bs(
            &px, w, h, 1, 1, 4, 4, false, false,
        );
        let (p4, _) = encode_frame_rle_cabac_adaptive(&coeffs4, None).unwrap();
        let cabac_dct4 = p4.len();

        let coeffs8 = crate::crf::encoder::dct_path::dct_quantize_interleaved_bs(
            &px, w, h, 1, 1, 8, 8, false, false,
        );
        let (p8, _) = encode_frame_rle_cabac_adaptive(&coeffs8, None).unwrap();
        let cabac_dct8 = p8.len();

        println!(
            "{}: RLE(Med)={} | DCT4={} (Δ{}) | DCT8={} (Δ{})",
            name,
            rle_bytes,
            cabac_dct4,
            rle_bytes as i64 - cabac_dct4 as i64,
            cabac_dct8,
            rle_bytes as i64 - cabac_dct8 as i64,
        );
    }
}
