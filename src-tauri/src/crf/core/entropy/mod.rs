//! entropy —— 符号化和熵编码
//!
//! 规划文档 §3.7。包含：
//! - symbol.rs：signed/unsigned/zigzag/token 定义
//! - scan.rs：zigzag/directional scan/EOB
//! - rle.rs：零行程和 run token
//! - golomb.rs：Golomb/Rice/Exp-Golomb 语法
//! - cabac.rs：CABAC 状态与二值化
//! - coeff.rs：coefficient token stream
//! - context.rs：上下文模型定义和更新契约
//!
//! 建议把"语法定义"和"具体 encoder/decoder 状态机"分开：
//! - `entropy/*` 定义 token、状态转移和边界；
//! - `encoder/entropy_writer` 将系数/预测残差写入 token；
//! - `decoder/entropy_reader` 从 bounded bit reader 还原 token；
//! - 两侧共享向量化测试、黄金向量和错误分类，不互相调用私有实现。
//!
//! **迁移状态（P1）**：`context` 模块已迁入（原 `encoder/ma_tree.rs`），
//! 解除 `decoder/rle_cabac.rs` 对 `encoder::ma_tree` 的反向依赖。
//! `encoder/ma_tree.rs` 保留为 `pub use` 转发层，后续删除。
//! `format/zigzag.rs` 将在 P4 迁移到 `core/entropy/scan.rs`。

#![allow(dead_code)]

/// context：上下文模型定义和更新契约（CtxModel/CtxIds/MaTree）
pub mod context;
