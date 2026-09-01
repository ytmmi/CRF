//! 性能可观测性层（规划文档 §3.2 `performance/`）
//!
//! 当前为 P0 阶段：只提供阶段计时与端到端基准，不改码流、不改变默认后端。
//! 后续 P3~P6 再接入 capability / scheduler / memory 等后端调度能力。
//!
//! - [`telemetry`]：阶段计时采样与报告（RAII 守卫，未启用时零开销）；
//! - [`bench`]：端到端编解码基准（`--bench` CLI 分派）。

pub mod bench;
/// probe_planar：planar 剪枝信号（色度平坦度）的 profile 验证探针
pub mod probe_planar;
pub mod telemetry;
