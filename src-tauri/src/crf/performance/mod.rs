//! 性能可观测性层（规划文档 §3.2 `performance/`）
//!
//! 当前为 P0 阶段：只提供阶段计时与端到端基准，不改码流、不改变默认后端。
//! 后续 P3~P6 再接入 capability / scheduler / memory 等后端调度能力。
//!
//! - [`telemetry`]：阶段计时采样与报告（RAII 守卫，未启用时零开销）；
//! - [`bench`]：端到端编解码基准（`--bench` CLI 分派）。

pub mod bench;
/// coeff_ctx_probe：P3 前置验证——CoeffCABAC 方向扫描 / 邻块上下文
/// 深化在合成内容上的系数编码字节收益探针（零外部数据集）
pub mod coeff_ctx_probe;
/// probe_activity：P4.2/P4.3/P4.4 activity masking 三旋钮标定探针
/// （DAT.1 单旋钮边际扫描，体积/PSNR 对照）
pub mod probe_activity;
/// probe_banded_alt：banded 条带高度自适应（32 vs 64 行）胜出率探针
pub mod probe_banded_alt;
/// probe_first_frame：首帧内部候选阶段 profile 探针
pub mod probe_first_frame;
/// probe_first_frame_bypass：首帧 RCT 双路竞争的 RGB 直通胜出率 + G 零值特征探针
pub mod probe_first_frame_bypass;
/// probe_lambda：P4.6 前置验证——RDOQ Trellis λ 敏感性扫描探针（S1）
pub mod probe_lambda;
/// probe_lic：LIC 整帧乘加照明补偿收益探针（§9 建议 6 采纳探索前置验证）
pub mod probe_lic;
/// probe_ma_tree：MA 树叶数分布与直方图共享(§8.2)可行性探针
pub mod probe_ma_tree;
/// probe_error_separation：首帧误差分离(§9 建议 3/10,JPEG2000 嵌入式)收益探针
pub mod probe_error_separation;
/// probe_monotonicity：DAT.1 跨内容单调性验收探针
pub mod probe_monotonicity;
/// probe_planar：planar 剪枝信号（色度平坦度）的 profile 验证探针
pub mod probe_planar;
/// probe_planar_sub：planar 子平面次级候选胜出频率探针（profile 验证）
pub mod probe_planar_sub;
/// probe_palette：palette 色数分布（分量级/像素级 + RCT 残差域可行性）探针
pub mod probe_palette;
/// probe_ringing：P4.7 前置验证——ringing 信号（Laplacian 高响应）与
/// edge 分类的独立性探针（S0）
pub mod probe_ringing;
/// probe_rest_frames：差分帧候选 profile 探针（胜出率 + 阶段耗时分离）
pub mod probe_rest_frames;
pub mod telemetry;
