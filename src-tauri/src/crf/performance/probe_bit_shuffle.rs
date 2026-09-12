//! 位洗牌 + 零消除掩码（LICO, DCC 2024）收益探针
//!
//! **背景**：LICO（Azami/Lawson/Burtscher, DCC 2024, `burtscher/LICO`）的变换链
//! 在 BMP 像素上依次做 y/x 差分 + 通道差分 + TCMS（幅值-符号）+ 通道分离转置 +
//! **BIT_1 位洗牌（8×8 位矩阵转置，位平面聚集）** + **ZERE 零消除（4 字节 /
//! 1 字节粒度，bitmap + 非零值）**。CRF 差分帧残差高度稀疏（大片 0），本探针
//! 测量「位洗牌 + 零消除」预处理对残差字节的压缩效果，与 CRF 现有编码对比。
//!
//! **探针口径**（每差分帧，RCT 残差 = `rct_forward(frame − golden)`）：
//! - **原始**：i32 → TCMS(zigzag) → 小端 4 字节，长度 = 4N；
//! - **ZERE**：原始 → ZERE_4 → ZERE_1（零消除；bitmap 未压缩，保守上界）；
//! - **位洗牌+ZERE**：原始 → BIT_1 → ZERE_4 → ZERE_1；
//! - **CRF 现有**：`encode_frame_adaptive`（预测 + CABAC/RLE/DCT 全竞争）。
//!
//! **判定**：若「位洗牌+ZERE」显著优于「ZERE」（位洗牌增益），且接近/优于
//! CRF 现有编码，则 LICO 变换在 CRF 残差上有价值。零码流改动、零外部数据集。
//!
//! CLI：`--probe-bit-shuffle [root]`（默认 `E:\CRF\test\png`）。

use crate::crf::core::color::rct;
use crate::crf::core::domain::{CompressionType, ImageData};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;
use rayon::prelude::*;

