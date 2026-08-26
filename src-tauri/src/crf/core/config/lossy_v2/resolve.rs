use std::fmt;

use super::types::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveContext {
    pub components: Option<usize>,
    pub frame_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: &'static str,
    pub message: String,
}
impl ConfigError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
impl std::error::Error for ConfigError {}

impl LossyOptionsV2 {
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_for_input(ResolveContext::default())
    }

    pub fn validate_for_input(&self, ctx: ResolveContext) -> Result<(), ConfigError> {
        if self.api_version != LOSSY_API_VERSION {
            return Err(ConfigError::new(
                "apiVersion",
                format!("unsupported version {}, expected 2", self.api_version),
            ));
        }
        if let LossyBase::Preset { quality_x100, .. } = self.base {
            in_range("base.qualityX100", quality_x100, 100, 10000)?;
        }
        validate_rate(&self.rate, &self.base)?;
        validate_first_frame(&self.first_frame)?;
        validate_quant(&self.quant)?;
        validate_chroma(&self.chroma, ctx)?;
        validate_perceptual(&self.perceptual, self.allow_ignored)?;
        validate_temporal(&self.temporal, self.allow_ignored)?;
        validate_experimental(self.experimental.as_ref())?;
        if self.performance.effort > 10 {
            return Err(ConfigError::new("performance.effort", "must be in 0..=10"));
        }
        if self.performance.threads > 1024 {
            return Err(ConfigError::new(
                "performance.threads",
                "must be in 0..=1024",
            ));
        }
        Ok(())
    }

    pub fn resolve_without_encoding(&self) -> Result<ResolvedLossyReport, ConfigError> {
        self.resolve_for_input(ResolveContext::default())
    }

    pub fn resolve_for_input(
        &self,
        ctx: ResolveContext,
    ) -> Result<ResolvedLossyReport, ConfigError> {
        self.validate_for_input(ctx)?;
        let mut effective = self.clone();
        let mut warnings = Vec::new();
        let mut ignored = Vec::new();
        let preset_revision = match effective.base {
            LossyBase::Preset {
                ref mut revision, ..
            } => {
                if revision.is_none() {
                    *revision = Some(PRESET_REVISION_CURRENT);
                }
                *revision
            }
            LossyBase::Explicit => None,
        };
        if effective.rate.min_quality_x100.is_none()
            && matches!(
                effective.rate.mode,
                RateMode::TargetBytes | RateMode::TargetBpp | RateMode::ConstrainedQuality
            )
        {
            if let LossyBase::Preset { quality_x100, .. } = effective.base {
                effective.rate.min_quality_x100 = Some(quality_x100);
            }
        }
        collect_ignored(&mut effective, &mut ignored);
        resolve_auto(&mut effective, &mut warnings, ctx);
        let bytes =
            serde_json::to_vec(&effective).map_err(|e| ConfigError::new("lossy", e.to_string()))?;
        Ok(ResolvedLossyReport {
            api_version: LOSSY_API_VERSION,
            preset_revision,
            config_fingerprint: fingerprint(&bytes),
            effective,
            warnings,
            ignored,
        })
    }
}

