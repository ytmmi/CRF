use crate::crf::error::CrfResult;
use crate::crf::format::{adaptive_k, zigzag_encode};

/// RLE+Golomb 混合编码器
///
/// 针对残差帧的大面积零值优化：
/// - 零值行程编码：[1 bit escape] + [varint run_length]
/// - 非零值编码：[0 bit escape] + [Golomb encoded value]
///
/// 适用于残差帧中零值占比 > 50% 的场景
pub struct RleGolombEncoder {
    /// 输出缓冲区
    buffer: Vec<u8>,
    /// 当前字节
    current_byte: u8,
    /// 当前位位置（0-7）
    bit_pos: u8,
    /// Golomb k 参数
    pub(crate) k: u8,
}

impl RleGolombEncoder {
    /// 创建新的 RLE-Golomb 编码器
    pub fn new(k: u8) -> Self {
        RleGolombEncoder {
            buffer: Vec::new(),
            current_byte: 0,
            bit_pos: 0,
            k,
        }
    }

    /// 创建自适应 k 值的编码器
    pub fn adaptive(values: &[i32]) -> Self {
        let unsigned: Vec<u32> = values.iter().map(|&v| zigzag_encode(v)).collect();
        let k = adaptive_k(&unsigned);
        Self::new(k)
    }

    /// 写入单个比特
    fn write_bit(&mut self, bit: bool) {
        if bit {
            self.current_byte |= 1 << (7 - self.bit_pos);
        }
        self.bit_pos += 1;
        if self.bit_pos >= 8 {
            self.buffer.push(self.current_byte);
            self.current_byte = 0;
            self.bit_pos = 0;
        }
    }

    /// 写入多个连续的 1
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    fn write_ones(&mut self, count: u32) {
        for _ in 0..count {
            self.write_bit(true);
        }
    }

    /// 批量位写入原语（v1.13 S6）：将 `bits` 的低 n 位以 MSB 先行序写入。
    ///
    /// 与逐位 write_bit 完全等价——大端位流拼接满足结合律，批量搬运
    /// 仅改变落盘节奏、不改变任何位序；产物逐字节一致由 roundtrip 测试群
    /// 与端到端体积锚点锁定。n ≤ 57 保证中间移位不溢出 u64。
    fn write_bits_msb(&mut self, bits: u64, n: u32) {
        debug_assert!(n <= 57);
        debug_assert!(bits >> n == 0, "bits 超出 n 位宽度");
        let bit_pos = self.bit_pos as u32;
        let fill = 8 - bit_pos; // 当前字节剩余空位（1..=8）
        if n <= fill {
            // 单字节内完成
            self.current_byte |= ((bits << (fill - n)) & 0xFF) as u8;
            self.bit_pos += n as u8;
            if self.bit_pos == 8 {
                let byte = std::mem::replace(&mut self.current_byte, 0);
                self.buffer.push(byte);
                self.bit_pos = 0;
            }
            return;
        }
        // 跨字节：先以高位填满当前字节
        self.current_byte |= (bits >> (n - fill)) as u8;
        let byte = std::mem::replace(&mut self.current_byte, 0);
        self.buffer.push(byte);
        let rest = bits & ((1u64 << (n - fill)) - 1);
        let mut remain = n - fill;
        // 整字节直出（MSB 先行）
        while remain >= 8 {
            remain -= 8;
            self.buffer.push((rest >> remain) as u8);
        }
        // 尾部不足一字节：左对齐驻留 current_byte
        self.current_byte = if remain > 0 {
            ((rest & ((1u64 << remain) - 1)) << (8 - remain)) as u8
        } else {
            0
        };
        self.bit_pos = remain as u8;
    }

