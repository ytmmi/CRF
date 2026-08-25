//! RLE+CABAC 混合熵解码器（frame_type=5）
//!
//! 与 encoder/rle_cabac.rs 严格对称：
//! - 同一套 9 上下文自适应概率模型（初始值与更新率一致）；
//! - escape / 前缀位走算术解码，余数/数据位走等概率直通。

// ===== Range Coder 解码端 =====

const RC_BITS: u32 = 12;
const RC_MOVE: u32 = 5;
const RC_TOP: u32 = 1 << 24;

pub struct RangeDecoder<'a> {
    range: u32,
    code: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    /// 初始化：跳过首字节（恒为 0），读入 4 字节大端码字
    pub fn new(data: &'a [u8]) -> Self {
        let mut code = 0u32;
        for i in 1..5 {
            code = (code << 8) | *data.get(i).unwrap_or(&0) as u32;
        }
        RangeDecoder {
            range: u32::MAX,
            code,
            data,
            pos: 5,
        }
    }

    #[inline]
    fn next_byte(&mut self) -> u32 {
        let b = *self.data.get(self.pos).unwrap_or(&0) as u32;
        self.pos += 1;
        b
    }

    #[inline]
    fn normalize(&mut self) {
        while self.range < RC_TOP {
            self.code = (self.code << 8) | self.next_byte();
            self.range <<= 8;
        }
    }

    #[inline]
    pub fn decode_bit(&mut self, prob: &mut u16) -> bool {
        let bound = (self.range >> RC_BITS) * (*prob as u32);
        let bit = self.code < bound; // bit=1 占下方子区间（与编码端一致）
        if bit {
            self.range = bound;
            *prob += ((1 << RC_BITS) - *prob) >> RC_MOVE;
        } else {
            self.code -= bound;
            self.range -= bound;
            *prob -= *prob >> RC_MOVE;
        }
        self.normalize();
        bit
    }

    #[inline]
    pub fn decode_direct(&mut self) -> bool {
        self.range >>= 1;
        let bit = self.code >= self.range;
        if bit {
            self.code -= self.range;
        }
        self.normalize();
        bit
    }
}

// ===== 上下文布局 v3（MA 树叶子槽位，共 60 个；与编码器一致）=====
//
// 下述常量与辅助函数构成与编码端逐一对称的完整上下文模型；
// 部分槽位当前主路径未消费，作为格式定义面对称保留。
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
pub(crate) const CTX_ESCAPE: usize = 0; // [0, 8)
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
const CTX_VAL_Q: usize = 8; // [8, 40)
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
const CTX_SIGN: usize = 40; // [40, 56)
pub(crate) const CTX_RUN_LEAD: usize = 56; // [56, 60)
pub(crate) const N_CTX: usize = 60;

#[inline]
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
fn ctx_escape(leaf: usize) -> usize {
    CTX_ESCAPE + leaf
}

#[inline]
fn ctx_run_lead(m: u32) -> usize {
    CTX_RUN_LEAD + m.min(3) as usize
}

#[inline]
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
fn ctx_val_q(q_so_far: u32, leaf: usize) -> usize {
    CTX_VAL_Q + leaf * 4 + q_so_far.min(3) as usize
}

#[inline]
#[allow(dead_code)] // 与编码端上下文布局对称保留（格式定义面）
fn ctx_sign(leaf: usize) -> usize {
    CTX_SIGN + leaf
}

/// RLE+CABAC 解码器封装
pub struct CabacDecoder<'a> {
    rc: RangeDecoder<'a>,
    probs: [u16; N_CTX],
    k: u8,
}

impl<'a> CabacDecoder<'a> {
    pub fn new(data: &'a [u8], k: u8) -> Self {
        CabacDecoder {
            rc: RangeDecoder::new(data),
            probs: [2048; N_CTX],
            k,
        }
    }

    #[inline]
    fn bit(&mut self, ctx: usize) -> bool {
        self.rc.decode_bit(&mut self.probs[ctx])
    }

    #[inline]
    fn direct_bit(&mut self) -> bool {
        self.rc.decode_direct()
    }

    /// 行程长度解码（分桶直通式，与编码器 enc_exp_golomb 对称）
    fn dec_exp_golomb(&mut self) -> u32 {
        let m: u32 = if !self.bit(ctx_run_lead(0)) {
            let mut mv = 0u32;
            for _ in 0..4 {
                mv = (mv << 1) | (self.direct_bit() as u32);
            }
            mv
        } else {
            let mut mv = 0u32;
            for _ in 0..5 {
                mv = (mv << 1) | (self.direct_bit() as u32);
            }
            mv + 15
        };
        if m == 0 {
            return 0; // 防御：行程最小为 1，正常流不应出现
        }
        let mut value = 1u32;
        for _ in 0..m {
            value = (value << 1) | (self.direct_bit() as u32);
        }
        value - 1
    }

