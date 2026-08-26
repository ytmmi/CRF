//! prediction —— 预测契约
//!
//! 规划文档 §3.5。包含：
//! - mode.rs：PredictionMode 及合法值
//! - intra.rs：DC/H/V/MED/Paeth/planar/D45/D135
//! - residual.rs：预测残差与逆预测契约
//! - intrabc.rs：COPY/PRED 语义
//! - context.rs：因果邻域、边界和邻块上下文
//! - cost.rs：SAD/SATD/梯度代价接口
//!
//! 预测函数必须明确输入是原始邻域还是 reconstructed 邻域。正式编码路径只能
//! 引用允许的已重建数据；探针可以读取原图，但不得复用为生产函数。
//!
//! **P0 状态**：空骨架。现有 `format/prediction.rs`（约 801 行，已预警）
//! 与 `format/types.rs` 的 `PredictionMode` 将在 P4 迁移到本模块。
//! `encoder/ma_tree.rs` 的 `CtxModel`（上下文模型）将迁移到 `core/entropy/context`。

#![allow(dead_code)]

// P4 阶段迁入子模块（当前为空声明）