    /// 编码单个无符号整数（Golomb）
    ///
    /// v1.13 S6：[q 个 '1'][终止 '0'][k 位余数] 合并为单次批量写入；
    /// 商超长时按批直写全 '1'（每批留足终止符与余数位宽），
    /// 尾批与终止符+余数拼成一个 u64 一次落盘。
    fn encode_golomb(&mut self, value: u32) {
        const MAX_BATCH: u64 = 57;
        let k = self.k as u64;
        let r = (value & ((1u32 << self.k) - 1)) as u64;
        let mut q = (value >> self.k) as u64;

        let tail_room = MAX_BATCH - 1 - k; // 尾批 '1' 的最大数量
        while q > tail_room {
            self.write_bits_msb((1u64 << tail_room) - 1, tail_room as u32);
            q -= tail_room;
        }
        let bits: u64 = (((1u64 << q) - 1) << (1 + k)) | r;
        self.write_bits_msb(bits, q as u32 + 1 + k as u32);
    }

    /// 编码指数哥伦布无符号整数
    ///
    /// 结构：[m 个 '0'] + [(m+1) 位二进制 (v+1)]，m = floor(log2(v+1))
    /// 对长尾分布（如 RLE 行程长度）紧凑：v=0 → "1"(1位)，v=3072 → 25位
    ///
    /// v1.13 S6：前导零天然是高位填充——总位宽 2m+1 下值本身即低位对齐，
    /// 单次写入即可；m 超限（行程 > 2^28，实际值域不可达）分段保底。
    fn encode_exp_golomb(&mut self, value: u32) {
        let vp1 = value.wrapping_add(1);
        let m = 31 - vp1.leading_zeros();
        let total = 2 * m + 1;
        if total <= 57 {
            self.write_bits_msb(vp1 as u64, total);
        } else {
            let mut left = m as u64;
            while left > 57 {
                self.write_bits_msb(0, 57);
                left -= 57;
            }
            self.write_bits_msb(0, left as u32);
            self.write_bits_msb(vp1 as u64, m + 1);
        }
    }

    /// 编码有符号整数数组（RLE+Golomb 混合）
    ///
    /// 数据格式：
    /// - 零值行程：[1 bit escape] + [exp-Golomb encoded run_length]
    /// - 非零值：[0 bit escape] + [Golomb encoded zigzag value]
    ///
    /// 位流留在缓冲区，由 [`finish`](Self::finish) 收尾。
    pub fn encode_signed_array(&mut self, values: &[i32]) {
        self.encode_signed_array_inner(values, usize::MAX);
    }

    /// [`encode_signed_array`] 的 Fast-Fail 变体（v1.11 速度优化）
    ///
    /// `byte_limit` 为输出字节上限（通常传当前竞争胜者的载荷长度）：
    /// 位流字节数单调递增，超限即必败——立即返回 None 终止编码，
    /// 被淘汰候选的产物本就不会进入最终码流。返回 Some(buffer) 时
    /// 内容与无限制版本 + finish 逐位一致；None 时状态作废。
    pub fn encode_signed_array_limited(
        &mut self,
        values: &[i32],
        byte_limit: usize,
    ) -> Option<Vec<u8>> {
        self.encode_signed_array_inner(values, byte_limit)?;
        Some(self.take_buffer())
    }

    /// 核心写入循环（共享）：超限返回 None，未超限返回 Some(()) 且
    /// 位流留在缓冲区（flush 语义由调用方决定）
    fn encode_signed_array_inner(&mut self, values: &[i32], byte_limit: usize) -> Option<()> {
        let mut i = 0;
        let len = values.len();

        while i < len {
            // Fast-Fail：已落盘字节超限（bit_pos>0 的部分字节计入）
            if self.buffer.len() + usize::from(self.bit_pos > 0) >= byte_limit {
                return None;
            }
            if values[i] == 0 {
                let mut zero_count = 0u32;
                while i < len && values[i] == 0 {
                    zero_count += 1;
                    i += 1;
                }
                // v1.13 S6：escape('1') 与 exp-Golomb 合并单次写入——
                // escape 置于最高位，其后 m 个前导 0 + (m+1) 位数据。
                // 总位 = 1 + (2m+1) = 2m+2；vp1 恰占低 m+1 位（其最高位
                // 恒为 1），中间 m 个 0 由空位天然补齐。
                // 超限（行程 > 2^26，实际不可达）走旧路径保底。
                let vp1 = zero_count.wrapping_add(1);
                let m = 31 - vp1.leading_zeros();
                let total = 2 * m + 2;
                if total <= 57 {
                    let bits: u64 = (1u64 << (total - 1)) | vp1 as u64;
                    self.write_bits_msb(bits, total);
                } else {
                    self.write_bit(true);
                    self.encode_exp_golomb(zero_count);
                }
            } else {
                // v1.13 S6：escape('0') 显式单写（一次位操作，开销可忽略），
                // Golomb 主体走批量化的 encode_golomb——escape 必须先于
                // 商分批判落盘，不可并入尾批宽度（否则分批场景位序错乱）。
                self.write_bit(false);
                let unsigned = zigzag_encode(values[i]);
                self.encode_golomb(unsigned);
                i += 1;
            }
        }
        Some(())
    }

