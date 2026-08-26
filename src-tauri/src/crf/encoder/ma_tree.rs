//! MA 树上下文建模 —— 已迁移到 `core::entropy::context`
//!
//! **迁移说明（P1）**：本文件原为 MA 树上下文建模的完整实现，因
//! `decoder/rle_cabac.rs` 生产路径引用 `CtxModel` 导致 decoder → encoder
//! 反向依赖。现已将全部内容迁移到 [`crate::crf::core::entropy::context`]。
//!
//! 本文件保留为 `pub use` 转发层，维持 `encoder::ma_tree::*` 旧路径兼容，
//! 避免大规模修改 encoder 内部引用。后续 P4/P6 阶段删除本文件，将
//! encoder 内部引用改为直接指向 `core::entropy::context`。
//!
//! 新代码请直接使用 [`crate::crf::core::entropy::context`]。

#[allow(unused_imports)]
pub use crate::crf::core::entropy::context::{
    build_ma_tree, ctx_run_lead_pub, grad_class, CtxIds, CtxModel, MaNode, MaTree, CTX_ESCAPE,
    CTX_RUN_LEAD, MA_ATTR_COUNT, MA_ATTR_LEFT, MA_ATTR_TOP, MA_ATTR_TOPLEFT, MA_ATTR_TOPRIGHT,
    MA_MAX_DEPTH, MA_MAX_LEAVES, MA_MAX_NODES, N_CTX, _MA_BAND_HINT,
};