/// 每组参与探针的最大帧数（0 = 不限）。`CRF_PROBE_BIT_SHUFFLE_MAX` 可覆盖。
fn group_frame_limit() -> usize {
    std::env::var("CRF_PROBE_BIT_SHUFFLE_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(0)
}

/// TCMS（LICO 幅值-符号 / zigzag）：0→0、−1→1、1→2、−2→3 ……
#[inline]
fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

/// i32 序列 → TCMS → 小端字节流（4 字节/值）。
fn serialize_tcms_le(values: &[i32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for &v in values {
        out.extend_from_slice(&zigzag(v).to_le_bytes());
    }
    out
}

/// LICO BIT_1：全局 8×8 位矩阵转置（位平面聚集）。
///
/// 每 8 字节组做位转置，输出布局 `out[g + i*groups]` = 第 g 组的第 i 位平面
/// （与 LICO `h_BMP_BIT` 的 BIT_1 一致）。尾部不足 8 字节原样保留。
fn bit_shuffle(input: &[u8]) -> Vec<u8> {
    let esize = (input.len() / 8) * 8;
    let groups = esize / 8;
    let mut out = vec![0u8; input.len()];
    for g in 0..groups {
        let b = &input[g * 8..g * 8 + 8];
        for i in 0..8 {
            let mut byte = 0u8;
            for (j, &v) in b.iter().enumerate() {
                if (v >> i) & 1 != 0 {
                    byte |= 1 << j;
                }
            }
            out[g + i * groups] = byte;
        }
    }
    out[esize..].copy_from_slice(&input[esize..]);
    out
}

/// LICO ZERE：`word_size` 字节粒度的零消除。
///
/// 每 `word_size*8` 个 word 一组，bitmap 标记非零 word，只写非零 word。
/// 输出 `[非零 word 数据][bitmap][尾部字节][pos(u16 LE)][csize(u16 LE)]`。
/// 与 LICO 的区别：bitmap 未做 REP 压缩（保守上界）。
fn zero_eliminate(input: &[u8], word_size: usize) -> Vec<u8> {
    let bits = word_size * 8;
    let n_words = input.len() / word_size;
    let num = n_words.div_ceil(bits);
    let mut data = Vec::with_capacity(input.len());
    let mut bitmap = vec![0u8; num * word_size];
    let mut cnt = 0usize;
    for i in 0..num {
        for j in 0..bits {
            if cnt >= n_words {
                break;
            }
            let w = &input[cnt * word_size..(cnt + 1) * word_size];
            if w.iter().any(|&b| b != 0) {
                bitmap[i * word_size + j / 8] |= 1 << (j % 8);
                data.extend_from_slice(w);
            }
            cnt += 1;
        }
    }
    let pos = data.len();
    let mut out = data;
    out.extend_from_slice(&bitmap);
    out.extend_from_slice(&input[n_words * word_size..]);
    out.push((pos & 0xff) as u8);
    out.push(((pos >> 8) & 0xff) as u8);
    out.push((input.len() & 0xff) as u8);
    out.push(((input.len() >> 8) & 0xff) as u8);
    out
}

/// LICO 式完整变换：可选位洗牌 → ZERE_4 → ZERE_1。
fn lico_transform(bytes: &[u8], shuffle: bool) -> Vec<u8> {
    let s = if shuffle {
        bit_shuffle(bytes)
    } else {
        bytes.to_vec()
    };
    let z4 = zero_eliminate(&s, 4);
    zero_eliminate(&z4, 1)
}

/// 单帧探针结果。
struct FrameStat {
    zero_rate: f64,
    raw: usize,
    zere: usize,
    shuffle_zere: usize,
    crf: usize,
}

/// 运行探针。`root` 为 test/png 根目录（含多个图像组子目录）或单个组目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();
    // 支持 root 直接指向单个图像组（≥2 帧）。
    if load_frames(root).map(|f| f.len() >= 2).unwrap_or(false) {
        groups = vec![std::path::PathBuf::from(root)];
    }

    let limit = group_frame_limit();
    println!("=== 位洗牌 + 零消除掩码（LICO）收益探针 ===");
    println!("root: {root}");
    println!(
        "每组帧数上限: {}",
        if limit == 0 {
            "不限".to_string()
        } else {
            limit.to_string()
        }
    );
    println!("口径: TCMS(zigzag)+小端序列化 → [BIT_1 位洗牌] → ZERE_4 → ZERE_1\n");

    let mut all: Vec<FrameStat> = Vec::new();

    for dir in &groups {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        if limit > 0 && frames.len() > limit {
            frames.truncate(limit);
        }
        let n = frames.len();
        let components = frames[0].color_format.component_count();
        if components != 3 {
            continue;
        }
        let golden = &frames[0].pixels;
        if frames.iter().any(|f| f.pixels.len() != golden.len()) {
            continue;
        }

        let stats: Vec<FrameStat> = (1..n)
            .into_par_iter()
            .map(|i| -> Result<FrameStat, String> {
                let frame = &frames[i];
                // 路径 G 差分帧：RGB 域 diff(frame − golden) 后 RCT。
                let mut diff_rgb = vec![0i32; golden.len()];
                crate::crf::backend::ops::sub_i32(&frame.pixels, golden, &mut diff_rgb);
                let residuals =
                    rct::rct_forward(&diff_rgb, components).map_err(|e| e.to_string())?;

                let zero = residuals.iter().filter(|&&v| v == 0).count();
                let zero_rate = zero as f64 / residuals.len().max(1) as f64;

                let serialized = serialize_tcms_le(&residuals);
                let raw = serialized.len();
                let zere = lico_transform(&serialized, false).len();
                let shuffle_zere = lico_transform(&serialized, true).len();

                let eff = ImageData {
                    width: frame.width,
                    height: frame.height,
                    bit_depth: frame.bit_depth,
                    color_format: frame.color_format,
                    pixels: residuals,
                };
                let out = encode_frame_adaptive(
                    &eff,
                    CompressionType::GolombRice,
                    8,
                    false,
                    FrameQuant::lossless(),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;

                Ok(FrameStat {
                    zero_rate,
                    raw,
                    zere,
                    shuffle_zere,
                    crf: out.data.len(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let s = |f: fn(&FrameStat) -> usize| stats.iter().map(f).sum::<usize>();
        let (raw, zere, shuf, crf) = (
            s(|x| x.raw),
            s(|x| x.zere),
            s(|x| x.shuffle_zere),
            s(|x| x.crf),
        );
        let zr = stats.iter().map(|x| x.zero_rate).sum::<f64>() / stats.len().max(1) as f64;
        let pct = |a: usize, b: usize| -> f64 {
            if b == 0 {
                0.0
            } else {
                (a as f64 - b as f64) / b as f64 * 100.0
            }
        };
        println!(
            "组 {:<12} ({} 帧, 零值率 {:.1}%): 原始 {} | ZERE {} ({:+.1}%) | 位洗牌+ZERE {} ({:+.1}% vs ZERE) | CRF {} ({:+.1}% vs ZERE)",
            name,
            n - 1,
            zr * 100.0,
            raw,
            zere,
            pct(zere, raw),
            shuf,
            pct(shuf, zere),
            crf,
            pct(crf, zere),
        );

        all.extend(stats);
    }

    println!("\n=== 汇总 ===");
    if all.is_empty() {
        println!("(无有效组：需 ≥2 帧且 RGB)");
        return Ok(());
    }
    let sum = |f: fn(&FrameStat) -> usize| all.iter().map(f).sum::<usize>();
    let (raw, zere, shuf, crf) = (
        sum(|x| x.raw),
        sum(|x| x.zere),
        sum(|x| x.shuffle_zere),
        sum(|x| x.crf),
    );
    let zr = all.iter().map(|x| x.zero_rate).sum::<f64>() / all.len() as f64;
    let pct = |a: usize, b: usize| -> f64 {
        if b == 0 {
            0.0
        } else {
            (a as f64 - b as f64) / b as f64 * 100.0
        }
    };
    println!("帧数 {}，平均零值率 {:.1}%", all.len(), zr * 100.0);
    println!("原始字节: {raw}");
    println!("ZERE:        {zere}  ({:+.1}% vs 原始)", pct(zere, raw));
    println!("位洗牌+ZERE: {shuf}  ({:+.1}% vs ZERE)", pct(shuf, zere));
    println!(
        "CRF 现有编码: {crf}  ({:+.1}% vs 位洗牌+ZERE)",
        pct(crf, shuf)
    );

    println!("\n判定：");
    let shuffle_gain = pct(shuf, zere);
    println!(
        "  位洗牌增益（位洗牌+ZERE vs ZERE）: {:+.2}% —— {}",
        shuffle_gain,
        if shuffle_gain <= -3.0 {
            "位洗牌有效（≥3%）"
        } else {
            "位洗牌无显著增益（<3%）"
        }
    );
    let vs_crf = pct(shuf, crf);
    println!(
        "  LICO 变换 vs CRF 现有编码: {:+.2}% —— {}",
        vs_crf,
        if vs_crf < 0.0 {
            "LICO 变换优于 CRF 现有编码"
        } else {
            "LICO 变换不如 CRF 现有编码（预期：CRF 含强熵编码）"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcms_roundtrip_small_values() {
        for v in [-3, -2, -1, 0, 1, 2, 3] {
            let z = zigzag(v);
            // 逆 zigzag：(z>>1) ^ -(z&1)
            let back = ((z >> 1) as i32) ^ -((z & 1) as i32);
            assert_eq!(back, v);
        }
    }

    #[test]
    fn bit_shuffle_is_self_inverse_on_one_group() {
        let input: Vec<u8> = vec![0b1010_0101, 0b0000_1111, 1, 2, 3, 4, 5, 0x80];
        let once = bit_shuffle(&input);
        let twice = bit_shuffle(&once);
        assert_eq!(twice, input);
    }

    #[test]
    fn zero_eliminate_preserves_nonzero_words() {
        // 3 个 u32：0, 0x11223344, 0
        let mut input = vec![0u8; 12];
        input[4..8].copy_from_slice(&[0x44, 0x33, 0x22, 0x11]);
        let out = zero_eliminate(&input, 4);
        // 非零 word 保留在数据区前 4 字节
        assert_eq!(&out[0..4], &[0x44, 0x33, 0x22, 0x11]);
        // bitmap 第 1 位置位（第 1 个 word 非零）
        assert_ne!(out[4] & 0b10, 0);
        assert_eq!(out[4] & 0b01, 0);
    }

    #[test]
    fn zero_eliminate_shrinks_all_zero_block() {
        let input = vec![0u8; 4096];
        let out = zero_eliminate(&input, 4);
        assert!(out.len() < input.len());
    }
}
