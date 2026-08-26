//! 上下文模型定义和更新契约（原 encoder/ma_tree.rs）
//!
//! MA 树上下文建模（Meta-Adaptive tree，JPEG-XL 同款思路）
//!
//! 手工固定的梯度分档（v2：4 档均匀切分）无法适配所有内容形态；
//! MA 树让上下文划分**由数据驱动**：每个内部节点是一个形如
//! `attr < threshold` 的二叉判定（属性来自因果邻居残差幅度），
//! 叶子对应一个独立的概率模型槽。
//!
//! - 编码端：对差分场采样后贪心生长（最大化不纯度下降），树结构
//!   序列化进 frame_type=5 载荷头；
//! - 解码端：重建同一棵树，逐像素沿树下走得到上下文槽位——
//!   属性仅依赖已解码邻居，两端路径必然一致。
//!
//! 参数：最大深度 3（≤8 叶子）、最小分裂样本 256、单遍采样 ≤16K 样本。
//! 训练开销毫秒级，树头部 ≤ 75 字节。
//!
//! **迁移说明（P1）**：本模块原位于 `encoder/ma_tree.rs`，因 decoder
//! 生产路径（`decoder/rle_cabac.rs`）直接引用 `CtxModel` 导致
//! decoder → encoder 反向依赖。现迁移到公共 `core/entropy/context`，
//! encoder 与 decoder 均通过公共契约访问。`encoder/ma_tree.rs` 保留为
//! `pub use` 转发层，迁移完成后删除。

use crate::crf::core::bitstream::constants::BAND_HEIGHT;

/// 最大树深（根为 0）：≤ 2^3 = 8 叶子
pub const MA_MAX_DEPTH: usize = 3;
/// 节点总数上限（满二叉树 2^(d+1)−1）
pub const MA_MAX_NODES: usize = 15;
/// 最小分裂样本数（低于此值不再分裂）
const MA_MIN_SAMPLES: usize = 192;
/// 最小不纯度下降（bits/样本），低于此值停止分裂
const MA_MIN_GAIN: f64 = 0.04;

/// 分裂属性编号
pub const MA_ATTR_LEFT: u8 = 0; // |left residual|
pub const MA_ATTR_TOP: u8 = 1; // |top residual|
/// v1.9 新增：|top_left residual|——对角邻居属性使水平边缘与斜边缘
/// 在属性空间可分（二者 |left|/|top| 分布相同，但 |top_left| 截然不同），
/// 是 JPEG-XL MA 树实践中对边缘走向区分度贡献最大的第三维。
pub const MA_ATTR_TOPLEFT: u8 = 2;
/// v1.11 新增：|top_right residual|——右上对角维度，与 TopRight(45°)
/// 斜向预测呼应，补全 "/" 走向边缘的上下文区分度。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub const MA_ATTR_TOPRIGHT: u8 = 3;

/// 属性数量
pub const MA_ATTR_COUNT: usize = 4;

/// 幅值桶数（熵评估用）：{0, 1-4, 5-16, 17-64, >64}
const MAG_BUCKETS: usize = 5;

#[inline]
fn mag_bucket(abs_v: u32) -> usize {
    match abs_v {
        0 => 0,
        1..=4 => 1,
        5..=16 => 2,
        17..=64 => 3,
        _ => 4,
    }
}

/// MA 树节点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaNode {
    /// bit7 = is_leaf
    pub flags: u8,
    /// 内部节点：分裂属性（MA_ATTR_*）；叶子：未用
    pub attr: u8,
    /// 内部节点：分裂阈值（attr < threshold → left 子树）
    pub threshold: u8,
    /// 内部节点：左右子节点索引（BFS 序）；叶子：未用
    pub children: [u8; 2],
}

impl MaNode {
    pub fn is_leaf(&self) -> bool {
        self.flags & 0x80 != 0
    }

    fn leaf() -> Self {
        MaNode {
            flags: 0x80,
            attr: 0,
            threshold: 0,
            children: [0, 0],
        }
    }

    fn internal(attr: u8, threshold: u32, left: u8, right: u8) -> Self {
        MaNode {
            flags: 0,
            attr,
            threshold: threshold.clamp(0, 255) as u8,
            children: [left, right],
        }
    }
}

