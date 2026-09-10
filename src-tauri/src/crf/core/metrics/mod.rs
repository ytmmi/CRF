//! metrics：静态图客观质量指标（SSIM 等），供质量标定与验收（first-frame-optimization-plan §6.2）。
//!
//! 纯计算模块，不依赖编解码器状态；只消费 [`ImageData`](crate::crf::core::domain::ImageData)。

pub mod ssim;
