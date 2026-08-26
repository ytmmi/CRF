//! GPU 传输策略和显存预算估算。

/// 主机与设备之间的数据传输策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Auto,
    Pinned,
    Mapped,
    Unified,
}

/// 一次批处理的显存估算结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuMemoryEstimate {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub scratch_bytes: u64,
}

impl GpuMemoryEstimate {
    pub fn total_bytes(self) -> u64 {
        self.input_bytes
            .saturating_add(self.output_bytes)
            .saturating_add(self.scratch_bytes)
    }

    pub fn fits(self, budget_bytes: Option<u64>) -> bool {
        budget_bytes.map_or(true, |budget| self.total_bytes() <= budget)
    }
}

pub fn estimate_i32_batch(elements: usize, planes: usize) -> GpuMemoryEstimate {
    let bytes = (elements as u64)
        .saturating_mul(planes as u64)
        .saturating_mul(std::mem::size_of::<i32>() as u64);
    GpuMemoryEstimate {
        input_bytes: bytes,
        output_bytes: bytes,
        scratch_bytes: bytes / 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_bounded_and_budget_aware() {
        let estimate = estimate_i32_batch(1024, 3);
        assert_eq!(estimate.input_bytes, 12_288);
        assert_eq!(estimate.total_bytes(), 30_720);
        assert!(estimate.fits(Some(30_720)));
        assert!(!estimate.fits(Some(30_719)));
    }
}
