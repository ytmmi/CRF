//! EncodeSession —— 编码会话生命周期
//!
//! 规划文档 §4.2。职责：
//! - 接收并验证输入帧；
//! - 解析并冻结 `ResolvedConfig`；
//! - 编码首帧并得到 G_hat；
//! - 建立后续帧的 reconstructed reference；
//! - 安排可并行的残差帧；
//! - 请求 `FrameEncoder` 产生帧 payload；
//! - 交给 container writer 组装文件。
//!
//! 它不实现候选预测、DCT、CABAC 或 GPU kernel。
//!
//! **迁移状态（P3）**：当前为批量输入的会话入口。`batch` 模块已承载
//! 逐帧量化配置与输出组装；`reference` 模块承载编码端参考状态类型。
//! `encode_sequence`（`sequence.rs`）仍为批量主流程实现，
//! 本会话负责配置冻结与编排，主流程逐步迁移到本模块。

use crate::crf::core::domain::ImageData;
use crate::crf::error::CrfResult;

/// 编码会话
///
/// 一次性封装编码请求的配置解析与序列编排。批量和 streaming 必须
/// 共用同一配置解析（规划文档 §3.2）。
pub struct EncodeSession;

impl EncodeSession {
    /// 批量编码序列（`encode_sequence` 的会话入口）
    ///
    /// 消费 facade 层已解析的 [`crate::crf::core::contract::ResolvedConfig`]，
    /// 直接委托主流程 [`crate::crf::encoder::sequence::encode_sequence_resolved`]，
    /// 不再重复解析 compression_type/header/RCT/`KernelLossyConfig`（P3.b 完成）。
    pub fn encode_sequence(
        frames: &[ImageData],
        resolved: &crate::crf::core::contract::ResolvedConfig,
    ) -> CrfResult<Vec<u8>> {
        crate::crf::encoder::sequence::encode_sequence_resolved(frames, resolved)
    }
}
