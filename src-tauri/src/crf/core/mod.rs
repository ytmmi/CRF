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
//! **迁移状态（P0）**：仅定义公共契约类型（[`contract`] 模块）与空子模块声明。
//! P1~P4 阶段逐步将 `format/` 与 `transform/` 的实现迁移到本层各子模块。
//! 迁移期旧路径保持生产，新模块通过 re-export 保持兼容。
//!
//! 参考：[编解码器分层重构规划](../../../../docs/codec-architecture-refactor-plan.md) §3

/// 公共契约类型（FramePacket / ReferenceState / ResolvedConfig / CandidateResult）
pub mod contract;

// ===== 以下子模块 P0 阶段为空声明，P1~P4 逐步迁入实现 =====
//
// 每个子模块的 mod.rs 当前仅包含职责说明与 `#![allow(dead_code)]`，
// 不包含任何实现。迁移顺序遵循规划文档 §10：
// - P1：transform（解除 decoder → encoder 反向依赖）；
// - P2：bitstream（拆容器层）；
// - P3：config / domain（session 拆分）；
// - P4：prediction / entropy / color（工具层整理）。

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