/// 最大叶子数（= 2^MA_MAX_DEPTH）
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub const MA_MAX_LEAVES: usize = 8;

/// MA 树
#[derive(Debug, Clone)]
pub struct MaTree {
    pub nodes: Vec<MaNode>,
    /// node_idx → 叶子槽位（0..leaf_count）；内部节点的项未用
    pub leaf_slots: Vec<u8>,
}

impl MaTree {
    /// 单叶子退化树（等价于无上下文分级）
    pub fn single_leaf() -> Self {
        MaTree {
            nodes: vec![MaNode::leaf()],
            leaf_slots: vec![0],
        }
    }

    /// 逐像素走树：`attrs` = [left, top, topleft, topright]（缺失补 0）
    ///
    /// 返回叶子槽位（0..leaf_count），直接用作上下文维度基址。
    #[inline]
    pub fn walk(&self, left_abs: u32, top_abs: u32, topleft_abs: u32, topright_abs: u32) -> usize {
        let mut n = 0usize;
        while !self.nodes[n].is_leaf() {
            let node = &self.nodes[n];
            let v = match node.attr {
                MA_ATTR_LEFT => left_abs,
                MA_ATTR_TOP => top_abs,
                MA_ATTR_TOPLEFT => topleft_abs,
                _ => topright_abs,
            };
            n = if v < node.threshold as u32 {
                node.children[0] as usize
            } else {
                node.children[1] as usize
            };
        }
        self.leaf_slots[n] as usize // 叶子槽位（≤ MA_MAX_LEAVES）
    }

    /// 叶子数量（flags 判定）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn leaf_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_leaf()).count()
    }

    /// 构建后整理：为每个叶子分配连续槽位（DFS 遇到顺序）
    pub(crate) fn with_leaf_slots(nodes: Vec<MaNode>) -> Self {
        let mut slots = vec![0u8; nodes.len()];
        let mut next = 0u8;
        for (i, node) in nodes.iter().enumerate() {
            if node.is_leaf() {
                slots[i] = next;
                next += 1;
            }
        }
        MaTree {
            nodes,
            leaf_slots: slots,
        }
    }

    /// 序列化到载荷头（BFS 顺序）
    ///
    /// 格式：[node_count u8][逐节点: flags u8 + (内部: attr u8 + threshold u8 +
    /// left u8 + right u8) | (叶子: 无附加)]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![self.nodes.len().min(255) as u8];
        for node in &self.nodes {
            out.push(node.flags);
            if !node.is_leaf() {
                out.push(node.attr);
                out.push(node.threshold);
                out.push(node.children[0]);
                out.push(node.children[1]);
            }
        }
        out
    }

    /// 从载荷头反序列化（解码端重建同一棵树）
    pub fn deserialize(data: &[u8]) -> Option<(MaTree, usize)> {
        if data.is_empty() {
            return None;
        }
        let count = data[0] as usize;
        if count == 0 || count > MA_MAX_NODES {
            return None;
        }
        let mut pos = 1usize;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            if pos >= data.len() {
                return None;
            }
            let flags = data[pos];
            pos += 1;
            if flags & 0x80 != 0 {
                nodes.push(MaNode::leaf());
            } else {
                if pos + 4 > data.len() {
                    return None;
                }
                let attr = data[pos];
                let threshold = data[pos + 1];
                let left = data[pos + 2];
                let right = data[pos + 3];
                pos += 4;
                if attr >= MA_ATTR_COUNT as u8 || left >= count as u8 || right >= count as u8 {
                    return None; // 损坏防护：非法属性/越界子节点
                }
                nodes.push(MaNode::internal(attr, threshold as u32, left, right));
            }
        }
        Some((MaTree::with_leaf_slots(nodes), pos))
    }
}

// ===== 训练 =====

/// 训练样本：因果属性 + 标签
struct TrainSample {
    attrs: [u32; MA_ATTR_COUNT],
    bucket: usize, // mag_bucket(|v|)
}

