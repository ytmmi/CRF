//! format —— 公共格式层（算法实现，P4 迁移后仅保留量化/代价/自适应 k）
//!
//! 规划文档 §6.3：`types` 已迁 `core/domain`、`header` 已迁
//! `core/bitstream/header`、`LossyTuning`/`quant_step_from_quality` 已迁
//! `core/config/lossy`、预测与逆预测已迁 `core/prediction/intra`。
//! 本层仅保留剩余算法实现。

pub mod closed_loop;
pub mod cost;
pub mod k_value;
pub mod quant;

// Re-export public items（算法函数）
pub use closed_loop::closed_loop_predict_quant_banded;
pub use cost::satd_for_mode_sampled;
pub use k_value::{adaptive_k, block_adaptive_k};
pub use quant::quantize_residuals;
