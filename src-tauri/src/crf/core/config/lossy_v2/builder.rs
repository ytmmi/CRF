use super::*;

#[derive(Debug, Clone)]
pub struct LossyOptionsV2Builder {
    value: LossyOptionsV2,
}

impl LossyOptionsV2Builder {
    pub fn preset(quality_x100: u16) -> Self {
        Self {
            value: LossyOptionsV2 {
                api_version: LOSSY_API_VERSION,
                base: LossyBase::Preset {
                    quality_x100,
                    revision: None,
                },
                rate: Default::default(),
                first_frame: Default::default(),
                quant: Default::default(),
                chroma: Default::default(),
                perceptual: Default::default(),
                temporal: Default::default(),
                experimental: None,
                performance: Default::default(),
                allow_ignored: false,
            },
        }
    }
    pub fn explicit() -> Self {
        let mut x = Self::preset(9600);
        x.value.base = LossyBase::Explicit;
        x.value.quant.mode = QuantMode::ExplicitSteps;
        x
    }
    pub fn explicit_steps(luma_q8: u16, chroma_q8: u16) -> Self {
        Self::explicit().with_explicit_steps(luma_q8, chroma_q8)
    }
    pub fn revision(mut self, revision: u16) -> Self {
        if let LossyBase::Preset { revision: r, .. } = &mut self.value.base {
            *r = Some(revision);
        }
        self
    }
    pub fn target_bytes(mut self, bytes: u64) -> Self {
        self.value.rate.mode = RateMode::TargetBytes;
        self.value.rate.target_bytes = Some(bytes);
        self
    }
    pub fn min_quality(mut self, quality_x100: u16) -> Self {
        self.value.rate.min_quality_x100 = Some(quality_x100);
        self
    }
    pub fn first_frame_offset(mut self, offset_x100: i16) -> Self {
        self.value.first_frame.mode = FirstFrameMode::QualityOffset;
        self.value.first_frame.quality_offset_x100 = Some(offset_x100);
        self
    }
    pub fn chroma_sampling(mut self, sampling: ChromaSampling) -> Self {
        self.value.chroma.sampling = sampling;
        self
    }
    pub fn perceptual_strength(mut self, strength_x100: u16) -> Self {
        self.value.perceptual.activity_masking_x100 = strength_x100;
        self.value.perceptual.flat_area_protection_x100 = strength_x100;
        self.value.perceptual.edge_protection_x100 = strength_x100;
        self
    }
    pub fn reference_mode(mut self, mode: ReferenceModeV2) -> Self {
        self.value.temporal.reference_mode = mode;
        self
    }
    pub fn effort(mut self, effort: u8) -> Self {
        self.value.performance.effort = effort;
        self
    }
    pub fn with_explicit_steps(mut self, luma_q8: u16, chroma_q8: u16) -> Self {
        self.value.base = LossyBase::Explicit;
        self.value.quant.mode = QuantMode::ExplicitSteps;
        self.value.quant.luma_step_q8 = Some(luma_q8);
        self.value.quant.chroma_step_q8 = Some(chroma_q8);
        self
    }
    pub fn build(self) -> Result<LossyOptionsV2, ConfigError> {
        self.value.validate()?;
        Ok(self.value)
    }
}

impl LossyOptionsV2 {
    pub fn builder_preset(quality_x100: u16) -> LossyOptionsV2Builder {
        LossyOptionsV2Builder::preset(quality_x100)
    }
}