/// 由差分场构建 MA 树（贪心不纯度下降）
///
/// `pixels`：预测后的残差流（与闭环量化输出同域）；`stride` 为行距，
/// None 时退化为单叶子树。
pub fn build_ma_tree(
    pixels: &[i32],
    stride: Option<usize>,
) -> crate::crf::error::CrfResult<MaTree> {
    let st = match stride {
        Some(s) if s > 0 => s,
        _ => return Ok(MaTree::single_leaf()),
    };

    // 采样：目标 ≤16K 样本
    let total = pixels.len();
    let sample_step = total.div_ceil(16384).max(1);

    let mut samples: Vec<TrainSample> = Vec::with_capacity(total / sample_step + 1);
    for i in (0..total).step_by(sample_step) {
        let l = if i >= 1 {
            pixels[i - 1].unsigned_abs()
        } else {
            0
        };
        let t = if i >= st {
            pixels[i - st].unsigned_abs()
        } else {
            0
        };
        // 对角邻居：与 left 同约定（序列相邻 i-1），组合偏移 i-st-1
        let tl = if i > st {
            pixels[i - st - 1].unsigned_abs()
        } else {
            0
        };
        // 右上对角邻居：组合偏移 i-(st-1)，i ≥ st-1 时有效
        let tr = if i + 1 >= st {
            pixels[i + 1 - st].unsigned_abs()
        } else {
            0
        };
        samples.push(TrainSample {
            attrs: [l, t, tl, tr],
            bucket: mag_bucket(pixels[i].unsigned_abs()),
        });
    }

    // 固定候选阈值（覆盖典型残差幅度谱）
    const CANDIDATE_THRESHOLDS: [u32; 4] = [2, 6, 14, 30];

    struct Builder<'a> {
        samples: &'a [TrainSample],
    }

    impl<'a> Builder<'a> {
        /// 子集不纯度：幅值桶分布的香农熵（bits）
        fn impurity(&self, idx: &[usize]) -> f64 {
            if idx.is_empty() {
                return 0.0;
            }
            let mut hist = [0u32; MAG_BUCKETS];
            for &i in idx {
                hist[self.samples[i].bucket] += 1;
            }
            let n = idx.len() as f64;
            hist.iter()
                .filter(|&&c| c > 0)
                .map(|&c| {
                    let p = c as f64 / n;
                    -p * p.log2()
                })
                .sum()
        }

        fn grow(
            &self,
            subset: &[usize],
            depth: usize,
            next_node: &mut usize,
            nodes: &mut Vec<MaNode>,
        ) -> u8 {
            let can_split = depth < MA_MAX_DEPTH
                && subset.len() >= MA_MIN_SAMPLES
                && *next_node + 2 <= MA_MAX_NODES;

            let mut best_gain = MA_MIN_GAIN;
            let mut best: Option<(u8, u32, Vec<usize>, Vec<usize>)> = None;

            if can_split {
                // 各属性的候选阈值 = 固定集 ∩ (min, max) 开区间内的有效值
                for attr in 0..MA_ATTR_COUNT {
                    let lo = subset
                        .iter()
                        .map(|&i| self.samples[i].attrs[attr])
                        .min()
                        .unwrap_or(0);
                    let hi = subset
                        .iter()
                        .map(|&i| self.samples[i].attrs[attr])
                        .max()
                        .unwrap_or(0);
                    if lo == hi {
                        continue; // 该属性在子集内无区分度
                    }
                    for &t in CANDIDATE_THRESHOLDS.iter() {
                        if t <= lo || t > hi {
                            continue;
                        }
                        let mut left_set = Vec::new();
                        let mut right_set = Vec::new();
                        for &i in subset {
                            if self.samples[i].attrs[attr] < t {
                                left_set.push(i);
                            } else {
                                right_set.push(i);
                            }
                        }
                        if left_set.len() < MA_MIN_SAMPLES / 4
                            || right_set.len() < MA_MIN_SAMPLES / 4
                        {
                            continue;
                        }
                        let parent_imp = self.impurity(subset);
                        let gain = parent_imp
                            - (self.impurity(&left_set) * left_set.len() as f64
                                + self.impurity(&right_set) * right_set.len() as f64)
                                / subset.len() as f64;
                        if gain > best_gain {
                            best_gain = gain;
                            best = Some((attr as u8, t, left_set, right_set));
                        }
                    }
                }
            }

            let my_idx = *next_node as u8;
            *next_node += 1;
            match best {
                Some((attr, t, left_set, right_set)) => {
                    // 占位以保持 BFS 索引稳定（children 稍后回填）
                    nodes.push(MaNode::internal(attr, t, 0, 0));
                    let li = self.grow(&left_set, depth + 1, next_node, nodes);
                    let ri = self.grow(&right_set, depth + 1, next_node, nodes);
                    nodes[my_idx as usize].children = [li, ri];
                    my_idx
                }
                None => {
                    nodes.push(MaNode::leaf());
                    my_idx
                }
            }
        }
    }

    let builder = Builder { samples: &samples };
    let all: Vec<usize> = (0..samples.len()).collect();
    let mut nodes: Vec<MaNode> = Vec::with_capacity(MA_MAX_NODES);
    let mut next_node = 0usize;
    builder.grow(&all, 0, &mut next_node, &mut nodes);

    Ok(MaTree::with_leaf_slots(nodes))
}

