//! domain —— 数据模型与不变量
//!
//! 规划文档 §3.1。不包含编码决策的公共领域模型：
//! - types.rs：ColorFormat/CompressionType/PredictionMode/Flags/FrameHeader/
//!   FrameIndexEntry/ImageData/DecodeResult/EncodeParams（原 format/types.rs）
//!
//! **迁移状态（P4）**：`types` 模块已迁入（原 `format/types.rs`）。
//! 旧 `format/types.rs` 不再保留。

/// types：公共领域类型（原 format/types.rs）
pub mod types;

// 与旧 format 路径一致的 re-export（供迁移期外部引用）
pub use types::*;