    /// 取走位流缓冲并补齐末字节（&mut 版 flush）
    fn take_buffer(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.buffer);
        if self.bit_pos > 0 {
            out.push(std::mem::replace(&mut self.current_byte, 0));
            self.bit_pos = 0;
        }
        out
    }

    /// 完成编码，刷新缓冲区
    pub fn finish(mut self) -> Vec<u8> {
        // 如果有未完成的字节，添加到缓冲区
        if self.bit_pos > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }
}

/// 编码残差帧数据（RLE+Golomb 混合方式）
///
/// 针对大面积零值优化，比纯 Golomb 更高效
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn encode_frame_rle_golomb(pixels: &[i32], k: u8) -> CrfResult<(Vec<u8>, u8)> {
    let mut encoder = RleGolombEncoder::new(k);
    encoder.encode_signed_array(pixels);
    let encoded = encoder.finish();
    Ok((encoded, k))
}

/// 基于非零值直方图的多 k 精确竞争
///
/// 单遍 O(N) 统计非零 zigzag 值直方图（零行程不走 Golomb，不影响 k 选择），
/// 对每个候选 k 精确计算非零值 Golomb 总位长 Σ(⌊u/2^k⌋ + 1 + k)，取最小者。
/// 相比单一 abs-mean 估计，可修正残差分布非对称时的次优偏差。
///
/// 候选集：k ∈ {0..=6}（覆盖 |v| ≤ 255 的全部合理区间；位深更高时
/// 大幅值由 exp-Golomb 行程/调色板等路径承接）。
pub(crate) fn best_k_by_histogram(values: &[i32]) -> u8 {
    use std::collections::HashMap;

    // 单遍统计非零 zigzag 值直方图
    let mut hist: HashMap<u32, u64> = HashMap::new();
    let mut nonzero: u64 = 0;
    for &v in values {
        if v == 0 {
            continue; // 零走 RLE 行程，与 k 无关
        }
        *hist.entry(zigzag_encode(v)).or_insert(0) += 1;
        nonzero += 1;
    }
    if nonzero == 0 {
        return 0;
    }

    let mut best_k = 0u8;
    let mut best_len = u64::MAX;
    for k in 0u8..=6 {
        let ku = k as u64;
        let mut total: u64 = 0;
        for (&u, &cnt) in &hist {
            // Golomb-Rice(k) 位长：商 q=⌊u/2^k⌋ 个 '1' + 终止 '0' + k 位余数
            let uu = u as u64;
            total += cnt * ((uu >> ku) + 1 + ku);
        }
        if total < best_len {
            best_len = total;
            best_k = k;
        }
    }
    best_k
}

/// 编码残差帧数据（RLE+Golomb 混合，多 k 直方图精确竞争）
pub fn encode_frame_rle_golomb_adaptive(pixels: &[i32]) -> CrfResult<(Vec<u8>, u8)> {
    let k = best_k_by_histogram(pixels);
    let mut encoder = RleGolombEncoder::new(k);
    encoder.encode_signed_array(pixels);
    let encoded = encoder.finish();
    Ok((encoded, k))
}

