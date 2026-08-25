//! P3 CABAC 系数解码器：与 encoder/coeff_cabac.rs 对称
//!
//! 3 上下文（nonzero/run/level_q）+ 直通余数/sign。
//! 解码每块 zigzag 系数 → 逆 run-level → 按模式重建。

use crate::crf::decoder::rle_cabac::RangeDecoder;

const INIT_PROB: u16 = 2048;

pub struct CoeffCABACDecoder<'a> {
    rc: RangeDecoder<'a>,
    ctx_nonzero: u16,
    ctx_run: u16,
    ctx_level_q: u16,
}

impl<'a> CoeffCABACDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        CoeffCABACDecoder {
            rc: RangeDecoder::new(data),
            ctx_nonzero: INIT_PROB,
            ctx_run: INIT_PROB,
            ctx_level_q: INIT_PROB,
        }
    }

    /// 解码一个 8×8 块的 zigzag 系数（64 元素）
    pub fn decode_block(&mut self) -> [i32; 64] {
        let mut coeffs = [0i32; 64];
        if !self.rc.decode_bit(&mut self.ctx_nonzero) {
            // 全零块
            return coeffs;
        }
        // 非零块：逆序 run-level 重建
        let mut pos = 63i32;
        while pos >= 0 {
            // 解码 run
            let mut run = 0u32;
            while run < 63 && self.rc.decode_bit(&mut self.ctx_run) {
                run += 1;
            }
            // 如果 run 达到 63 且最后一位是 1，表示截断（无终止 0）
            // 但编码端 run < 63 时有终止 0，run == 63 时无终止 0
            // decode_bit 返回 true 表示读到 1（继续 run），false 表示 0（终止）
            // 上面的循环：run < 63 且 decode_bit=true → run++ 继续
            //            decode_bit=false → 终止
            //            run==63 → 截断（无终止 0）
            // 但编码端：run==63 时不编码终止 0，解码端 run==63 退出循环
            // 问题：编码端 run < 63 编码终止 0，run == 63 不编码
            // 解码端：循环条件 run < 63 && decode_bit=true
            //         如果 run < 63 且读到 false → 终止 ✓
            //         如果 run == 63 → 退出（截断）✓
            // 但如果 run < 63 且 decode_bit=true，run++ 后如果 run==63 退出
            // 这时多读了一个 true bit——实际上编码端 run==63 是连续 63 个 1 无终止
            // 解码端读到 63 个 true 后退出，pos 减去 63 ✓

            pos -= run as i32;
            if pos < 0 {
                break;
            }
            // 解码 level（截断一元商 + sign）
            let mut level = 0u32;
            while self.rc.decode_bit(&mut self.ctx_level_q) {
                level += 1;
            }
            let sign = self.rc.decode_direct();
            let v = if sign { -(level as i32) } else { level as i32 };
            if pos < 64 {
                coeffs[pos as usize] = v;
            }
            pos -= 1; // 移到下一个非零位置之前
        }
        coeffs
    }
}