/// BAND_HEIGHT 引用占位（条带级扩展预留）
pub const _MA_BAND_HINT: usize = BAND_HEIGHT;

#[cfg(test)]
mod tests {
    use super::*;

    /// v1.9 第三属性（|top_left|）必须可被贪心训练选中。
    ///
    /// 构造「left/top 全程为零（无区分度）、唯 top_left 承载信息」的稀疏
    /// 对角簇场：每簇由对角相邻的两点组成——(bx,by)=100（其 tl=5），
    /// (bx−1,by−1)=5（其 tl=0）。因此「tl≥2」的子集恰好富集全部大值点，
    /// 而任何基于 left/top 的分裂均无法分离 bucket。
    #[test]
    fn test_trainer_selects_topleft_attribute() {
        let st = 32usize;
        let rows = 256; // 8×8 = 64 个大值点 ≥ 最小分裂样本/4
        let mut data = vec![0i32; st * rows];
        for br in 0..8 {
            for bc in 0..8 {
                let by = br * rows / 8 + 8;
                let bx = bc * st / 8 + 8;
                data[by * st + bx] = 100;
                data[(by - 1) * st + (bx - 1)] = 5;
            }
        }
        let tree = build_ma_tree(&data, Some(st)).unwrap();
        let has_topleft_split = tree
            .nodes
            .iter()
            .any(|n| !n.is_leaf() && n.attr == MA_ATTR_TOPLEFT);
        assert!(
            has_topleft_split,
            "训练器未选用 |top_left| 属性：树 {:?}",
            tree.nodes
                .iter()
                .map(|n| (n.attr, n.threshold))
                .collect::<Vec<_>>()
        );
        // 三属性树往返一致（walk 稳定性）
        for i in 0..data.len() {
            let l = if i >= 1 {
                data[i - 1].unsigned_abs().min(255)
            } else {
                0
            };
            let t = if i >= st {
                data[i - st].unsigned_abs().min(255)
            } else {
                0
            };
            let tl = if i > st {
                data[i - st - 1].unsigned_abs().min(255)
            } else {
                0
            };
            let tr = if st >= 2 && i + 1 >= st {
                data[i + 1 - st].unsigned_abs().min(255)
            } else {
                0
            };
            let leaf = tree.walk(l, t, tl, tr);
            assert!(leaf < tree.leaf_count());
        }
    }

    /// 反序列化校验随属性数联动：attr=2/3 新合法，attr=4 仍拒绝。
    #[test]
    fn test_deserialize_accepts_new_attr_rejects_unknown() {
        // 合法最小树：单内部节点用 attr=TOPLEFT
        let ok = vec![3u8, 0x00, MA_ATTR_TOPLEFT, 10, 1, 2, 0x80, 0x80];
        assert!(MaTree::deserialize(&ok).is_some());
        // 非法属性 attr=4 ≥ COUNT → 拒绝
        let bad = vec![3u8, 0x00, 4u8, 10, 1, 2, 0x80, 0x80];
        assert!(MaTree::deserialize(&bad).is_none());
    }
}

