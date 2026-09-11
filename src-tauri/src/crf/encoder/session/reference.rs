//! 编码器参考状态 —— golden/previous/anchor 重建参考
//!
//! 规划文档 §4.2 / §7.4。`ReferenceState` 只保存重建帧，禁止存储
//! 对后续解码不可得的原始 frame。首帧有损时必须先完成 `golden`；
//! 后续帧可以并行，但不能绕过该阶段依赖。
//!
//! **迁移状态（P3）**：定义类型，当前 `sequence.rs` 内联管理参考状态。
//! P3.b 阶段将 `encode_sequence` 中的 G_hat 管理迁移到本模块。

// P3.b 预留参考状态类型，暂无调用方；契约冻结期允许 dead_code。
#![allow(dead_code)]

use crate::crf::core::domain::ImageData;

/// 编码器参考状态
///
/// 管理 golden/previous/anchor 重建帧的生命周期。
/// 编码端本地重建后保存到此，供后续帧差分参考。
#[derive(Debug, Default)]
pub struct EncodeReferenceState {
    /// golden 重建帧（首帧编码后本地重建）
    pub golden: Option<ImageData>,
    /// 前一帧重建帧（链式参考使用）
    pub previous: Option<ImageData>,
}

impl EncodeReferenceState {
    /// 创建空的参考状态
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 golden 重建帧
    pub fn set_golden(&mut self, frame: ImageData) {
        self.golden = Some(frame);
    }

    /// 获取 golden 重建帧的像素引用
    pub fn golden_pixels(&self) -> Option<&[i32]> {
        self.golden.as_ref().map(|f| f.pixels.as_slice())
    }
}