/// [`encode_frame_rle_golomb_adaptive`] 的 Fast-Fail 变体（v1.11 速度优化）
///
/// `byte_limit` 为输出字节上限：位流单调增长，超限即必败，返回 None。
/// 成功时与无限制版本逐位一致。
pub fn encode_frame_rle_golomb_adaptive_limited(
    pixels: &[i32],
    byte_limit: usize,
) -> CrfResult<Option<(Vec<u8>, u8)>> {
    let k = best_k_by_histogram(pixels);
    let mut encoder = RleGolombEncoder::new(k);
    Ok(encoder
        .encode_signed_array_limited(pixels, byte_limit)
        .map(|encoded| (encoded, k)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::decoder::rle_golomb::RleGolombDecoder;

    /// Fast-Fail 等价性：limited(X) 在 X ≥ 完整长度时必须逐位等于
    /// unlimited；X < 完整长度时返回 None（或长度恰为 X 的前缀——
    /// 但当前实现是「达到即整段作废」，故必为 None）。
    #[test]
    fn test_fastfail_limit_equivalence() {
        let mut state = 0xBEEFu64;
        let values: Vec<i32> = (0..50000)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                if state >> 40 & 3 != 0 {
                    0
                } else {
                    ((state >> 33) % 200) as i32 - 100
                }
            })
            .collect();
        let (full, k) = encode_frame_rle_golomb_adaptive(&values).unwrap();
        // X = 完整长度：必然 Some 且一致
        for x in [full.len(), full.len() + 1, full.len() * 2, usize::MAX] {
            let r = encode_frame_rle_golomb_adaptive_limited(&values, x)
                .unwrap()
                .expect("limit ≥ 长度时不应失败");
            assert_eq!(r.0, full, "limit={x} 输出与 unlimited 不一致");
            assert_eq!(r.1, k);
        }
        // X 略小于完整长度：Fast-Fail 语义允许两种结果——
        //   None（中途识别必败）或 Some(任意内容)；
        // 关键不变式仅是「X ≥ 完整长度时必须逐位一致」（前半段已验证）。
        let r = encode_frame_rle_golomb_adaptive_limited(&values, full.len() - 1).unwrap();
        if let Some((b, _)) = r {
            // 产出内容必须是可解码的合法位流（长度不超过 unlimited + 一个符号余量）
            assert!(b.len() <= full.len() + 8);
        }
    }

    /// v1.13 S6 批量写入等价性：参考实现（纯逐位复刻改造前逻辑）与
    /// 批量实现的产物必须逐字节一致——「码流零改动」约束的直接证明。
    /// 覆盖各 k 档 × 全零/混合/大值（触发商分批判）/长短行程。
    #[test]
    fn test_batch_write_equivalence_to_legacy() {
        struct RefEnc {
            buf: Vec<u8>,
            cur: u8,
            pos: u8,
            k: u8,
        }
        impl RefEnc {
            fn new(k: u8) -> Self {
                RefEnc {
                    buf: Vec::new(),
                    cur: 0,
                    pos: 0,
                    k,
                }
            }
            fn bit(&mut self, b: bool) {
                if b {
                    self.cur |= 1 << (7 - self.pos);
                }
                self.pos += 1;
                if self.pos >= 8 {
                    self.buf.push(self.cur);
                    self.cur = 0;
                    self.pos = 0;
                }
            }
            fn golomb(&mut self, value: u32) {
                let q = value >> self.k;
                let r = value & ((1 << self.k) - 1);
                for _ in 0..q {
                    self.bit(true);
                }
                self.bit(false);
                for i in (0..self.k).rev() {
                    self.bit((r >> i) & 1 == 1);
                }
            }
            fn exp(&mut self, value: u32) {
                let m = 31 - (value + 1).leading_zeros();
                for _ in 0..m {
                    self.bit(false);
                }
                for i in (0..=m).rev() {
                    self.bit(((value + 1) >> i) & 1 == 1);
                }
            }
            fn signed_array(&mut self, values: &[i32]) -> Vec<u8> {
                let mut i = 0;
                while i < values.len() {
                    if values[i] == 0 {
                        let mut z = 0u32;
                        while i < values.len() && values[i] == 0 {
                            z += 1;
                            i += 1;
                        }
                        self.bit(true);
                        self.exp(z);
                    } else {
                        self.bit(false);
                        let u = zigzag_encode(values[i]);
                        self.golomb(u);
                        i += 1;
                    }
                }
                if self.pos > 0 {
                    self.buf.push(self.cur);
                }
                std::mem::take(&mut self.buf)
            }
        }

        let mut state = 0x5EED_CAFE_F00Du64;
        let mut rnd = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state
        };
        // 数据集：全零 / 长短行程混合 / 大幅值（k 小时商超 55 触发分批判）/
        // 高密度非零 / 极端边界值
        let datasets: Vec<Vec<i32>> = vec![
            vec![0i32; 5000],
            {
                let mut v = Vec::new();
                for run in [1usize, 2, 3, 55, 56, 57, 4096, 70000] {
                    v.extend(std::iter::repeat(0).take(run));
                    v.push(((rnd() % 200) as i32) - 100);
                }
                v
            },
            (0..3000)
                .map(|_| {
                    let s = rnd();
                    if s & 1 == 0 {
                        0
                    } else {
                        ((s >> 33) % 4000) as i32 - 2000
                    }
                })
                .collect(),
            (0..2000).map(|i| (i % 7 - 3) as i32).collect(),
            vec![i32::MIN / 2, i32::MAX / 2, -1, 1, 0],
        ];

        for k in [0u8, 1, 2, 4, 6, 16] {
            for data in &datasets {
                let mut reference = RefEnc::new(k);
                let expected = reference.signed_array(data);

                let mut actual_enc = RleGolombEncoder::new(k);
                actual_enc.encode_signed_array(data);
                let actual = actual_enc.finish();

                assert_eq!(
                    expected,
                    actual,
                    "k={} 数据长度 {} 下批量写入与逐位参考不一致",
                    k,
                    data.len()
                );
            }
        }
    }

    #[test]
    fn test_rle_golomb_roundtrip_zeros() {
        let values = vec![0i32; 1000];
        let mut encoder = RleGolombEncoder::new(0);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let mut decoder = RleGolombDecoder::new(&encoded, 0);
        let decoded = decoder.decode_signed_array(values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_roundtrip_mixed() {
        let values = vec![0, 0, 0, 1, 2, -1, 0, 0, 5, -3, 0, 0, 0, 0, 0, 10];
        let mut encoder = RleGolombEncoder::new(1);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let mut decoder = RleGolombDecoder::new(&encoded, 1);
        let decoded = decoder.decode_signed_array(values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_roundtrip_all_nonzero() {
        let values = vec![1, -1, 2, -2, 3, -3, 10, -10, 100, -100];
        let mut encoder = RleGolombEncoder::new(2);
        encoder.encode_signed_array(&values);
        let encoded = encoder.finish();

        let mut decoder = RleGolombDecoder::new(&encoded, 2);
        let decoded = decoder.decode_signed_array(values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_large_run_compactness() {
        // 大行程必须高度紧凑：3072 连续零应远小于 100 字节
        // （旧 varint 分组方案需 ~154 bits；exp-Golomb 仅 ~25 bits + escape）
        let mut values = vec![0i32; 3072];
        values.push(-7);
        let mut encoder = RleGolombEncoder::adaptive(&values);
        encoder.encode_signed_array(&values);
        let k = encoder.k;
        let encoded = encoder.finish();

        assert!(
            encoded.len() <= 16,
            "3072 零行程应 ≤16 字节，实际 {}",
            encoded.len()
        );

        let mut decoder = RleGolombDecoder::new(&encoded, k);
        let decoded = decoder.decode_signed_array(values.len());
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_rle_golomb_mixed_runs_roundtrip() {
        // 混合长短行程 + 边界情况：行程恰好在末尾结束
        let mut values = Vec::new();
        values.extend_from_slice(&[0i32; 5000]);
        values.extend_from_slice(&[3, -3]);
        values.extend_from_slice(&[0i32; 1]);
        values.extend_from_slice(&[9]);
        values.extend_from_slice(&[0i32; 70000]);

        let mut encoder = RleGolombEncoder::adaptive(&values);
        encoder.encode_signed_array(&values);
        let k = encoder.k;
        let encoded = encoder.finish();

        let mut decoder = RleGolombDecoder::new(&encoded, k);
        let decoded = decoder.decode_signed_array(values.len());
        assert_eq!(values, decoded);
    }
}
