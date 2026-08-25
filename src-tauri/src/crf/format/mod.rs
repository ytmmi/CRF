pub mod constants;
pub mod header;
pub mod k_value;
pub mod prediction;
pub(crate) use prediction::predict_at;
pub mod quant;
pub mod rct;
pub mod simd;
pub mod types;
pub mod zigzag;

// Re-export all public items for backward compatibility
pub use constants::*;
pub use header::CrfHeader;
pub use k_value::{adaptive_k, block_adaptive_k};
pub use prediction::{
    apply_prediction, apply_prediction_band, closed_loop_predict_quant_banded,
    sad_for_mode_sampled, undo_prediction, undo_prediction_range,
};
pub use quant::{quant_step_from_quality, quantize_residuals, LossyTuning};
pub use rct::{rct_applicable, rct_forward, rct_inverse};
pub use types::{
    ColorFormat, CompressionType, DecodeResult, EncodeParams, Flags, FrameHeader, FrameIndexEntry,
    ImageData, PredictionMode,
};
pub use zigzag::{zigzag_decode, zigzag_encode, zigzag_inverse, zigzag_scan};
