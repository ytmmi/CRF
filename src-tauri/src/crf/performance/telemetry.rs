//! 阶段计时采样与报告（P0 可观测性）
//!
//! 提供 RAII 阶段守卫 [`Span`]：进入阶段时 `Span::begin("name")`，离开时
//! 自动记录耗时。默认**不启用**（零开销：仅一次原子读取）；通过环境变量
//! `CRF_PERF=1` 或 [`enable`] 显式开启。
//!
//! 采样写入进程级 [`Mutex`] 聚合器，由 [`report`] 输出按阶段分组的
//! count / total / mean / p50 / p95。该模块**不得**影响编码决策、码流
//! 字节或默认后端（规划文档 P0 硬约束）。
//!
//! 线程说明：编码热路径含 rayon 并行段，因此采样器必须 `Send + Sync`。
//! 并行段内部的阶段守卫会并发写聚合器；本模块只在基准模式下开启，
//! 竞争代价不影响生产默认路径。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 启用标志。默认关闭；`CRF_PERF=1` 或 [`enable`] 开启后不可回退。
static ENABLED: AtomicBool = AtomicBool::new(false);
static INIT: OnceLock<()> = OnceLock::new();

/// 采样聚合器（进程级；按阶段名累积原始耗时样本）。
static SAMPLES: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());

fn ensure_init() {
    INIT.get_or_init(|| {
        let on = std::env::var_os("CRF_PERF").is_some();
        ENABLED.store(on, Ordering::Relaxed);
    });
}

/// 查询是否启用（调用前确保 init 已执行）。
#[inline]
pub fn enabled() -> bool {
    ensure_init();
    ENABLED.load(Ordering::Relaxed)
}

/// 显式启用（基准 CLI 使用；等价于 `CRF_PERF=1`）。
pub fn enable() {
    ensure_init();
    ENABLED.store(true, Ordering::Relaxed);
}

/// 记录一次已完成的阶段耗时（未启用时为空操作）。
pub fn record(name: &str, elapsed: Duration) {
    if enabled() {
        if let Ok(mut samples) = SAMPLES.lock() {
            samples.push((name.to_owned(), elapsed));
        }
    }
}

/// 清空历史采样（新一轮基准开始前调用，避免跨轮叠加）。
pub fn clear() {
    if let Ok(mut samples) = SAMPLES.lock() {
        samples.clear();
    }
}

/// RAII 阶段守卫：`Drop` 时记录耗时。
pub struct Span {
    name: Option<&'static str>,
    start: Instant,
    active: bool,
}

impl Span {
    /// 进入一个命名阶段。未启用时返回空守卫（零开销）。
    #[inline]
    pub fn begin(name: &'static str) -> Span {
        let active = enabled();
        Span {
            name: active.then_some(name),
            start: Instant::now(),
            active,
        }
    }

    /// 提前结束并记录（幂等；与 Drop 二选一）。
    #[inline]
    pub fn finish(mut self) {
        if self.active {
            if let Some(name) = self.name.take() {
                record(name, self.start.elapsed());
            }
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if self.active {
            if let Some(name) = self.name.take() {
                record(name, self.start.elapsed());
            }
        }
    }
}

/// 计算一组已排序耗时的分位数（最近邻取整法，p ∈ [0,1]）。
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    debug_assert!(!sorted.is_empty());
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn fmt_ms(d: Duration) -> String {
    format!("{:.3}ms", d.as_secs_f64() * 1000.0)
}

/// 汇总报告（按阶段分组聚合）。未启用或无采样时返回空字符串。
pub fn report() -> String {
    let samples = match SAMPLES.lock() {
        Ok(samples) if !samples.is_empty() => samples.clone(),
        _ => return String::new(),
    };

    // 按阶段名分组（BTreeMap 保证稳定输出顺序）。
    let mut groups: std::collections::BTreeMap<String, Vec<Duration>> =
        std::collections::BTreeMap::new();
    for (name, elapsed) in samples {
        groups.entry(name).or_default().push(elapsed);
    }

    let mut out = String::new();
    out.push_str("stage                          count     total       mean        p50        p95\n");
    for (name, mut durations) in groups {
        durations.sort_unstable();
        let count = durations.len();
        let total: Duration = durations.iter().sum();
        let mean = total / count as u32;
        let p50 = percentile(&durations, 0.50);
        let p95 = percentile(&durations, 0.95);
        out.push_str(&format!(
            "{:<28} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}\n",
            name,
            count,
            fmt_ms(total),
            fmt_ms(mean),
            fmt_ms(p50),
            fmt_ms(p95),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let d = |ms: u64| Duration::from_millis(ms);
        let sorted = vec![d(1), d(2), d(3), d(4), d(5)];
        assert_eq!(percentile(&sorted, 0.0), d(1));
        assert_eq!(percentile(&sorted, 0.50), d(3));
        assert_eq!(percentile(&sorted, 0.95), d(5));
        assert_eq!(percentile(&sorted, 1.0), d(5));
    }

    #[test]
    fn span_records_when_enabled() {
        enable();
        clear();
        {
            let _span = Span::begin("test.span");
            std::thread::sleep(Duration::from_millis(2));
        }
        let r = report();
        assert!(r.contains("test.span"), "report 应包含阶段名：{r}");
        clear();
    }

    #[test]
    fn finish_is_idempotent_and_single_record() {
        enable();
        clear();
        let span = Span::begin("test.finish");
        span.finish(); // 显式结束即记录一次；Drop 因 name 已 take 不会再记
        let r = report();
        // 恰好一条采样（count 列 = 1）
        let line = r.lines().find(|l| l.contains("test.finish")).unwrap();
        let count = line.split_whitespace().nth(1).unwrap();
        assert_eq!(count, "1", "finish 应恰好记录一次：{r}");
        clear();
    }
}
