//! RLE+CABAC 混合熵编码器（frame_type=5 / frame_type=6 共用）
//!
//! 位流语法与 RLE+Golomb 完全同构，区别在于：
//! - escape / Golomb 商前缀 / 行程 exp-Golomb 前导位改由**自适应二值算术编码**
//!   （简化 CABAC，LZMA 式 range coder）承载；
//! - Golomb 余数与 exp-Golomb 数据位为高熵信息，使用**等概率直通（bypass）**。
//!
//! v3（MA 树上下文）：三种上下文分类器帧内竞争——MA 树（JPEG-XL
//! Meta-Adaptive 同款，数据驱动决策树）/ 固定梯度 4 档 / 全帧统一单档，
//! 取码流最小者，flags 编码所选模式。保证相对任何单一模式单调不劣化。

use rayon::prelude::*;

use crate::crf::core::entropy::cabac::{RC_BITS, RC_MOVE, RC_TOP, INIT_PROB};
use crate::crf::core::entropy::context::{CtxModel, N_CTX};

// ===== Range Coder（32 位区间 + 64 位低位累积）=====
// RC 常量（RC_BITS/RC_MOVE/RC_TOP/INIT_PROB）统一定义在 core/entropy/cabac.rs（P4）。

/// 二值算术编码器
pub struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl RangeEncoder {
    pub fn new() -> Self {
        RangeEncoder {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    /// LZMA 式延迟进位输出
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let mut temp = self.cache;
            let add = (self.low >> 32) as u8;
            loop {
                self.out.push(temp.wrapping_add(add));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low << 8) & 0xFFFF_FFFF;
    }

    #[inline]
    fn normalize(&mut self) {
        while self.range < RC_TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    /// 自适应概率位编码（prob 为 P(bit=1)，12-bit 定点）
    #[inline]
    pub fn encode_bit(&mut self, bit: bool, prob: &mut u16) {
        let bound = (self.range >> RC_BITS) * (*prob as u32);
        if bit {
            *prob += ((1 << RC_BITS) - *prob) >> RC_MOVE;
            self.range = bound;
        } else {
            *prob -= *prob >> RC_MOVE;
            self.low += bound as u64;
            self.range -= bound;
        }
        self.normalize();
    }

    /// 等概率位直通（bypass，不消耗模型）
    #[inline]
    pub fn encode_direct(&mut self, bit: bool) {
        self.range >>= 1;
        if bit {
            self.low += self.range as u64;
        }
        self.normalize();
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }

    /// 已产出字节数（Fast-Fail 用：含 cache_size 的未冲刷估算，
    /// 单调递增——超限即必败）
    #[inline]
    pub fn out_len(&self) -> usize {
        self.out.len() + self.cache_size as usize
    }
}

// ===== 上下文布局与分类器见 ma_tree.rs（编解码共享）=====

pub struct CabacEncoder {
    rc: RangeEncoder,
    probs: Vec<u16>,
    k: u8,
}

impl CabacEncoder {
    /// 创建指定 k 的编码器（k 语义与 RLE+Golomb 一致）
    pub fn new(k: u8) -> Self {
        CabacEncoder {
            rc: RangeEncoder::new(),
            probs: vec![INIT_PROB; N_CTX],
            k,
        }
    }

    /// 直方图多 k 精确竞争版本（与 RLE 路径同源策略）
    pub fn adaptive(values: &[i32]) -> Self {
        // 符号分离后幅值直方图 → 各 k 的商前缀总长取最小
        use std::collections::HashMap;
        let mut hist: HashMap<u64, u64> = HashMap::new();
        for &v in values {
            if v != 0 {
                *hist.entry(v.unsigned_abs() as u64).or_insert(0) += 1;
            }
        }
        let mut best_k = 0u8;
        let mut best_len = u64::MAX;
        for k in 0u8..=6 {
            let ku = k as u64;
            let mut total: u64 = 0;
            for (&m, &cnt) in &hist {
                total += cnt * ((m >> ku) + 1 + ku);
            }
            if total < best_len {
                best_len = total;
                best_k = k;
            }
        }
        Self::new(best_k)
    }

    /// 获取 k 值（写入帧头 coding_params）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn k(&self) -> u8 {
        self.k
    }

    #[inline]
    fn bit(&mut self, b: bool, ctx: usize) {
        self.rc.encode_bit(b, &mut self.probs[ctx]);
    }

    #[inline]
    fn direct_bit(&mut self, b: bool) {
        self.rc.encode_direct(b);
    }

    /// 行程长度编码（分桶直通式，规避概率模型下限）
    fn enc_exp_golomb(&mut self, run: u32) {
        let v = run + 1;
        let m = 31 - v.leading_zeros();
        if m < 15 {
            self.bit(false, crate::crf::core::entropy::context::ctx_run_lead_pub(0));
            for i in (0..4).rev() {
                self.direct_bit((m >> i) & 1 == 1);
            }
        } else {
            self.bit(true, crate::crf::core::entropy::context::ctx_run_lead_pub(0));
            let mm = m - 15;
            for i in (0..5).rev() {
                self.direct_bit((mm >> i) & 1 == 1);
            }
        }
        // 低 m 位数据直写（最高位 1 隐含于 m 中）
        for i in (0..m).rev() {
            self.direct_bit((v >> i) & 1 == 1);
        }
    }

    /// 编码有符号残差数组（v3：统一 CtxModel 分发）
    ///
    /// 三种分类器（MA 树 / 固定梯度 / 全帧统一）由 [`CtxModel`] 枚举
    /// 承载，逐像素 classify 得到各 bin 的上下文基址。
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    fn encode_signed_array(
        &mut self,
        values: &[i32],
        k: u8,
        model: &CtxModel<'_>,
        stride: Option<usize>,
    ) {
        self.encode_signed_array_limited(values, k, model, stride, usize::MAX);
    }

    /// Fast-Fail 变体（v1.11 速度优化）：`byte_limit` 为输出字节上限，
    /// range coder 的 out 字节数单调递增——超限即必败，返回 None 终止。
    /// 被淘汰候选的产物本就不会进入最终码流；Some(()) 时与无限制
    /// 版本逐位一致。
    fn encode_signed_array_limited(
        &mut self,
        values: &[i32],
        k: u8,
        model: &CtxModel<'_>,
        stride: Option<usize>,
        byte_limit: usize,
    ) -> Option<()> {
        let mask = if k >= 32 { u32::MAX } else { (1u32 << k) - 1 };
        let mut i = 0usize;
        let mut left_nonzero = false;

        // 因果属性：attrs(i) 由 |values[i-1]|、|values[i-stride]|、
        // |values[i-stride-1]| 与 |values[i-stride+1]| 推导（v1.11 第四维
        // 右上对角属性），编码端/解码端可从已处理数据一致重现（不引入边信息）。
        // 注意 tr 仅在 st ≥ 2 时有效（st=1 时 i+1-st=i 为当前自身，非因果）。
        let attrs_at = |pos: usize| -> (u32, u32, u32, u32) {
            match stride {
                None => (0, 0, 0, 0),
                Some(0) => (0, 0, 0, 0),
                Some(st) => {
                    let l = if pos >= 1 {
                        values[pos - 1].unsigned_abs()
                    } else {
                        0
                    };
                    let t = if pos >= st {
                        values[pos - st].unsigned_abs()
                    } else {
                        0
                    };
                    let tl = if pos > st {
                        values[pos - st - 1].unsigned_abs()
                    } else {
                        0
                    };
                    let tr = if st >= 2 && pos + 1 >= st {
                        values[pos + 1 - st].unsigned_abs()
                    } else {
                        0
                    };
                    (l.min(255), t.min(255), tl.min(255), tr.min(255))
                }
            }
        };

        while i < values.len() {
            // Fast-Fail 检查：range coder 输出字节（cache_size 含未冲刷部分）
            if self.rc.out_len() >= byte_limit {
                return None;
            }
            let (l, t, tl, tr) = attrs_at(i);
            let ctx = model.classify(l, t, tl, tr, left_nonzero);
            if values[i] == 0 {
                let mut run = 0u32;
                while i < values.len() && values[i] == 0 {
                    run += 1;
                    i += 1;
                }
                self.bit(true, ctx.escape);
                self.enc_exp_golomb(run);
                left_nonzero = false;
            } else {
                self.bit(false, ctx.escape);
                left_nonzero = true;

                let v = values[i];
                let neg = v < 0;
                self.bit(neg, ctx.sign);

                let mag = v.unsigned_abs();
                let q = mag >> k;
                for step in 0..q {
                    self.bit(true, ctx.val_q_base + (step as usize).min(3));
                }
                self.bit(false, ctx.val_q_base + (q as usize).min(3));
                let r = mag & mask;
                for j in (0..k).rev() {
                    self.direct_bit((r >> j) & 1 == 1);
                }
                i += 1;
            }
        }
        Some(())
    }

    pub fn finish(self) -> Vec<u8> {
        self.rc.finish()
    }
}

/// 编码残差帧数据（RLE+CABAC 自适应 k，v3 三模式竞争）
///
/// **三变体竞争（单调不劣化）**：同时生成「MA 树分级」「固定梯度 4 档」
/// 「全帧统一」三个码流变体（含各自头部开销），取字节数最小者。
/// 载荷首字节 flags：bit0=MA 树启用、bit1=梯度模式启用，解码端据此分派。
///
/// `stride`：Some(n) 时 MA 与 Gradient 可用；None 时仅 Uniform 可用
/// （非空间域载荷，如 DCT 系数流）。
///
/// 返回 (body, k)：body = [flags][(ma_tree 头)][cabac 码流]，
/// 最终帧内载荷 = [k][body]。
pub fn encode_frame_rle_cabac_adaptive(
    pixels: &[i32],
    stride: Option<usize>,
) -> crate::crf::error::CrfResult<(Vec<u8>, u8)> {
    // usize::MAX 上限下 Fast-Fail 永不触发，与无限制版本逐位一致
    let payload = encode_frame_rle_cabac_adaptive_limited(pixels, stride, usize::MAX)?;
    Ok(payload.expect("usize::MAX 限制下必然产出载荷"))
}

/// [`encode_frame_rle_cabac_adaptive`] 的 Fast-Fail 变体（v1.11 速度优化）
///
/// `byte_limit` 为 body 字节上限：**每个变体独立短路**——某变体编码中途
/// 超限即终止该变体（超限必败），但绝不能因一个变体超限而放弃其余变体
/// （历史缺陷：MA 变体含树头开销较大，其超限不代表 Uniform 变体会超限；
/// 提前返回会误杀 CABAC 候选导致总体积回退）。全部变体均失败时才返回
/// None；任一成功则与无限制版本逐位一致。
#[allow(unused_assignments)] // 竞争胜出时才赋值；末次赋值不回读属正常控制流
pub fn encode_frame_rle_cabac_adaptive_limited(
    pixels: &[i32],
    stride: Option<usize>,
    byte_limit: usize,
) -> crate::crf::error::CrfResult<Option<(Vec<u8>, u8)>> {
    use crate::crf::core::entropy::context::build_ma_tree;

    // 共用 k（同像素集直方图竞争结果一致）
    let probe = CabacEncoder::adaptive(pixels);
    let k = probe.k;

    // P1 首帧加速：三变体并行。串行版的收缩 best_len 只对后续变体收紧
    // Fast-Fail 上限，但该上限仅淘汰「必败」变体——out_len() 是最终体积的
    // 下界，超限即最终体积 ≥ 上限，严格 < 竞争必败；胜出者恒为严格最小
    // 体积的变体，与上限无关。故并行给每个变体 byte_limit（最宽松）作独立
    // 预算，rayon collect 保序后按 MA→Gradient→Uniform 顺序严格 < 归约，
    // 字节逐位一致。
    let variants: Vec<u8> = if stride.is_some() {
        vec![0, 1, 2] // MA / Gradient / Uniform
    } else {
        vec![2] // 非空间域载荷（如 DCT 系数流）仅 Uniform
    };
    let results: Vec<Option<(Vec<u8>, usize)>> = variants
        .par_iter()
        .map(|&variant| -> crate::crf::error::CrfResult<Option<(Vec<u8>, usize)>> {
            encode_cabac_variant(pixels, k, stride, byte_limit, variant)
        })
        .collect::<crate::crf::error::CrfResult<Vec<_>>>()?;

    let mut best: Option<(Vec<u8>, usize)> = None;
    let mut best_len = byte_limit;
    for r in results {
        if let Some((body, total)) = r {
            if total < best_len {
                best_len = total;
                best = Some((body, total));
            }
        }
    }
    Ok(best.map(|(body, _)| (body, k)))
}

/// 编码单个 CABAC 上下文变体（0=MA 树 / 1=固定梯度 / 2=全帧统一），
/// 返回完整 body（含 flags 字节与 MA 树头）与总体积；超 `byte_limit`
/// 返回 None（该变体必败）。
///
/// 每个变体使用 `byte_limit`（最宽松）作独立 Fast-Fail 预算——`out_len()`
/// 为最终体积下界，超限即必败；收紧为串行版收缩 best_len 只是提前淘汰
/// 败者、节省败者编码时间，不改变胜出变体的字节。故并行版与串行版
/// 逐字节一致。
fn encode_cabac_variant(
    pixels: &[i32],
    k: u8,
    stride: Option<usize>,
    byte_limit: usize,
    variant: u8,
) -> crate::crf::error::CrfResult<Option<(Vec<u8>, usize)>> {
    use crate::crf::core::entropy::context::{build_ma_tree, CtxModel};

    match variant {
        0 => {
            // 变体 1：MA 树（仅空间域可用）
            let Some(st) = stride else {
                return Ok(None);
            };
            let tree = build_ma_tree(pixels, Some(st))?;
            let ma_header = tree.serialize();
            let model = CtxModel::Ma(&tree);
            let mut enc = CabacEncoder::new(k);
            let stream_limit = byte_limit.saturating_sub(1 + ma_header.len());
            if enc
                .encode_signed_array_limited(pixels, k, &model, stride, stream_limit)
                .is_none()
            {
                return Ok(None);
            }
            let stream = enc.finish();
            let total = 1 + ma_header.len() + stream.len();
            if total >= byte_limit {
                return Ok(None);
            }
            let mut body = Vec::with_capacity(total);
            body.push(0x01); // flags: MA
            body.extend_from_slice(&ma_header);
            body.extend_from_slice(&stream);
            Ok(Some((body, total)))
        }
        1 => {
            // 变体 2：固定梯度 4 档（仅空间域可用）
            let model = CtxModel::Gradient;
            let mut enc = CabacEncoder::new(k);
            let stream_limit = byte_limit.saturating_sub(1);
            if enc
                .encode_signed_array_limited(pixels, k, &model, stride, stream_limit)
                .is_none()
            {
                return Ok(None);
            }
            let stream = enc.finish();
            let total = 1 + stream.len();
            if total >= byte_limit {
                return Ok(None);
            }
            let mut body = Vec::with_capacity(total);
            body.push(0x02); // flags: Gradient
            body.extend_from_slice(&stream);
            Ok(Some((body, total)))
        }
        _ => {
            // 变体 3：全帧统一（恒可用，无树头/额外开销）
            let model = CtxModel::Uniform;
            let mut enc = CabacEncoder::new(k);
            let stream_limit = byte_limit.saturating_sub(1);
            if enc
                .encode_signed_array_limited(pixels, k, &model, stride, stream_limit)
                .is_none()
            {
                return Ok(None);
            }
            let stream = enc.finish();
            let total = 1 + stream.len();
            if total >= byte_limit {
                return Ok(None);
            }
            let mut body = Vec::with_capacity(total);
            body.push(0x00); // flags: Uniform
            body.extend_from_slice(&stream);
            Ok(Some((body, total)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crf::decoder::rle_cabac::{decode_frame_rle_cabac, RangeDecoder};

    /// Fast-Fail 等价性：limited(巨大上限) 必须与 unlimited 逐位一致。
    #[test]
    fn test_cabac_limited_equivalence() {
        let mut state = 0xABCDEFu64;
        let values: Vec<i32> = (0..30000)
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
        let (uncapped, k1) = encode_frame_rle_cabac_adaptive(&values, None).unwrap();
        let (capped, k2) = encode_frame_rle_cabac_adaptive_limited(&values, None, usize::MAX)
            .unwrap()
            .expect("MAX 上限下不应失败");
        assert_eq!(k1, k2);
        assert_eq!(uncapped, capped, "limited(usize::MAX) 与 unlimited 不一致");
    }

    /// 组装完整 v3 载荷 [k][flags][(tree)][stream] 并返回 (载荷, k)
    fn build_v3_payload(values: &[i32], stride: Option<usize>) -> (Vec<u8>, u8) {
        let (body, k) = encode_frame_rle_cabac_adaptive(values, stride).unwrap();
        let mut full = Vec::with_capacity(body.len() + 1);
        full.push(k);
        full.extend_from_slice(&body);
        (full, k)
    }

    fn roundtrip(values: &[i32], stride: Option<usize>) {
        let (full, k) = build_v3_payload(values, stride);
        let decoded = decode_frame_rle_cabac(&full[1..], k, values.len(), stride);
        assert_eq!(values, decoded, "CABAC 往返失败");
    }

    #[test]
    fn test_cabac_roundtrip_zeros() {
        roundtrip(&vec![0i32; 5000], None);
    }

    #[test]
    fn test_cabac_roundtrip_mixed() {
        roundtrip(&[0, 0, 0, 1, 2, -1, 0, 0, 5, -3, 0, 0, 0, 0, 0, 10], None);
    }

    #[test]
    fn test_cabac_roundtrip_random_uniform() {
        let mut state: u64 = 0x0123456789ABCDEF;
        let mut values = Vec::with_capacity(20000);
        for _ in 0..20000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((state >> 33) as i32 % 512) - 256;
            if state >> 40 & 3 != 0 {
                values.push(0);
            } else {
                values.push(v / 16);
            }
        }
        roundtrip(&values, None);
    }

    #[test]
    fn test_cabac_roundtrip_random_with_stride() {
        // 带 stride 的空间域往返：MA / Gradient / Uniform 三分类器全覆盖
        let width = 97usize;
        let height = 61usize;
        let mut state: u64 = 0xFEED_FACE_CAFE_BABE;
        let mut values = Vec::with_capacity(width * height);
        for _ in 0..width * height {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((state >> 33) as i32 % 256) - 128;
            if state >> 45 & 7 != 0 {
                values.push(0);
            } else {
                values.push(v / 8);
            }
        }
        roundtrip(&values, Some(width));
    }

    #[test]
    fn test_cabac_beats_static_golomb_on_skewed() {
        // 真实规模的偏斜分布：大量长零行程 + 稀疏小幅值非零值。
        // 三模式竞争下 CABAC 输出不应大于静态 Golomb-Rice 版本。
        let mut values = Vec::new();
        for g in 0..1000 {
            values.extend_from_slice(&[0i32; 500]);
            values.push((g % 7) as i32 + 1);
            values.extend_from_slice(&[0i32; 37]);
            values.push((g % 3) as i32 - 1);
        }
        let (cabac_data, _) = encode_frame_rle_cabac_adaptive(&values, None).unwrap();

        use crate::crf::encoder::rle_golomb::encode_frame_rle_golomb_adaptive;
        let (golomb_data, _) = encode_frame_rle_golomb_adaptive(&values).unwrap();

        println!(
            "skewed: cabac={} golomb={}",
            cabac_data.len(),
            golomb_data.len()
        );

        assert!(
            cabac_data.len() <= golomb_data.len(),
            "偏斜分布下 CABAC({}) 应不劣于 Golomb({})",
            cabac_data.len(),
            golomb_data.len()
        );
    }
}
