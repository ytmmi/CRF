//! P3 专用系数熵编码器：面向 DCT 系数统计特性的 run-level 编码
//!
//! 探针 v2 负结果根因：通用 RLE+CABAC 承载变换系数时 last_nz 位置流
//! 开销 > 截断收益。本编码器采用 H.264 式嵌入式 run-level 方案：
//!
//! 每块格式：
//! - 全零块：`0`（1 bit）
//! - 非零块：`1` + 从 last_nz 逆序的 (run, level) 对
//!   - run = 前导零行程（截断一元码，上限 63）
//!   - level = 系数值（截断 Rice + 1 bit sign）
//!   - 末尾隐式 EOB（最后一个非零系数后不再编码 run）
//!
//! 与通用 CABAC 相比的优势：
//! 1. 位置信息编码在 run 中（不单独的位置流）
//! 2. 全零块仅 1 bit（对比通用 CABAC 的零行程 Golomb 商）
//! 3. 小幅值系数（±1,±2）用截断 Rice 极短码
//! 4. 无 MA 树上下文开销（系数统计与空间域残差不同）

/// 截断一元码编码（0..max-1 → 0..max-1 个 1 + 0）
fn write_truncated_unary(buf: &mut Vec<u8>, bit_pos: &mut u8, cur: &mut u8, val: u32, max: u32) {
    let v = val.min(max);
    for _ in 0..v {
        write_bit(buf, bit_pos, cur, true);
    }
    if v < max {
        write_bit(buf, bit_pos, cur, false);
    }
}

/// 截断 Rice 编码（k=0：与截断一元等价；k>0 偏移）
fn write_truncated_rice(buf: &mut Vec<u8>, bit_pos: &mut u8, cur: &mut u8, val: u32, k: u32) {
    let q = val >> k;
    let r = val & ((1 << k) - 1);
    // 商：截断一元（前导 1，终止 0）
    for _ in 0..q {
        write_bit(buf, bit_pos, cur, true);
    }
    write_bit(buf, bit_pos, cur, false);
    // 余数：k 位二进制
    if k > 0 {
        for i in (0..k).rev() {
            write_bit(buf, bit_pos, cur, (r >> i) & 1 == 1);
        }
    }
}

fn write_bit(buf: &mut Vec<u8>, bit_pos: &mut u8, cur: &mut u8, bit: bool) {
    if bit {
        *cur |= 1 << (7 - *bit_pos);
    }
    *bit_pos += 1;
    if *bit_pos == 8 {
        buf.push(*cur);
        *cur = 0;
        *bit_pos = 0;
    }
}

/// 专用系数熵编码器
pub struct CoeffEncoder {
    buffer: Vec<u8>,
    bit_pos: u8,
    current_byte: u8,
}

impl CoeffEncoder {
    pub fn new() -> Self {
        CoeffEncoder {
            buffer: Vec::new(),
            bit_pos: 0,
            current_byte: 0,
        }
    }

    /// 编码一个 8×8 块的 zigzag 扫描系数（64 元素）
    ///
    /// P3 深化（§5-P3 第 4 项）：DC/AC 分离——DC 系数独立编码
    /// （截断 Rice + sign），AC 系数走 run-level。DC 主导块
    /// （AC 全零）只需 DC 值 + 1 bit，极紧凑。
    pub fn encode_block(&mut self, zigzag: &[i32]) {
        debug_assert!(zigzag.len() == 64);
        let dc = zigzag[0];
        // DC: 截断 Rice k=0 + sign
        let dc_abs = dc.unsigned_abs();
        write_truncated_rice(
            &mut self.buffer,
            &mut self.bit_pos,
            &mut self.current_byte,
            dc_abs,
            0,
        );
        write_bit(
            &mut self.buffer,
            &mut self.bit_pos,
            &mut self.current_byte,
            dc < 0,
        );

        // AC: zigzag[1..64] 逆序 run-level
        let ac = &zigzag[1..64];
        let mut last_nz = None;
        for (i, &v) in ac.iter().enumerate().rev() {
            if v != 0 {
                last_nz = Some(i);
                break;
            }
        }
        match last_nz {
            None => {
                write_bit(
                    &mut self.buffer,
                    &mut self.bit_pos,
                    &mut self.current_byte,
                    false,
                );
            }
            Some(last) => {
                write_bit(
                    &mut self.buffer,
                    &mut self.bit_pos,
                    &mut self.current_byte,
                    true,
                );
                let mut prev_pos = last + 1;
                for i in (0..=last).rev() {
                    let v = ac[i];
                    if v != 0 {
                        let run = (prev_pos - i - 1) as u32;
                        write_truncated_unary(
                            &mut self.buffer,
                            &mut self.bit_pos,
                            &mut self.current_byte,
                            run,
                            62,
                        );
                        let av = v.unsigned_abs();
                        write_truncated_rice(
                            &mut self.buffer,
                            &mut self.bit_pos,
                            &mut self.current_byte,
                            av,
                            0,
                        );
                        write_bit(
                            &mut self.buffer,
                            &mut self.bit_pos,
                            &mut self.current_byte,
                            v < 0,
                        );
                        prev_pos = i;
                    }
                }
            }
        }
    }

    /// 完成编码，返回字节流
    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_pos > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }

    /// 当前已编码字节数（含未完成字节）
    #[allow(dead_code)]
    pub fn byte_len(&self) -> usize {
        self.buffer.len() + if self.bit_pos > 0 { 1 } else { 0 }
    }
}
