use super::*;

/// 编码内核直接消费的 V2 有效配置。它不是兼容层，也不对外序列化。
#[derive(Debug, Clone)]
pub struct KernelLossyConfig {
    pub enabled: bool,
    pub global_step: u8,
    pub first_frame_step: u8,
    pub anchor_step: u8,
    pub chroma_scale_x1000: u16,
    pub explicit_chroma_step: Option<u8>,
    pub deadzone_bias: i8,
    pub chroma_deadzone_bias: i8,
    pub chroma_half_res: bool,
    pub noise_adaptive: bool,
    pub noise_tau_x100: u16,
    /// P4.2 activity masking：纹理区增步长强度（100=中性，>100 纹理区加宽死区省码率）
    pub activity_masking_x100: u16,
    /// P4.3 flat-area protection：平坦区减步长强度（100=中性，>100 平坦区收窄死区防 banding）
    pub flat_area_protection_x100: u16,
    pub reference_mode: ReferenceModeV2,
    pub change_mask: ToolMode,
    pub motion_mode: MotionModeV2,
    pub motion_range: u8,
    pub scene_cut: SceneCutModeV2,
    pub scene_cut_threshold_x1000: u16,
    pub anchor_interval: u16,
    pub rate: RateControlV2,
    pub palette: ToolMode,
    pub q95_perceptual: bool,
}

impl KernelLossyConfig {
    pub fn lossless() -> Self {
        Self {
            enabled: false,
            global_step: 0,
            first_frame_step: 0,
            anchor_step: 0,
            chroma_scale_x1000: 1000,
            explicit_chroma_step: None,
            deadzone_bias: 0,
            chroma_deadzone_bias: 0,
            chroma_half_res: false,
            noise_adaptive: false,
            noise_tau_x100: 150,
            activity_masking_x100: 100,
            flat_area_protection_x100: 100,
            reference_mode: ReferenceModeV2::Golden,
            change_mask: ToolMode::Off,
            motion_mode: MotionModeV2::Off,
            motion_range: 0,
            scene_cut: SceneCutModeV2::Off,
            scene_cut_threshold_x1000: 500,
            anchor_interval: 0,
            rate: RateControlV2::default(),
            palette: ToolMode::Auto,
            q95_perceptual: false,
        }
    }

    pub fn from_options(
        options: Option<&LossyOptionsV2>,
        ctx: ResolveContext,
    ) -> Result<Self, ConfigError> {
        let Some(options) = options else {
            return Ok(Self::lossless());
        };
        let effective = options.resolve_for_input(ctx)?.effective;
        if effective.chroma.sampling == ChromaSampling::Cs422 {
            return Err(ConfigError::new(
                "chroma.sampling",
                "the current CRF bitstream supports 444 and 420, not 422",
            ));
        }

        let base_quality = match &effective.base {
            LossyBase::Preset { quality_x100, .. } => Some(*quality_x100),
            LossyBase::Explicit => None,
        };
        let global_step = match effective.quant.mode {
            QuantMode::FromQuality => quality_step(base_quality.expect("validated preset")),
            QuantMode::ExplicitSteps => q8_step(effective.quant.luma_step_q8.expect("validated")),
        };
        let first_frame_step = match effective.first_frame.mode {
            FirstFrameMode::Lossless => 0,
            FirstFrameMode::MatchSequence => global_step,
            FirstFrameMode::QualityOffset => quality_step(
                base_quality
                    .ok_or_else(|| ConfigError::new(
                        "firstFrame.mode",
                        "quality-offset requires a preset base",
                    ))?
                    .saturating_add_signed(effective.first_frame.quality_offset_x100.unwrap_or(0))
                    .clamp(100, 10000),
            ),
            FirstFrameMode::Explicit => quality_step(effective.first_frame.quality_x100.unwrap()),
        };
        let anchor_step = if effective.temporal.anchor_quality_offset_x100 == 0 {
            global_step
        } else {
            quality_step(
                base_quality
                    .ok_or_else(|| ConfigError::new(
                        "temporal.anchorQualityOffsetX100",
                        "a non-zero anchor quality offset requires a preset base",
                    ))?
                    .saturating_add_signed(effective.temporal.anchor_quality_offset_x100)
                    .clamp(100, 10000),
            )
        };
        let explicit_chroma_step = effective.quant.chroma_step_q8.map(q8_step);
        let palette = effective
            .experimental
            .as_ref()
            .map_or(ToolMode::Auto, |x| x.palette);

        Ok(Self {
            enabled: true,
            global_step,
            first_frame_step,
            anchor_step,
            chroma_scale_x1000: effective.quant.chroma_scale_x1000,
            explicit_chroma_step,
            deadzone_bias: (effective.quant.deadzone_luma_x256 / 4).clamp(-32, 32) as i8,
            chroma_deadzone_bias: (effective.quant.deadzone_chroma_x256 / 4).clamp(-32, 32) as i8,
            chroma_half_res: effective.chroma.sampling == ChromaSampling::Cs420,
            noise_adaptive: effective.perceptual.noise_mode == NoiseMode::Manual,
            noise_tau_x100: effective.perceptual.noise_tau_x100.unwrap_or(150),
            activity_masking_x100: effective.perceptual.activity_masking_x100,
            flat_area_protection_x100: effective.perceptual.flat_area_protection_x100,
            reference_mode: effective.temporal.reference_mode,
            change_mask: effective.temporal.change_mask,
            motion_mode: effective.temporal.motion_mode,
            motion_range: effective.temporal.motion_range.unwrap_or(2),
            scene_cut: effective.temporal.scene_cut,
            scene_cut_threshold_x1000: effective
                .temporal
                .scene_cut_threshold_x1000
                .unwrap_or(500),
            anchor_interval: effective.temporal.anchor_interval.unwrap_or(0),
            rate: effective.rate,
            palette,
            q95_perceptual: base_quality == Some(9500),
        })
    }

    pub fn chroma_step(&self, luma_step: u8) -> u8 {
        if let Some(step) = self.explicit_chroma_step {
            return step;
        }
        let scaled = u32::from(luma_step) * u32::from(self.chroma_scale_x1000) / 1000;
        if self.chroma_scale_x1000 > 1000 && scaled <= u32::from(luma_step) && luma_step < 255 {
            return luma_step + 1;
        }
        scaled.clamp(1, 255) as u8
    }
}

fn quality_step(quality_x100: u16) -> u8 {
    let quality = ((quality_x100 + 50) / 100).clamp(1, 100) as u8;
    ((100 - quality as u32).div_ceil(5) as u8).clamp(1, 20)
}

fn q8_step(step_q8: u16) -> u8 {
    ((u32::from(step_q8) + 128) / 256).clamp(1, 255) as u8
}
