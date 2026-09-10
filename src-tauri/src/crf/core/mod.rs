//! 公共核心层 —— 编解码器共享的纯契约与数学
//!
//! `core` 不知道文件路径和设备 API，只定义：
//! - domain：数据模型与不变量（image/frame/tile/coefficient）；
//! - config：请求、预设、解析与有效配置（[`contract::ResolvedConfig`]）；
//! - bitstream：格式契约与容器边界（header/index/frame/footer）；
//! - color：色彩和采样（RCT/resample/range）；
//! - prediction：预测契约（mode/intra/residual/IntraBC/cost）；
//! - transform：变换、量化和重建（DCT/矩形/量化/RDOQ/逆变换）；
//! - entropy：符号化和熵编码（token/scan/RLE/Golomb/CABAC/context）。
//!
//! 依赖方向（规划文档 §2）：
//! ```text
//! encoder/decoder
//!   ↓
//! core (prediction/transform/entropy/color)
//!   ↓
//! backend kernels
//!   ↓
//! domain + bitstream contracts
//! ```
//!
//! **迁移状态（P4）**：`contract` 定义公共契约类型（FramePacket/ReferenceState/
//! ResolvedConfig/CandidateResult）。`domain`/`config`/`bitstream`/`color`/
//! `prediction`/`transform`/`entropy` 各子模块已迁入真实实现，旧 `format/` 与
//! 顶层 `transform/` 目录已删除，无转发层。
//!
//! 参考：[编解码器分层重构规划](../../../../docs/codec-architecture-refactor-plan.md) §3

/// 公共契约类型（FramePacket / ReferenceState / ResolvedConfig / CandidateResult）
pub mod contract;

// ===== 子模块职责（P4 已迁入实现，不再为空声明）=====
// 迁移顺序遵循规划文档 §10（P1~P4 已基本完成）：
// - P1：transform（解除 decoder → encoder 反向依赖）✅；
// - P2：bitstream（拆容器层）✅；
// - P3：config / domain（session 拆分）✅；
// - P4：prediction / entropy / color（工具层整理）◐ 熵编码状态机待续。

/// domain：数据模型与不变量（image/frame/tile/coefficient）
pub mod domain;

/// config：请求、预设、解析与有效配置
pub mod config;

/// bitstream：格式契约与容器（header/index/frame/footer/边界）
pub mod bitstream;

/// color：色彩和采样（RCT/resample/range）
pub mod color;

/// prediction：预测契约（mode/intra/residual/IntraBC/cost）
pub mod prediction;

/// transform：变换、量化和重建（DCT/矩形/量化/RDOQ/逆变换）
pub mod transform;

/// entropy：符号化和熵编码（token/scan/RLE/Golomb/CABAC/context）
pub mod entropy;

/// perceptual：噪声感知估计与软阈值（P4 迁入，原 encoder/noise.rs）
pub mod perceptual;

/// illumination：LIC（局部照明补偿）帧级乘加加权参考的纯数学
/// （搜索/应用/定点常量；编解码与重建链共用，v1.16）
pub mod illumination;

/// metrics：静态图客观质量指标（SSIM 等，标定/验收用，§6.2）
pub mod metrics;
