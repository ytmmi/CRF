//! GPU kernel 端到端加速比探针（性能优化规划 P3）
//!
//! 对现有 CUDA kernel（`diff_i32` / `rct_forward`）做 CPU vs GPU 端到端对拍：
//! GPU 路径包含完整的 device 分配 → HtoD → launch → sync → DtoH → 释放，
//! 用于回答 P3 门槛「端到端 ≥1.5×」是否达成。
//!
//! 同时验证 CPU/GPU 结果逐位一致（正确性前提）。
//!
//! 不接入生产路径，仅由 `--probe-gpu-kernel [dir]` CLI 分派（dir 忽略）。

use std::time::{Duration, Instant};

use crate::crf::backend::cpu::simd;
use crate::crf::backend::gpu::{probe_nvidia, NvidiaCudaBackend};

const REPEAT: usize = 5;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 运行探针。`_dir` 忽略（探针不依赖图像数据）。
pub fn run(_dir: &str) -> Result<(), String> {
    let dev = probe_nvidia(0).ok_or_else(|| "NVIDIA 设备不可用（nvidia-smi 失败）".to_string())?;
    println!("=== GPU kernel 端到端加速比探针（P3）===");
    println!(
        "设备: {}  驱动={:?}  显存={:?}MB  compute={:?}\n",
        dev.name,
        dev.driver_version,
        dev.memory_bytes.map(|b| b / 1024 / 1024),
        dev.compute_capability
    );
    let backend =
        NvidiaCudaBackend::new(0).ok_or_else(|| "NvidiaCudaBackend 初始化失败".to_string())?;

    // 组 1000 单帧：1024×1820×3 分量（i32 元素数）
    let (w, h) = (1024usize, 1820usize);
    let n = w * h * 3;

    // ===== diff_i32 =====
    let a: Vec<i32> = (0..n).map(|i| (i as i32 % 511) - 255).collect();
    let b: Vec<i32> = (0..n).map(|i| ((i * 7) as i32 % 511) - 255).collect();
    let mut out_cpu = vec![0i32; n];
    let mut cpu_t = Vec::new();
    for _ in 0..REPEAT {
        let t0 = Instant::now();
        simd::sub_i32(&a, &b, &mut out_cpu);
        cpu_t.push(t0.elapsed());
    }
    let mut out_gpu = vec![0i32; n];
    let mut gpu_t = Vec::new();
    for _ in 0..REPEAT {
        let t0 = Instant::now();
        out_gpu = backend.diff_i32(&a, &b).map_err(|e| format!("{e:?}"))?;
        gpu_t.push(t0.elapsed());
    }
    let cpu_p50 = ms(median(cpu_t));
    let gpu_p50 = ms(median(gpu_t));
    println!(
        "diff_i32     n={n}: CPU p50={cpu_p50:>7.2}ms  GPU p50={gpu_p50:>7.2}ms  加速比={:>5.2}x  一致={}",
        cpu_p50 / gpu_p50,
        out_cpu == out_gpu
    );

    // ===== rct_forward =====
    let base: Vec<i32> = (0..n).map(|i| i as i32 % 256).collect();
    let mut px_cpu = base.clone();
    let mut cpu_t = Vec::new();
    for _ in 0..REPEAT {
        let t0 = Instant::now();
        simd::rct_forward_interleaved(&mut px_cpu);
        cpu_t.push(t0.elapsed());
    }
    let mut px_gpu = base.clone();
    let mut gpu_t = Vec::new();
    for _ in 0..REPEAT {
        let t0 = Instant::now();
        backend
            .rct_forward(&mut px_gpu)
            .map_err(|e| format!("{e:?}"))?;
        gpu_t.push(t0.elapsed());
    }
    let cpu_p50 = ms(median(cpu_t));
    let gpu_p50 = ms(median(gpu_t));
    println!(
        "rct_forward  n={n}: CPU p50={cpu_p50:>7.2}ms  GPU p50={gpu_p50:>7.2}ms  加速比={:>5.2}x  一致={}",
        cpu_p50 / gpu_p50,
        px_cpu == px_gpu
    );

    println!("\n判定参考：GPU 端到端加速比 ≥1.5× 才达 P3 门槛（含传输/同步/分配开销）。");
    Ok(())
}