    /// 解码有符号残差数组（v3：统一 CtxModel 分发）
    ///
    /// `model`：上下文分类器（MA 树 / 固定梯度 / 全帧统一），
    /// 由载荷 flags 分派；须与编码端一致。
    /// `stride`：行距，用于因果属性（|left|、|top|）；须与编码端一致；
    /// None 时属性恒为 0（非空间域载荷）。
    pub fn decode_signed_array(
        &mut self,
        count: usize,
        model: &crate::crf::encoder::ma_tree::CtxModel<'_>,
        stride: Option<usize>,
    ) -> Vec<i32> {
        let mut out = Vec::with_capacity(count);
        let mut remaining = count;
        let mut left_nonzero = false;

        // 已解码元素的绝对值缓存（用于因果属性推导；v1.11 含右上对角
        // 第四维，与编码端 attrs_at 逐位对称——任何不对称都会破坏无损往返）。
        // tr 仅在 st ≥ 2 时有效（st=1 时引用自身非因果）。
        let attrs_at = |out: &Vec<i32>, pos: usize| -> (u32, u32, u32, u32) {
            match stride {
                None => (0, 0, 0, 0),
                Some(0) => (0, 0, 0, 0),
                Some(st) => {
                    let l = if pos >= 1 {
                        out[pos - 1].unsigned_abs()
                    } else {
                        0
                    };
                    let t = if pos >= st {
                        out[pos - st].unsigned_abs()
                    } else {
                        0
                    };
                    let tl = if pos > st {
                        out[pos - st - 1].unsigned_abs()
                    } else {
                        0
                    };
                    let tr = if st >= 2 && pos + 1 >= st {
                        out[pos + 1 - st].unsigned_abs()
                    } else {
                        0
                    };
                    (l.min(255), t.min(255), tl.min(255), tr.min(255))
                }
            }
        };

        let mut pos = 0usize;
        while remaining > 0 {
            let (l, t, tl, tr) = attrs_at(&out, pos);
            let ctx = model.classify(l, t, tl, tr, left_nonzero);
            if self.bit(ctx.escape) {
                // 零行程
                let run = self.dec_exp_golomb() as usize;
                let actual = run.min(remaining);
                out.resize(out.len() + actual, 0);
                pos += actual;
                remaining -= actual;
                left_nonzero = false;
            } else {
                left_nonzero = true;
                // 符号位
                let neg = self.bit(ctx.sign);
                // 幅值 Golomb 商前缀 + 终止 0
                let mut q: u32 = 0;
                loop {
                    if !self.bit(ctx.val_q_base + (q as usize).min(3)) {
                        break;
                    }
                    q += 1;
                    if q > (1 << 26) {
                        out.push(0); // 损坏数据防护
                        pos += 1;
                        remaining -= 1;
                        break;
                    }
                }
                let mut mag = 0u32;
                for _ in 0..self.k {
                    mag = (mag << 1) | (self.direct_bit() as u32);
                }
                mag += q << self.k;
                out.push(if neg { -(mag as i32) } else { mag as i32 });
                pos += 1;
                remaining -= 1;
            }
        }

        out
    }
}

/// 解码残差帧数据（RLE+CABAC v3 三模式）
///
/// 载荷布局 v3：[flags u8][(ma_tree 头，仅 flags.bit0)][cabac 码流]
/// （k 由调用方从载荷首字节提取后剥离——见 decoder/mod.rs 的
/// frame_type=5 分支）。flags：bit0=MA 树分级变体、bit1=固定梯度变体、
/// 全零=全帧统一变体。
///
/// `stride`：与编码端一致；None 用于非空间域载荷。
pub fn decode_frame_rle_cabac(
    data: &[u8],
    k: u8,
    pixel_count: usize,
    stride: Option<usize>,
) -> Vec<i32> {
    use crate::crf::encoder::ma_tree::{CtxModel, MaTree};

    if data.is_empty() {
        return vec![0; pixel_count]; // 损坏防护
    }
    let flags = data[0];
    let body = &data[1..];

    let model = if flags & 0x01 != 0 {
        // MA 变体：解析树头
        let (tree, header_len) = MaTree::deserialize(body).expect("损坏的 MA 树头");
        let mut dec = CabacDecoder::new(&body[header_len..], k);
        return dec.decode_signed_array(pixel_count, &CtxModel::Ma(&tree), stride);
    } else if flags & 0x02 != 0 {
        CtxModel::Gradient
    } else {
        CtxModel::Uniform
    };

    let mut dec = CabacDecoder::new(body, k);
    dec.decode_signed_array(pixel_count, &model, stride)
}
