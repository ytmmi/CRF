//! P3 CABAC 系数编码器：概率自适应上下文 + RangeEncoder
//!
//! 上下文模型（§5-P3 第 5 项最小版）：
//! - ctx_nonzero：块是否有非零系数（自适应，基于前一块状态）
//! - ctx_run：run 截断一元的每个 bit（共用一个 prob）
//! - ctx_level_q：level 商前缀的每个 bit（共用一个 prob）
//! - level 余数 + sign：等概率直通（encode_direct）
//!
//! 与定长位流版（coeff_coder）相比：全零块概率高的内容下
//! ctx_nonzero 的 0-bit 更紧凑；run/level 的偏斜分布在 CABAC 下更高效。

use crate::crf::encoder::rle_cabac::RangeEncoder;
use crate::crf::core::entropy::cabac::INIT_PROB;

pub struct CoeffCABAC {
    rc: RangeEncoder,
    ctx_nonzero: u16,
    ctx_run: u16,
    ctx_level_q: u16,
}

impl CoeffCABAC {
    pub fn new() -> Self {
        CoeffCABAC {
            rc: RangeEncoder::new(),
            ctx_nonzero: INIT_PROB,
            ctx_run: INIT_PROB,
            ctx_level_q: INIT_PROB,
        }
    }

    /// 编码一个 8×8 块的 zigzag 系数（64 元素）
    /// 含 transform skip 标志：skip=true 时系数是空间域量化残差，
    /// skip=false 时是 DCT 域量化系数。CABAC 编码方式相同。
    pub fn encode_block(&mut self, zigzag: &[i32]) {
        debug_assert!(zigzag.len() == 64);
        let mut last_nz = None;
        for (i, &v) in zigzag.iter().enumerate().rev() {
            if v != 0 {
                last_nz = Some(i);
                break;
            }
        }
        match last_nz {
            None => {
                self.rc.encode_bit(false, &mut self.ctx_nonzero);
            }
            Some(last) => {
                self.rc.encode_bit(true, &mut self.ctx_nonzero);
                let mut prev_pos = 64; // 从数组末尾开始，第一个 run = 63-last
                for i in (0..=last).rev() {
                    let v = zigzag[i];
                    if v != 0 {
                        let run = (prev_pos - i - 1) as u32;
                        // run：截断一元，每个 bit 走 CABAC ctx_run
                        let r = run.min(63);
                        for _ in 0..r {
                            self.rc.encode_bit(true, &mut self.ctx_run);
                        }
                        if r < 63 {
                            self.rc.encode_bit(false, &mut self.ctx_run);
                        }
                        // level：截断 Rice k=0 = 截断一元商 + 0 余数
                        let av = v.unsigned_abs();
                        for _ in 0..av {
                            self.rc.encode_bit(true, &mut self.ctx_level_q);
                        }
                        self.rc.encode_bit(false, &mut self.ctx_level_q);
                        // sign：等概率直通
                        self.rc.encode_direct(v < 0);
                    prev_pos = i;
                }
            }
            // 终止 run：使解码端 pos 跳到 < 0 退出 while 循环。
            // 位置 0 非零时 prev_pos=0 无需终止（decode pos=-1 自然退出）。
            // 修复：此前 decode while pos>=0 在尾部零处越界读，消耗
            // RangeDecoder 状态破坏后续块——RCT 域负值触发，RGB 小值巧合通过。
            if prev_pos > 0 {
                let r = prev_pos.min(63);
                for _ in 0..r {
                    self.rc.encode_bit(true, &mut self.ctx_run);
                }
                if r < 63 {
                    self.rc.encode_bit(false, &mut self.ctx_run);
                }
            }
        }
    }
}

    pub fn finish(self) -> Vec<u8> {
        self.rc.finish()
    }
}