fn validate_rate(v: &RateControlV2, base: &LossyBase) -> Result<(), ConfigError> {
    if v.target_bytes.is_some() && v.target_bpp_x10000.is_some() {
        return Err(ConfigError::new(
            "rate",
            "targetBytes and targetBppX10000 are mutually exclusive",
        ));
    }
    match v.mode {
        RateMode::TargetBytes if v.target_bytes.is_none() => {
            return Err(ConfigError::new(
                "rate.targetBytes",
                "required by target-bytes mode",
            ))
        }
        RateMode::TargetBpp if v.target_bpp_x10000.is_none() => {
            return Err(ConfigError::new(
                "rate.targetBppX10000",
                "required by target-bpp mode",
            ))
        }
        RateMode::ConstrainedQuality if v.min_quality_x100.is_none() => {
            if matches!(base, LossyBase::Explicit) {
                return Err(ConfigError::new(
                    "rate.minQualityX100",
                    "required by constrained-quality mode with an explicit base",
                ));
            }
        }
        RateMode::ConstantQuality if v.target_bytes.is_some() || v.target_bpp_x10000.is_some() => {
            return Err(ConfigError::new(
                "rate.mode",
                "constant-quality cannot carry a bitrate target",
            ))
        }
        _ => {}
    }
    if matches!(v.mode, RateMode::ConstrainedQuality)
        && v.target_bytes.is_none()
        && v.target_bpp_x10000.is_none()
    {
        return Err(ConfigError::new(
            "rate",
            "constrained-quality requires targetBytes or targetBppX10000",
        ));
    }
    if matches!(base, LossyBase::Explicit)
        && matches!(v.mode, RateMode::TargetBytes | RateMode::TargetBpp)
        && v.min_quality_x100.is_none()
    {
        return Err(ConfigError::new(
            "rate.minQualityX100",
            "a bitrate target with an explicit base requires a quality floor",
        ));
    }
    if let Some(q) = v.min_quality_x100 {
        in_range("rate.minQualityX100", q, 100, 10000)?;
    }
    in_range(
        "rate.rdoLambdaScaleX1000",
        v.rdo_lambda_scale_x1000,
        250,
        4000,
    )?;
    in_range("rate.maxFrameDropDbX100", v.max_frame_drop_db_x100, 0, 500)?;
    Ok(())
}

fn validate_first_frame(v: &FirstFrameTuning) -> Result<(), ConfigError> {
    match v.mode {
        FirstFrameMode::QualityOffset if v.quality_offset_x100.is_none() => {
            return Err(ConfigError::new(
                "firstFrame.qualityOffsetX100",
                "required by quality-offset mode",
            ))
        }
        FirstFrameMode::Explicit if v.quality_x100.is_none() => {
            return Err(ConfigError::new(
                "firstFrame.qualityX100",
                "required by explicit mode",
            ))
        }
        FirstFrameMode::MatchSequence | FirstFrameMode::Lossless
            if v.quality_offset_x100.is_some() || v.quality_x100.is_some() =>
        {
            return Err(ConfigError::new(
                "firstFrame.mode",
                "quality override is invalid for this mode",
            ))
        }
        _ => {}
    }
    if let Some(x) = v.quality_offset_x100 {
        if !(-2000..=2000).contains(&x) {
            return Err(ConfigError::new(
                "firstFrame.qualityOffsetX100",
                "must be in -2000..=2000",
            ));
        }
    }
    if let Some(q) = v.quality_x100 {
        in_range("firstFrame.qualityX100", q, 100, 10000)?;
    }
    in_range(
        "firstFrame.rdoLambdaScaleX1000",
        v.rdo_lambda_scale_x1000,
        250,
        4000,
    )
}

fn validate_quant(v: &QuantizationTuning) -> Result<(), ConfigError> {
    match v.mode {
        QuantMode::ExplicitSteps if v.luma_step_q8.is_none() || v.chroma_step_q8.is_none() => {
            return Err(ConfigError::new(
                "quant",
                "explicit-steps requires lumaStepQ8 and chromaStepQ8",
            ))
        }
        QuantMode::FromQuality if v.luma_step_q8.is_some() || v.chroma_step_q8.is_some() => {
            return Err(ConfigError::new(
                "quant.mode",
                "from-quality cannot carry explicit steps",
            ))
        }
        _ => {}
    }
    for (field, value) in [
        ("quant.lumaStepQ8", v.luma_step_q8),
        ("quant.chromaStepQ8", v.chroma_step_q8),
    ] {
        if let Some(x) = value {
            in_range(field, x, 256, 65280)?;
        }
    }
    for (field, value, lo, hi) in [
        ("quant.lumaScaleX1000", v.luma_scale_x1000, 500, 4000),
        ("quant.chromaScaleX1000", v.chroma_scale_x1000, 500, 4000),
        ("quant.dcScaleX1000", v.dc_scale_x1000, 250, 2000),
        (
            "quant.highFreqScaleX1000",
            v.high_freq_scale_x1000,
            500,
            4000,
        ),
    ] {
        in_range(field, value, lo, hi)?;
    }
    if !(-128..=256).contains(&v.deadzone_luma_x256)
        || !(-128..=256).contains(&v.deadzone_chroma_x256)
    {
        return Err(ConfigError::new("quant.deadzone", "must be in -128..=256"));
    }
    Ok(())
}

