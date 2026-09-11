//! prediction —— 预测契约
//!
//! 规划文档 §3.5。包含：
//! - intra.rs：帧内预测与逆预测（apply/undo/band，原 format/prediction.rs）
//! - mode.rs：PredictionMode 及合法值（已迁 core/domain/types.rs）
//! - residual.rs：预测残差与逆预测契约
//! - intrabc.rs：COPY/PRED 语义
//! - context.rs：因果邻域、边界和邻块上下文
//! - cost.rs：SAD/SATD/梯度代价接口
//!
//! 预测函数必须明确输入是原始邻域还是 reconstructed 邻域。正式编码路径只能
//! 引用允许的已重建数据；探针可以读取原图，但不得复用为生产函数。
//!
//! **迁移状态（P4）**：`intra` 模块已迁入（原 `format/prediction.rs`）。
//! `cost` 模块已迁入（原 `format/cost.rs`）。旧 `format/` 目录已删除。

/// cost：预测模式代价估计（P4 迁入，原 format/cost.rs）
pub mod cost;
/// intra：帧内预测与逆预测（P4 迁入，原 format/prediction.rs）
pub mod intra;
