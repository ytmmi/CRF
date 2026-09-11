//! planar 三子平面「外层并行 + 内层禁用并行」收益探针
//!
//! 回答 [performance-optimization-plan.md §P1c] 遗留待办与
//! [optimization-review.md §36] 的两个未决问题：
//! 1. 内层禁用并行的真实加权减速比（§36 仅凭 P1c 三点外推，需实测）；
//! 2. 去掉 preferred_sub 历史引导后的字节影响（有损路径风险）。
//!
//! 方案：把 planar 的 3 个子平面（Y/Co/Cg）视为独立编码单元，
//! - A1 = 当前生产语义：串行 + preferred_sub 链 + 内层 rayon 并行；
//! - A2 = 串行 + 无引导 + 内层并行（仅测引导的字节影响）；
//! - B  = 目标方案：3 路并行 + 每个子平面用独立单线程 rayon 池禁用内层并行；
//! 对比 A1/B 的耗时（收益）与字节（是否透明）。
//!
//! 生产路径字节对拍（`CRF_PLANAR_NO_PREF` 钩子，planar.rs）：对有损 q90/q75
//! 用真实 `encode_sequence` 批量编码，比较默认 vs 去引导的完整文件字节。
//!
//! 内存峰值：独立进程模式 `CRF_PLANAR_PROBE_MODE=serial|parallel` 只跑对应
//! 调度，外部两次运行比较进程峰值工作集（Windows psapi）。
//!
//! 简化说明：调度对比直接对 RCT 后的 Y/Co/Cg 平面编码，未复现 planar 的 CfL
//! 与色度下采样（并行前置/预处理，不改变调度对比结论）。
//!
//! 不接入生产路径，仅由 `--probe-planar-parallel <dir>` CLI 分派。

use std::time::{Duration, Instant};

use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use crate::crf::core::color::rct;
use crate::crf::core::domain::{
    ColorFormat, CompressionType, EncodeParams, ImageData, PredictionMode,
};
use crate::crf::encoder::frame::candidate::encode_frame_adaptive;
use crate::crf::encoder::frame::FrameQuant;
use crate::crf::performance::bench::load_frames;

const REPEAT: usize = 3;
const BLOCK: u16 = 8;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 编码单个子平面。`pool` 为 Some 时内层 rayon 走该池（单线程池 = 禁用内层并行）。
fn encode_one(
    plane: &ImageData,
    fq: FrameQuant,
    preferred: Option<PredictionMode>,
    pool: Option<&rayon::ThreadPool>,
) -> Result<(usize, Option<PredictionMode>), String> {
    let run = || {
        encode_frame_adaptive(
            plane,
            CompressionType::GolombRice,
            BLOCK,
            false,
            fq,
            preferred,
            None,
        )
        .map(|o| (o.data.len(), o.pred_mode))
        .map_err(|e| e.to_string())
    };
    match pool {
        Some(p) => p.install(run),
        None => run(),
    }
}

/// Co/Cg 子平面量化（镜像 planar.rs L180-198）。
fn chroma_fq(fq: FrameQuant) -> FrameQuant {
    if fq.chroma_step > 0 {
        FrameQuant {
            step: fq.chroma_step,
            bias: fq.chroma_bias,
            chroma_step: fq.chroma_step,
            chroma_bias: fq.chroma_bias,
            chroma_half_res: false,
            q1_matrix_scale: fq.q1_matrix_scale,
        }
    } else {
        FrameQuant {
            step: fq.step,
            bias: fq.bias,
            chroma_step: 0,
            chroma_bias: fq.chroma_bias,
            chroma_half_res: false,
            q1_matrix_scale: fq.q1_matrix_scale,
        }
    }
}