fn validate_chroma(v: &ChromaTuning, ctx: ResolveContext) -> Result<(), ConfigError> {
    in_range("chroma.edgeProtectionX100", v.edge_protection_x100, 0, 200)?;
    if v.sampling == ChromaSampling::Cs420 && ctx.components.is_some_and(|n| n != 3) {
        return Err(ConfigError::new(
            "chroma.sampling",
            "420 requires a three-component input",
        ));
    }
    Ok(())
}
fn validate_perceptual(v: &PerceptualTuning, allow_ignored: bool) -> Result<(), ConfigError> {
    for (field, value) in [
        ("perceptual.activityMaskingX100", v.activity_masking_x100),
        (
            "perceptual.flatAreaProtectionX100",
            v.flat_area_protection_x100,
        ),
        ("perceptual.edgeProtectionX100", v.edge_protection_x100),
        ("perceptual.ringingControlX100", v.ringing_control_x100),
    ] {
        in_range(field, value, 0, 200)?;
    }
    match (v.noise_mode, v.noise_tau_x100) {
        (NoiseMode::Manual, None) => Err(ConfigError::new(
            "perceptual.noiseTauX100",
            "required by manual noise mode",
        )),
        (NoiseMode::Manual, Some(t)) => in_range("perceptual.noiseTauX100", t, 50, 400),
        (_, Some(_)) if !allow_ignored => Err(ConfigError::new(
            "perceptual.noiseTauX100",
            "only valid in manual noise mode",
        )),
        _ => Ok(()),
    }
}
fn validate_temporal(v: &TemporalTuning, allow_ignored: bool) -> Result<(), ConfigError> {
    if v.scene_cut == SceneCutModeV2::Manual && v.scene_cut_threshold_x1000.is_none() {
        return Err(ConfigError::new(
            "temporal.sceneCutThresholdX1000",
            "required by manual scene-cut mode",
        ));
    }
    if v.scene_cut != SceneCutModeV2::Manual
        && v.scene_cut_threshold_x1000.is_some()
        && !allow_ignored
    {
        return Err(ConfigError::new(
            "temporal.sceneCutThresholdX1000",
            "only valid in manual scene-cut mode",
        ));
    }
    if let Some(t) = v
        .scene_cut_threshold_x1000
        .filter(|_| v.scene_cut == SceneCutModeV2::Manual)
    {
        in_range("temporal.sceneCutThresholdX1000", t, 0, 1000)?;
    }
    if v.motion_mode == MotionModeV2::Integer && v.motion_range.is_none() {
        return Err(ConfigError::new(
            "temporal.motionRange",
            "required by integer motion mode",
        ));
    }
    if let Some(r) = v.motion_range {
        if r > 32 {
            return Err(ConfigError::new(
                "temporal.motionRange",
                "must be in 0..=32",
            ));
        }
    }
    if !(-2000..=2000).contains(&v.anchor_quality_offset_x100) {
        return Err(ConfigError::new(
            "temporal.anchorQualityOffsetX100",
            "must be in -2000..=2000",
        ));
    }
    Ok(())
}
fn validate_experimental(v: Option<&ExperimentalToolTuning>) -> Result<(), ConfigError> {
    let Some(v) = v else {
        return Ok(());
    };
    let configured = v.tile_size.is_some()
        || !v.transform_sizes.is_empty()
        || v.transform_skip != ToolMode::Auto
        || v.coefficient_coding.is_some()
        || v.palette != ToolMode::Auto
        || v.rdo_candidate_limit != 0;
    if configured && !v.allow_experimental {
        return Err(ConfigError::new(
            "experimental.allowExperimental",
            "must be true when experimental tools are configured",
        ));
    }
    if let Some(t) = v.tile_size {
        if !matches!(t, 32 | 64) {
            return Err(ConfigError::new(
                "experimental.tileSize",
                "must be 32 or 64",
            ));
        }
    }
    Ok(())
}

