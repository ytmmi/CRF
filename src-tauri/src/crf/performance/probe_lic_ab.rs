//! LIC A/B 字节对比探针：LIC on vs CRF_DISABLE_LIC=1 的实际编码字节差。
//!
//! §49（LIC 正式实现）验证工具：对每组用**完全相同的参数**分别编码
//! k两次（LIC 开启 / 关闭），输出组级字节差、LIC 启用帧数，并给出
//! 「光照/时间渐变场景组」与「插画差分组」的分层汇总——LIC 有效性以
//! 场景组为口径（整帧乘加模型前提），插画差分组应以零/负收益且单调
//! 不劣化（on ≤ off）呈现。
//!
//! 环境变量注入在进程内**串行**执行两遍（先 off 后 on），探针非并行
//! 测试，不存在跨用例污染。

use crate::crf::core::bitstream::constants::{FRAME_HEADER_SIZE, LIC_A_NUM_OFFSET};
use crate::crf::core::domain::{ColorFormat, EncodeParams, ImageData, PredictionMode};
use crate::crf::encoder::sequence::encode_sequence;
use crate::crf::error::CrfResult;
use crate::crf::performance::bench::load_frames;

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

/// 解析每帧 LIC 启用字段数（lic_a_num != 0）。
fn count_lic_frames(data: &[u8], frame_count: usize) -> usize {
    use crate::crf::core::bitstream::constants::HEADER_SIZE;
    let frames_start = HEADER_SIZE + frame_count * 8;
    let mut off = frames_start;
    let mut hit = 0usize;
    for _ in 0..frame_count {
        let fs = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        if data[off + LIC_A_NUM_OFFSET] != 0 {
            hit += 1;
        }
        off += FRAME_HEADER_SIZE + fs;
    }
    hit
}

/// 对单组做 A/B：返回 (LIC on 字节, off 字节, on 启用 LIC 帧数)。
fn ab_encode(frames: &[ImageData]) -> CrfResult<(usize, usize, usize)> {
    let params = mk_params();
    // 先 off（注入环境变量），编码后立即清除——串行两遍，无并发污染。
    // 注意：清除动作必须无条件执行（off 编码失败时也要移除，否则残留
    // CRF_DISABLE_LIC=1 会污染后续所有组的 on 编码——真实全量曾因此
    // 表现为全组 0 启用）。
    unsafe {
        std::env::set_var("CRF_DISABLE_LIC", "1");
    }
    let off_result = encode_sequence(frames, &params);
    unsafe {
        std::env::remove_var("CRF_DISABLE_LIC");
    }
    let off = off_result?;
    let on = encode_sequence(frames, &params)?;
    let lic_frames = count_lic_frames(&on, frames.len());
    Ok((on.len(), off.len(), lic_frames))
}

/// 诊断：打印每组差分帧的 search_lic 预筛命中（仅 worthwhile，即采样
/// SAD 收益超门槛）——与最终字节竞争结果对照，揭示 RGB 域采样收益 vs
/// RCT 域编码字节可能的脱节。
#[allow(dead_code)]
fn diag_search(frames: &[ImageData], name: &str) {
    let golden = &frames[0].pixels;
    for (i, f) in frames.iter().enumerate().skip(1).take(3) {
        if let Some(fit) = crate::crf::core::illumination::search_lic(golden, &f.pixels) {
            if fit.worthwhile() {
                println!(
                    "    [diag] {name} 帧{i}: a={} b={} baseSad={} licSad={} drop={:.1}% ",
                    fit.a_num,
                    fit.b,
                    fit.base_sad,
                    fit.lic_sad,
                    fit.drop_percent(),
                );
            }
        }
    }
}

