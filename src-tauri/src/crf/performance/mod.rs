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
/// probe_avif_target：AVIF CQ18 对标标定探针（Q 档扫描 + PSNR/SSIM/最差帧 + 字节）
pub mod probe_avif_target;
/// probe_banded_alt：banded 条带高度自适应（32 vs 64 行）胜出率探针
pub mod probe_banded_alt;
/// probe_bit_shuffle：位洗牌 + 零消除掩码（LICO, DCC 2024）收益探针——
/// TCMS+序列化 → BIT_1 位平面转置 → ZERE_4/ZERE_1 零消除（不写码流）
pub mod probe_bit_shuffle;
/// probe_dct_simd：DCT i16 打包 SIMD 可行性探针（§56 新候选）
pub mod probe_dct_simd;
/// probe_delta_palette：JPEG-XL 式 delta palette（§8.4）收益探针——像素级色数
/// + delta 条目编码 + 索引流竞争，与当前最优候选逐帧对比（不写码流）
pub mod probe_delta_palette;
/// probe_error_separation：首帧误差分离(§9 建议 3/10,JPEG2000 嵌入式)收益探针
pub mod probe_error_separation;
/// probe_first_frame：首帧内部候选阶段 profile 探针
pub mod probe_first_frame;
/// probe_first_frame_bypass：首帧 RCT 双路竞争的 RGB 直通胜出率 + G 零值特征探针
pub mod probe_first_frame_bypass;
/// probe_gpu_kernel：GPU kernel 端到端加速比探针（性能规划 P3 CPU/CUDA 对拍）
pub mod probe_gpu_kernel;
/// probe_group_bench：指定图像组的 adaptive/q90 压缩时间与体积探针（streaming）
pub mod probe_group_bench;
/// probe_lambda：P4.6 前置验证——RDOQ Trellis λ 敏感性扫描探针（S1）
pub mod probe_lambda;
/// probe_lic：LIC 整帧乘加照明补偿收益探针（§9 建议 6 采纳探索前置验证）
pub mod probe_lic;
/// probe_lic_ab：LIC A/B 字节对比探针（v1.16 正式实现——on vs
/// CRF_DISABLE_LIC=1 的实际编码字节差，§49 有效性验证工具）
pub mod probe_lic_ab;
/// probe_ma_depth：MA 树深度/上下文密度收益探针（P0-A）——扫描深度 3..8 的
/// 幅值桶条件熵，量化"加深上下文树"的收益上限
pub mod probe_ma_depth;
/// probe_ma_depth_ab：MA 树深度 A/B 真实编码字节探针——按生产口径实测
/// depth-3 基线 vs 更深深度的整帧竞争字节（含树头开销）
pub mod probe_ma_depth_ab;
/// probe_ma_train_ab：MA 树训练参数（min_gain/候选阈值）A/B 真实字节探针
/// （P0-A 第二杠杆）——Stage 1 树头+条件熵代理筛选 + Stage 2 真实字节确认
pub mod probe_ma_train_ab;
/// probe_ma_tree：MA 树叶数分布与直方图共享(§8.2)可行性探针
pub mod probe_ma_tree;
/// probe_monotonicity：DAT.1 跨内容单调性验收探针
pub mod probe_monotonicity;
/// probe_palette：palette 色数分布（分量级/像素级 + RCT 残差域可行性）探针
pub mod probe_palette;
/// probe_palette_dither：JPEG-XL 式抖动调色板收益探针（§56 低优先候选）
pub mod probe_palette_dither;
/// probe_palette_mtf：调色板排序 + MTF 编码收益探针（§56 新候选）
pub mod probe_palette_mtf;
/// probe_planar：planar 剪枝信号（色度平坦度）的 profile 验证探针
pub mod probe_planar;
/// probe_planar_band_mode：planar 子平面条带级模式切换收益探针（§9 建议 9）
pub mod probe_planar_band_mode;
/// probe_planar_candidate：planar 子平面候选级 Fast-Fail 空间探针（§56 新候选）
pub mod probe_planar_candidate;
/// probe_planar_parallel：planar 三子平面「外层并行 + 内层禁用并行」收益探针
/// （§P1c 遗留 + optimization-review §36：内层减速比实测 + preferred_sub 字节影响）
pub mod probe_planar_parallel;
/// probe_planar_sub：planar 子平面次级候选胜出频率探针（profile 验证）
pub mod probe_planar_sub;
/// probe_ref_graph：参考结构图（MST/深度约束森林）收益探针——三方案
/// （星型 golden / 时间链 previous / MST）SAD + 实际编码字节对比（不写码流）
pub mod probe_ref_graph;
/// probe_rest_frames：差分帧候选 profile 探针（胜出率 + 阶段耗时分离）
pub mod probe_rest_frames;
/// probe_ringing：P4.7 前置验证——ringing 信号（Laplacian 高响应）与
/// edge 分类的独立性探针（S0）
pub mod probe_ringing;
/// probe_self_correcting：JXL Modular 自校正/加权预测器（weighted::State）
/// 收益探针——4 子预测器 + 历史误差加权，对比 MED/Paeth 残差熵编码字节
pub mod probe_self_correcting;
/// probe_squeeze：JXL Modular Squeeze 类 Haar 可逆小波收益探针——多级
/// 子带分解（LL/LH/HL/HH）后各子带独立熵编码，对比直接编码
pub mod probe_squeeze;
/// probe_squeeze_adaptive：Squeeze + CRF adaptive 组合探针——验证小波子带
/// 接强编码器的净价值（A 交织 / B 分离分量 / C squeeze 子带）
pub mod probe_squeeze_adaptive;
/// probe_valid_set：扩展验证集质量趋势探针（30 张分层首帧，§6.1）
pub mod probe_valid_set;
pub mod telemetry;