/// 进程峰值工作集（Windows psapi）；非 Windows 返回 0。
fn process_peak_working_set() -> usize {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
        }
        #[link(name = "psapi")]
        extern "system" {
            fn GetProcessMemoryInfo(
                process: *mut std::ffi::c_void,
                counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }
        let mut pmc = ProcessMemoryCounters {
            cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
        if ok != 0 {
            pmc.peak_working_set_size
        } else {
            0
        }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 生产路径批量编码（有损预设），`no_pref` 控制 `CRF_PLANAR_NO_PREF` 钩子。
fn encode_batch_prod(frames: &[ImageData], quality: u16, no_pref: bool) -> Result<Vec<u8>, String> {
    if no_pref {
        std::env::set_var("CRF_PLANAR_NO_PREF", "1");
    } else {
        std::env::remove_var("CRF_PLANAR_NO_PREF");
    }
    let lossy = crate::crf::LossyOptionsV2Builder::preset(quality * 100)
        .build()
        .map_err(|e| e.to_string())?;
    let params = EncodeParams {
        compression_type: "golomb-rice".to_string(),
        block_size: None,
        prediction_mode: PredictionMode::Med,
        adaptive_prediction: true,
        lossy: Some(lossy),
        input_original_frames: false,
        user_metadata: None,
    };
    let out = crate::crf::encoder::encode_sequence(frames, &params).map_err(|e| e.to_string())?;
    std::env::remove_var("CRF_PLANAR_NO_PREF");
    Ok(out)
}

/// 生产路径字节对拍（有损 q90/q75，默认 vs 去引导）。
fn run_prod(frames: &[ImageData], dir: &str) -> Result<(), String> {
    println!("=== 生产路径有损字节对拍（encode_sequence 批量）===");
    println!("目录: {dir}  ({} 帧)\n", frames.len());
    for q in [90u16, 75u16] {
        let pref = encode_batch_prod(frames, q, false)?;
        let nopref = encode_batch_prod(frames, q, true)?;
        let delta = pref.len() as i64 - nopref.len() as i64;
        println!(
            "q{q}: 默认={} B   NO_PREF={} B   差={:+}  ({})",
            pref.len(),
            nopref.len(),
            delta,
            if pref == nopref {
                "字节透明"
            } else {
                "有差异 ⚠"
            }
        );
    }
    Ok(())
}

/// 运行探针。`dir` 为图像组目录。
///
/// 环境变量 `CRF_PLANAR_PROBE_MODE`：`both`(默认) / `serial` / `parallel` / `prod`。
pub fn run(dir: &str) -> Result<(), String> {
    let mode = std::env::var("CRF_PLANAR_PROBE_MODE").unwrap_or_else(|_| "both".to_string());
    let frames = load_frames(dir)?;
    if frames.is_empty() {
        return Err(format!("{dir}: no images"));
    }
    if mode == "prod" {
        return run_prod(&frames, dir);
    }
    let frame = &frames[0];
    if frame.color_format != ColorFormat::Rgb {
        return Err(format!("{dir}: 首帧非 RGB"));
    }
    let ycocg = rct::rct_forward(&frame.pixels, 3).map_err(|e| e.to_string())?;
    let w = frame.width;
    let h = frame.height;

    let mut planes: [Vec<i32>; 3] =
        std::array::from_fn(|_| Vec::with_capacity(w as usize * h as usize));
    for px in ycocg.chunks_exact(3) {
        planes[0].push(px[0]);
        planes[1].push(px[1]);
        planes[2].push(px[2]);
    }
    let plane_imgs: Vec<ImageData> = planes
        .into_iter()
        .map(|p| ImageData {
            width: w,
            height: h,
            bit_depth: frame.bit_depth,
            color_format: ColorFormat::Gray,
            pixels: p,
        })
        .collect();

    // 每个外层任务一个独立单线程池：3 路并行、内层各自串行。
    let pools: Vec<rayon::ThreadPool> = (0..3)
        .map(|_| {
            ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;

    let run_serial = mode != "parallel";
    let run_parallel = mode != "serial";

    println!("=== planar sub-plane parallel probe ===");
    println!("group: {dir}  首帧 {w}x{h}  repeat={REPEAT}  mode={mode}\n");

    let configs: [(&str, FrameQuant); 2] = [
        ("lossless", FrameQuant::lossless()),
        (
            "lossy-q90-like",
            FrameQuant {
                step: 2,
                bias: 4,
                chroma_step: 3,
                chroma_bias: -4,
                chroma_half_res: false,
                q1_matrix_scale: false,
            },
        ),
    ];
    let sub_names = ["Y", "Co", "Cg"];

    for (label, fq) in configs {
        let fq_c = chroma_fq(fq);
        let fq_for = |pi: usize| if pi == 0 { fq } else { fq_c };

        // 预热（填充分配器/线程池）
        for (pi, img) in plane_imgs.iter().enumerate() {
            let _ = encode_one(img, fq_for(pi), None, None)?;
            let _ = encode_one(img, fq_for(pi), None, Some(&pools[pi]))?;
        }

        let mut a1_bytes = 0usize;
        let mut a1_total = Duration::ZERO;
        let mut a2_bytes = 0usize;
        let mut par_times: Vec<Duration> = Vec::new();
        let mut ser_times: Vec<Duration> = Vec::new();
        let mut b_wall = Duration::ZERO;
        let mut b_bytes = 0usize;

        if run_serial {
            // A1：串行 + preferred 链 + 内层并行
            let mut a1_times = Vec::new();
            for _ in 0..REPEAT {
                let t0 = Instant::now();
                let mut preferred: Option<PredictionMode> = None;
                let mut bytes = 0usize;
                for (pi, img) in plane_imgs.iter().enumerate() {
                    let (b, pm) = encode_one(img, fq_for(pi), preferred, None)?;
                    bytes += b;
                    preferred = pm;
                }
                a1_times.push(t0.elapsed());
                a1_bytes = bytes;
            }
            a1_total = median(a1_times);

            // A2：串行 + 无引导 + 内层并行（仅测引导字节影响）
            for _ in 0..REPEAT {
                let mut bytes = 0usize;
                for (pi, img) in plane_imgs.iter().enumerate() {
                    let (b, _) = encode_one(img, fq_for(pi), None, None)?;
                    bytes += b;
                }
                a2_bytes = bytes;
            }

            // 逐子平面：内层并行 vs 内层单线程
            for (pi, img) in plane_imgs.iter().enumerate() {
                let mut pv = Vec::new();
                let mut sv = Vec::new();
                for _ in 0..REPEAT {
                    let t0 = Instant::now();
                    let _ = encode_one(img, fq_for(pi), None, None)?;
                    pv.push(t0.elapsed());
                    let t0 = Instant::now();
                    let _ = encode_one(img, fq_for(pi), None, Some(&pools[pi]))?;
                    sv.push(t0.elapsed());
                }
                par_times.push(median(pv));
                ser_times.push(median(sv));
            }
        }

        if run_parallel {
            // B：3 路并行 + 内层各自单线程 + 无引导
            let mut b_times = Vec::new();
            for _ in 0..REPEAT {
                let t0 = Instant::now();
                let results: Vec<(usize, Option<PredictionMode>)> = (0..3usize)
                    .into_par_iter()
                    .map(|pi| encode_one(&plane_imgs[pi], fq_for(pi), None, Some(&pools[pi])))
                    .collect::<Result<Vec<_>, String>>()?;
                b_times.push(t0.elapsed());
                b_bytes = results.iter().map(|(b, _)| *b).sum();
            }
            b_wall = median(b_times);
        }

        println!("--- {label} ---");
        if run_serial {
            println!("subplane  inner-par(ms)  inner-ser(ms)  ratio");
            for (i, (pd, sd)) in par_times.iter().zip(ser_times.iter()).enumerate() {
                let p = ms(*pd).max(0.001);
                let s = ms(*sd);
                println!(
                    "{:<8}  {:>13.1}  {:>12.1}  {:>5.2}x",
                    sub_names[i],
                    p,
                    s,
                    s / p
                );
            }
            println!(
                "A1 串行+引导+内层并行 : {:>8.1}ms  bytes {a1_bytes}",
                ms(a1_total)
            );
            println!("A2 串行+无引导+内层并行 : bytes {a2_bytes}");
        }
        if run_parallel {
            println!(
                "B  并行+内层单线程      : {:>8.1}ms  bytes {b_bytes}",
                ms(b_wall)
            );
        }
        if run_serial && run_parallel {
            let serial_est: f64 = par_times.iter().map(|d| ms(*d)).sum();
            let b_est = ser_times.iter().map(|d| ms(*d)).fold(0.0, f64::max);
            println!("   (估算 Σt_par = {serial_est:.1}ms / max t_ser = {b_est:.1}ms)");
            println!(
                "引导字节差 A1-A2        : {}  ({})",
                a1_bytes as i64 - a2_bytes as i64,
                if a1_bytes == a2_bytes {
                    "字节透明"
                } else {
                    "有差异 ⚠"
                }
            );
            println!(
                "调度字节透明 B==A2      : {}",
                if b_bytes == a2_bytes {
                    "是"
                } else {
                    "否 ⚠"
                }
            );
            let gain = 1.0 - ms(b_wall) / ms(a1_total).max(1e-9);
            println!("planar 阶段预估收益      : {:+.1}%", gain * 100.0);
        }
        println!();
    }

    let peak = process_peak_working_set();
    if peak > 0 {
        println!(
            "进程峰值工作集: {:.1} MB (mode={mode})",
            peak as f64 / 1048576.0
        );
    }

    if mode == "both" {
        println!();
        run_prod(&frames, dir)?;
    }
    Ok(())
}