/// 编码级诊断：对场景组逐帧对比「golden 差分编码字节」与「LIC 差分编码字节」
/// （RCT 域、同一自适应管线），直接验证字节竞争的结果。
#[allow(dead_code)]
fn diag_encode_compare(frames: &[ImageData], name: &str) {
    use crate::crf::core::color::rct::rct_forward;
    use crate::crf::core::domain::CompressionType;
    use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
    use crate::crf::encoder::FrameQuant;

    let golden = &frames[0].pixels;
    let comp = CompressionType::GolombRice;
    for (i, f) in frames.iter().enumerate().skip(1).take(3) {
        let Some(fit) = crate::crf::core::illumination::search_lic(golden, &f.pixels) else {
            continue;
        };
        if fit.lic_sad >= fit.base_sad {
            continue;
        }
        // golden 差分（RGB → RCT → 编码）
        let mut dg = vec![0i32; f.pixels.len()];
        crate::crf::backend::ops::sub_i32(&f.pixels, golden, &mut dg);
        let rg = rct_forward(&dg, 3).unwrap_or_else(|_| dg.clone());
        let img_g = ImageData {
            width: f.width,
            height: f.height,
            bit_depth: f.bit_depth,
            color_format: f.color_format,
            pixels: rg,
        };
        let bytes_g =
            encode_frame_adaptive(&img_g, comp, 8, false, FrameQuant::lossless(), None, None)
                .map(|o| o.data.len())
                .unwrap_or(0);
        // LIC 差分
        let mut lic_ref = vec![0i32; golden.len()];
        crate::crf::core::illumination::fit_into(golden, fit.a_num, fit.b, &mut lic_ref);
        let mut dl = vec![0i32; f.pixels.len()];
        crate::crf::backend::ops::sub_i32(&f.pixels, &lic_ref, &mut dl);
        let rl = rct_forward(&dl, 3).unwrap_or_else(|_| dl.clone());
        let img_l = ImageData {
            width: f.width,
            height: f.height,
            bit_depth: f.bit_depth,
            color_format: f.color_format,
            pixels: rl,
        };
        let bytes_l =
            encode_frame_adaptive(&img_l, comp, 8, false, FrameQuant::lossless(), None, None)
                .map(|o| o.data.len())
                .unwrap_or(0);
        println!(
            "    [enc] {name} 帧{i}: golden={bytes_g}B LIC={bytes_l}B (a={} b={}) LIC更小={}",
            fit.a_num,
            fit.b,
            bytes_l < bytes_g
        );
    }
}

/// 组名是否属于「光照/时间渐变场景」语义分组（整帧乘加模型前提）。
fn is_scene_group(name: &str) -> bool {
    name.contains("纯场景") || name.contains("场景差分") || name.contains("light")
        // v1.16 合成光照渐变序列（test/lic-synthetic，§49 字节有效性数据集）
        || matches!(name, "sunset" | "dawn" | "night" | "cloudy" | "indoor" | "sky")
}

/// 运行 A/B 探针。`root` 为 test/png 根目录。
pub fn run(root: &str) -> Result<(), String> {
    let mut groups: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| format!("{root}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    groups.sort();

    println!("=== LIC A/B 字节对比（on vs CRF_DISABLE_LIC=1）===");
    println!("root: {root}\n");
    println!(
        "{:<16} {:>12} {:>12} {:>9} {:>8}",
        "组", "LICon字节", "LICoff字节", "差幅%", "LIC帧/差分帧"
    );

    let mut scene_on = 0usize;
    let mut scene_off = 0usize;
    let mut all_on = 0usize;
    let mut all_off = 0usize;
    let mut scene_frames = 0usize;
    let mut scene_lic_hits = 0usize;
    let mut all_frames = 0usize;
    let mut all_lic_hits = 0usize;
    let mut worse_groups = 0usize;

    for dir in &groups {
        let frames = match load_frames(&dir.to_string_lossy()) {
            Ok(f) if f.len() >= 2 => f,
            _ => continue,
        };
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        diag_search(&frames, &name);
        if is_scene_group(&name) {
            diag_encode_compare(&frames, &name);
        }
        let (on, off, lic_frames) = match ab_encode(&frames) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{name}: 编码失败 {e}");
                continue;
            }
        };
        let pct = if off > 0 {
            (on as isize - off as isize) as f64 / off as f64 * 100.0
        } else {
            0.0
        };
        if on > off {
            worse_groups += 1;
        }
        let diff_frames = frames.len() - 1;
        println!("{name:<16} {on:>12} {off:>12} {pct:>8.2}% {lic_frames:>4}/{diff_frames}");
        all_on += on;
        all_off += off;
        all_frames += diff_frames;
        all_lic_hits += lic_frames;
        if is_scene_group(&name) {
            scene_on += on;
            scene_off += off;
            scene_frames += diff_frames;
            scene_lic_hits += lic_frames;
        }
    }

    println!("\n--- 汇总 ---");
    let fmt_pct = |on: usize, off: usize| -> String {
        if off > 0 {
            format!(
                "{:.2}%",
                (on as isize - off as isize) as f64 / off as f64 * 100.0
            )
        } else {
            "—".to_string()
        }
    };
    println!(
        "全组: {all_on} vs {all_off} = {}  差幅帧 {all_lic_hits}/{all_frames}  劣化组数 {worse_groups}",
        fmt_pct(all_on, all_off)
    );
    if scene_frames > 0 {
        println!(
            "场景组(光照/时间渐变): {scene_on} vs {scene_off} = {}  LIC 启用 {scene_lic_hits}/{scene_frames}",
            fmt_pct(scene_on, scene_off)
        );
    } else {
        println!("场景组: (当前 root 未含光照渐变场景组，请用 test/png 全量根目录)");
    }
    println!(
        "判定: {}",
        if worse_groups == 0 {
            "单调不劣化（on ≤ off 恒成立）"
        } else {
            "存在劣化组（需复核）"
        }
    );
    Ok(())
}

#[allow(dead_code)]
fn _color_anchor(_: &ColorFormat) {}