fn collect_ignored(v: &mut LossyOptionsV2, ignored: &mut Vec<IgnoredField>) {
    if v.allow_ignored
        && v.perceptual.noise_mode != NoiseMode::Manual
        && v.perceptual.noise_tau_x100.take().is_some()
    {
        ignored.push(IgnoredField {
            field: "perceptual.noiseTauX100".into(),
            reason: "noise mode is not manual".into(),
        });
    }
    if v.allow_ignored
        && v.temporal.scene_cut != SceneCutModeV2::Manual
        && v.temporal.scene_cut_threshold_x1000.take().is_some()
    {
        ignored.push(IgnoredField {
            field: "temporal.sceneCutThresholdX1000".into(),
            reason: "scene-cut mode is not manual".into(),
        });
    }
}

fn resolve_auto(v: &mut LossyOptionsV2, warnings: &mut Vec<ConfigWarning>, ctx: ResolveContext) {
    if v.chroma.sampling == ChromaSampling::Auto {
        v.chroma.sampling = if ctx.components.is_some_and(|n| n != 3) {
            ChromaSampling::Cs444
        } else {
            ChromaSampling::Cs420
        };
    }
    if v.temporal.reference_mode == ReferenceModeV2::Auto {
        v.temporal.reference_mode = ReferenceModeV2::Hybrid;
    }
    if v.temporal.motion_range.is_none() {
        v.temporal.motion_range = Some(2);
    }
    if v.temporal.anchor_interval.is_none() {
        v.temporal.anchor_interval = Some(10);
    }
    if v.perceptual.noise_mode == NoiseMode::Auto {
        v.perceptual.noise_mode = NoiseMode::Off;
        warnings.push(ConfigWarning {
            code: "auto-resolved".into(),
            field: "perceptual.noiseMode".into(),
            message: "Auto resolved to off without source-noise metadata".into(),
        });
    }
}

fn in_range<T: Copy + PartialOrd + fmt::Display>(
    field: &'static str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ConfigError> {
    if value < min || value > max {
        Err(ConfigError::new(field, format!("must be in {min}..={max}")))
    } else {
        Ok(())
    }
}
fn fingerprint(bytes: &[u8]) -> [u8; 16] {
    let mut a = 0xcbf29ce484222325u64;
    let mut b = 0x84222325cbf29ce4u64;
    for &x in bytes {
        a = (a ^ x as u64).wrapping_mul(0x100000001b3);
        b = (b ^ (x as u64).wrapping_add(a.rotate_left(13))).wrapping_mul(0x100000001b3);
    }
    let mut out = [0; 16];
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..].copy_from_slice(&b.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::core::config::lossy_v2::LossyOptionsV2Builder;

    #[test]
    fn validates_conflicts_and_resolves_deterministically() {
        let cfg = LossyOptionsV2Builder::preset(9650)
            .effort(8)
            .build()
            .unwrap();
        let a = cfg.resolve_without_encoding().unwrap();
        let b = cfg.resolve_without_encoding().unwrap();
        assert_eq!(a.config_fingerprint, b.config_fingerprint);
        assert_eq!(a.preset_revision, Some(PRESET_REVISION_CURRENT));
        let mut bad = cfg;
        bad.rate.mode = RateMode::TargetBytes;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn bitrate_target_inherits_preset_quality_floor() {
        let cfg = LossyOptionsV2Builder::preset(9650)
            .target_bytes(1_000_000)
            .build()
            .unwrap();
        let report = cfg.resolve_without_encoding().unwrap();
        assert_eq!(report.effective.rate.min_quality_x100, Some(9650));

        let mut explicit = LossyOptionsV2Builder::explicit()
            .with_explicit_steps(352, 448)
            .build()
            .unwrap();
        explicit.rate.mode = RateMode::TargetBytes;
        explicit.rate.target_bytes = Some(1_000_000);
        assert!(explicit.validate().is_err());
    }

    #[test]
    fn ignored_fields_require_explicit_opt_in() {
        let mut cfg = LossyOptionsV2Builder::preset(9600).build().unwrap();
        cfg.perceptual.noise_mode = NoiseMode::Off;
        cfg.perceptual.noise_tau_x100 = Some(150);
        assert!(cfg.resolve_without_encoding().is_err());
        cfg.allow_ignored = true;
        let report = cfg.resolve_without_encoding().unwrap();
        assert_eq!(report.ignored.len(), 1);
        assert!(report.effective.perceptual.noise_tau_x100.is_none());
    }
}
