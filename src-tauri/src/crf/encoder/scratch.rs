//! 编码热路径 Scratch Buffer。
//!
//! 生命周期刻意限定在单帧仲裁或单个 Rayon worker 内：不使用全局池、锁或
//! thread_local，既复用大型 `Vec<i32>` 容量，又保持并行候选之间完全隔离。

/// 帧级预测/闭环量化临时缓冲。
#[derive(Default)]
pub(crate) struct FrameScratch {
    residuals: Vec<i32>,
    reconstruction: Vec<i32>,
}

impl FrameScratch {
    /// 准备整帧残差缓冲（无损开环预测路径）。
    pub(crate) fn residuals(&mut self, len: usize) -> &mut [i32] {
        self.residuals.resize(len, 0);
        &mut self.residuals
    }

    /// 准备整帧残差和重建缓冲（有损闭环路径）。
    pub(crate) fn closed_loop(&mut self, len: usize) -> (&mut [i32], &mut [i32]) {
        self.residuals.resize(len, 0);
        self.reconstruction.resize(len, 0);
        (&mut self.residuals, &mut self.reconstruction)
    }
}

/// 条带级候选残差缓冲；每个 Rayon worker 持有一份并跨条带复用。
pub(crate) struct BandScratch {
    candidates: [Vec<i32>; 8],
}

impl Default for BandScratch {
    fn default() -> Self {
        Self {
            candidates: std::array::from_fn(|_| Vec::new()),
        }
    }
}

impl BandScratch {
    pub(crate) fn candidate(&mut self, index: usize, len: usize) -> &mut [i32] {
        let buffer = &mut self.candidates[index];
        buffer.resize(len, 0);
        buffer
    }

    pub(crate) fn candidate_ref(&self, index: usize) -> &[i32] {
        &self.candidates[index]
    }

    #[cfg(test)]
    pub(crate) fn capacities(&self) -> [usize; 8] {
        std::array::from_fn(|index| self.candidates[index].capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_scratch_reuses_capacity_after_shrink() {
        let mut scratch = FrameScratch::default();
        let (residuals, reconstruction) = scratch.closed_loop(4096);
        let res_ptr = residuals.as_ptr();
        let recon_ptr = reconstruction.as_ptr();
        let (residuals, reconstruction) = scratch.closed_loop(1024);
        assert_eq!(residuals.as_ptr(), res_ptr);
        assert_eq!(reconstruction.as_ptr(), recon_ptr);
    }

    #[test]
    fn band_scratch_reuses_all_candidate_capacities() {
        let mut scratch = BandScratch::default();
        for index in 0..8 {
            scratch.candidate(index, 2048);
        }
        let capacities = scratch.capacities();
        for index in 0..8 {
            scratch.candidate(index, 512);
        }
        assert_eq!(scratch.capacities(), capacities);
    }
}
