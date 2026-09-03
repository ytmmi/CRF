//! CRF 编码器
//!
//! 模块布局：
//! - [`session`]：序列级主流程与辅助函数（P3 架构迁移，提取自 sequence.rs）
//! - [`frame`]：帧管线层（P3.c 拆入：单帧入口、候选竞争、具体候选）
//! - [`sequence`]：序列级主流程（encode_sequence：头构建/RCT/golden 差分/文件组装）
//! - [`dct_path`]：DCT 变换域量化候选（frame_type=6；感知矩阵/正变换已迁 core/transform）
//! - [`planar`] / [`banded`]：三平面打包（3）/ 条带级自适应（2）
//! - [`golomb`] / [`rle_golomb`] / [`exp_golomb`] / [`rle_cabac`]：熵编码器族

pub(crate) mod banded;
pub mod coeff_cabac;
#[allow(dead_code)] // P3 定长位流参考实现（已被 coeff_cabac 替代，保留对拍）
pub mod coeff_coder;
pub(crate) mod dct_path;
pub mod exp_golomb;
pub mod frame;
pub mod golomb;
#[allow(dead_code)] // P2 实验探针：评估期不接入正式码流（§7.3）；收益确认后格式化并移除本豁免
pub(crate) mod intra_probe;
pub mod intra_transform;
pub(crate) mod planar;
pub mod rle_cabac;
pub mod rle_golomb;
pub(crate) mod scratch;
pub mod sequence;
pub mod sequence_tools;
/// session：序列级 session 与辅助函数（P3 架构迁移）
pub mod session;
pub mod streaming;
pub mod transform;

#[cfg(test)]
mod chroma_tests;
#[cfg(test)]
mod closed_loop_tests;
#[cfg(test)]
mod integration_test;
#[cfg(test)]
mod intra_transform_tests;
#[cfg(test)]
mod streaming_tests;
#[cfg(test)]
mod tests;

// ===== 公共 API（保持与拆分前一致的对外路径）=====
pub use frame::encode_frame;
pub use sequence::encode_sequence;

pub(crate) use frame::{assemble_frame, FrameQuant};