// ===== 上下文布局 v3 常量与分类器（编解码共享）=====
//
// 三种上下文分类器竞争（帧内取码流最小者，flags 编码所选模式）：
//
// | 模式     | 分类依据                           | escape | val_q      | sign |
// |----------|------------------------------------|--------|------------|------|
// | Ma       | MA 树叶子槽位（数据驱动划分）      | leaf   | leaf×4+q   | leaf |
// | Gradient | 固定梯度 4 档（|left|+|top| 分档） | nz×4+g | g×4+q      | g≥2  |
// | Uniform  | 单槽位（无分级）                   | 0      | q          | 0    |

pub const CTX_ESCAPE: usize = 0; // [0, 8)
const CTX_VAL_Q: usize = 8; // [8, 40)
const CTX_SIGN: usize = 40; // [40, 56)
pub const CTX_RUN_LEAD: usize = 56; // [56, 60)
pub const N_CTX: usize = 60;

/// 行程阶前导上下文（全模式共享，pub 供编码器使用）
#[inline]
pub fn ctx_run_lead_pub(m: u32) -> usize {
    CTX_RUN_LEAD + m.min(3) as usize
}

/// 因果梯度分级：左/上残差绝对值之和 → 4 档（Gradient 模式用）
#[inline]
pub fn grad_class(left_abs: u32, top_abs: u32) -> usize {
    let s = left_abs.saturating_add(top_abs);
    match s {
        0..=3 => 0,
        4..=15 => 1,
        16..=63 => 2,
        _ => 3,
    }
}

/// 上下文分类器：由因果邻居属性（左/上残差绝对值）推导各 bin 的
/// 上下文基址。编码端与解码端从已处理数据一致重现。
pub enum CtxModel<'a> {
    /// MA 树（JPEG-XL Meta-Adaptive）：数据驱动决策树
    Ma(&'a MaTree),
    /// 固定梯度 4 档
    Gradient,
    /// 全帧统一单档
    Uniform,
}

/// 一次分类的结果：各 bin 类型的上下文基址
#[derive(Debug, Clone, Copy)]
pub struct CtxIds {
    pub escape: usize,
    pub val_q_base: usize, // + q_so_far.min(3)
    pub sign: usize,
}

impl CtxModel<'_> {
    /// 分类：`left_nonzero` 仅 Gradient 模式的 escape 需要；
    /// `topleft_abs`/`topright_abs` 仅 MA 模式使用（v1.9/v1.11 对角属性）。
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn classify(
        &self,
        left_abs: u32,
        top_abs: u32,
        topleft_abs: u32,
        topright_abs: u32,
        left_nonzero: bool,
    ) -> CtxIds {
        match self {
            CtxModel::Ma(tree) => {
                let leaf = tree.walk(
                    left_abs.min(255),
                    top_abs.min(255),
                    topleft_abs.min(255),
                    topright_abs.min(255),
                );
                CtxIds {
                    escape: CTX_ESCAPE + leaf,
                    val_q_base: CTX_VAL_Q + leaf * 4,
                    sign: CTX_SIGN + leaf,
                }
            }
            CtxModel::Gradient => {
                let g = grad_class(left_abs, top_abs);
                CtxIds {
                    escape: CTX_ESCAPE + (if left_nonzero { 4 } else { 0 }) + g,
                    val_q_base: CTX_VAL_Q + g * 4,
                    sign: CTX_SIGN + if g >= 2 { 1 } else { 0 },
                }
            }
            CtxModel::Uniform => CtxIds {
                escape: CTX_ESCAPE,
                val_q_base: CTX_VAL_Q,
                sign: CTX_SIGN,
            },
        }
    }

    /// 变体标识字节（载荷 flags）：bit0=MA、bit1=Gradient、全零=Uniform
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn flags_byte(&self) -> u8 {
        match self {
            CtxModel::Ma(_) => 0x01,
            CtxModel::Gradient => 0x02,
            CtxModel::Uniform => 0x00,
        }
    }
}
