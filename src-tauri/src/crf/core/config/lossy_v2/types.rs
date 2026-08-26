use serde::{Deserialize, Serialize};

pub const LOSSY_API_VERSION: u16 = 2;
pub const PRESET_REVISION_CURRENT: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LossyOptionsV2 {
    #[serde(default = "api_v2")]
    pub api_version: u16,
    pub base: LossyBase,
    #[serde(default)]
    pub rate: RateControlV2,
    #[serde(default)]
    pub first_frame: FirstFrameTuning,
    #[serde(default)]
    pub quant: QuantizationTuning,
    #[serde(default)]
    pub chroma: ChromaTuning,
    #[serde(default)]
    pub perceptual: PerceptualTuning,
    #[serde(default)]
    pub temporal: TemporalTuning,
    #[serde(default)]
    pub experimental: Option<ExperimentalToolTuning>,
    #[serde(default)]
    pub performance: PerformanceTuning,
    #[serde(default)]
    pub allow_ignored: bool,
}

fn api_v2() -> u16 {
    LOSSY_API_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LossyBase {
    Preset {
        #[serde(rename = "qualityX100")]
        quality_x100: u16,
        #[serde(default)]
        revision: Option<u16>,
    },
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RateMode {
    #[default]
    ConstantQuality,
    TargetBytes,
    TargetBpp,
    ConstrainedQuality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RateControlV2 {
    pub mode: RateMode,
    pub target_bytes: Option<u64>,
    pub target_bpp_x10000: Option<u32>,
    pub max_bytes: Option<u64>,
    pub min_quality_x100: Option<u16>,
    pub rdo_lambda_scale_x1000: u16,
    pub max_frame_drop_db_x100: u16,
}
impl Default for RateControlV2 {
    fn default() -> Self {
        Self {
            mode: RateMode::ConstantQuality,
            target_bytes: None,
            target_bpp_x10000: None,
            max_bytes: None,
            min_quality_x100: None,
            rdo_lambda_scale_x1000: 1000,
            max_frame_drop_db_x100: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FirstFrameMode {
    #[default]
    MatchSequence,
    Lossless,
    QualityOffset,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FirstFrameTuning {
    pub mode: FirstFrameMode,
    pub quality_offset_x100: Option<i16>,
    pub quality_x100: Option<u16>,
    pub max_bytes: Option<u64>,
    pub rdo_lambda_scale_x1000: u16,
}
impl Default for FirstFrameTuning {
    fn default() -> Self {
        Self {
            mode: FirstFrameMode::MatchSequence,
            quality_offset_x100: None,
            quality_x100: None,
            max_bytes: None,
            rdo_lambda_scale_x1000: 1000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum QuantMode {
    #[default]
    FromQuality,
    ExplicitSteps,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum QuantMatrix {
    #[default]
    Auto,
    Flat,
    Perceptual,
    EdgePreserving,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RdoqMode {
    #[default]
    Auto,
    Off,
    Fast,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantizationTuning {
    pub mode: QuantMode,
    pub luma_step_q8: Option<u16>,
    pub chroma_step_q8: Option<u16>,
    pub luma_scale_x1000: u16,
    pub chroma_scale_x1000: u16,
    pub dc_scale_x1000: u16,
    pub high_freq_scale_x1000: u16,
    pub deadzone_luma_x256: i16,
    pub deadzone_chroma_x256: i16,
    pub matrix: QuantMatrix,
    pub rdoq: RdoqMode,
}
impl Default for QuantizationTuning {
    fn default() -> Self {
        Self {
            mode: QuantMode::FromQuality,
            luma_step_q8: None,
            chroma_step_q8: None,
            luma_scale_x1000: 1000,
            chroma_scale_x1000: 1300,
            dc_scale_x1000: 1000,
            high_freq_scale_x1000: 1000,
            deadzone_luma_x256: 16,
            deadzone_chroma_x256: -16,
            matrix: QuantMatrix::Auto,
            rdoq: RdoqMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaSampling {
    #[default]
    Auto,
    #[serde(rename = "444")]
    Cs444,
    #[serde(rename = "422")]
    Cs422,
    #[serde(rename = "420")]
    Cs420,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DownsampleFilter {
    #[default]
    Auto,
    Box,
    Bilinear,
    SixTap,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UpsampleFilter {
    #[default]
    Auto,
    Bilinear,
    FourTap,
    SixTap,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaSiting {
    #[default]
    Auto,
    Centered,
    Cosited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ChromaTuning {
    pub sampling: ChromaSampling,
    pub downsample_filter: DownsampleFilter,
    pub upsample_filter: UpsampleFilter,
    pub siting: ChromaSiting,
    pub edge_protection_x100: u16,
}
impl Default for ChromaTuning {
    fn default() -> Self {
        Self {
            sampling: ChromaSampling::Auto,
            downsample_filter: DownsampleFilter::Auto,
            upsample_filter: UpsampleFilter::Auto,
            siting: ChromaSiting::Auto,
            edge_protection_x100: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PerceptualMetric {
    Sse,
    SsimHybrid,
    #[default]
    Auto,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NoiseMode {
    Off,
    #[default]
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PerceptualTuning {
    pub activity_masking_x100: u16,
    pub flat_area_protection_x100: u16,
    pub edge_protection_x100: u16,
    pub ringing_control_x100: u16,
    pub metric: PerceptualMetric,
    pub noise_mode: NoiseMode,
    pub noise_tau_x100: Option<u16>,
}
impl Default for PerceptualTuning {
    fn default() -> Self {
        Self {
            activity_masking_x100: 100,
            flat_area_protection_x100: 100,
            edge_protection_x100: 100,
            ringing_control_x100: 100,
            metric: PerceptualMetric::Auto,
            noise_mode: NoiseMode::Auto,
            noise_tau_x100: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceModeV2 {
    #[default]
    Auto,
    Golden,
    Previous,
    Hybrid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SceneCutModeV2 {
    Off,
    #[default]
    Auto,
    Manual,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToolMode {
    Off,
    #[default]
    Auto,
    On,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MotionModeV2 {
    Off,
    #[default]
    Auto,
    Integer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalTuning {
    pub reference_mode: ReferenceModeV2,
    pub scene_cut: SceneCutModeV2,
    pub scene_cut_threshold_x1000: Option<u16>,
    pub change_mask: ToolMode,
    pub motion_mode: MotionModeV2,
    pub motion_range: Option<u8>,
    pub anchor_interval: Option<u16>,
    pub anchor_quality_offset_x100: i16,
}
impl Default for TemporalTuning {
    fn default() -> Self {
        Self {
            reference_mode: ReferenceModeV2::Auto,
            scene_cut: SceneCutModeV2::Auto,
            scene_cut_threshold_x1000: None,
            change_mask: ToolMode::Auto,
            motion_mode: MotionModeV2::Auto,
            motion_range: None,
            anchor_interval: None,
            anchor_quality_offset_x100: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperimentalToolTuning {
    pub allow_experimental: bool,
    pub tile_size: Option<u16>,
    pub transform_sizes: Vec<String>,
    pub transform_skip: ToolMode,
    pub coefficient_coding: Option<String>,
    pub palette: ToolMode,
    pub rdo_candidate_limit: u16,
}
impl Default for ExperimentalToolTuning {
    fn default() -> Self {
        Self {
            allow_experimental: false,
            tile_size: None,
            transform_sizes: Vec::new(),
            transform_skip: ToolMode::Auto,
            coefficient_coding: None,
            palette: ToolMode::Auto,
            rdo_candidate_limit: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FastFailMode {
    Off,
    #[default]
    Auto,
    On,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PerformanceTuning {
    pub effort: u8,
    pub threads: u16,
    pub memory_limit_mb: Option<u32>,
    pub deterministic: bool,
    pub fast_fail: FastFailMode,
}
impl Default for PerformanceTuning {
    fn default() -> Self {
        Self {
            effort: 7,
            threads: 0,
            memory_limit_mb: None,
            deterministic: true,
            fast_fail: FastFailMode::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWarning {
    pub code: String,
    pub field: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredField {
    pub field: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedLossyReport {
    pub api_version: u16,
    pub preset_revision: Option<u16>,
    pub effective: LossyOptionsV2,
    pub warnings: Vec<ConfigWarning>,
    pub ignored: Vec<IgnoredField>,
    pub config_fingerprint: [u8; 16],
}
