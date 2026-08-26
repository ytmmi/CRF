//! 有损精细参数 V2：稳定类型、纯解析、V1 兼容与用户侧 JSON/UI schema。

mod builder;
mod json;
mod kernel;
mod resolve;
mod types;
mod ui;

pub use builder::LossyOptionsV2Builder;
pub use kernel::KernelLossyConfig;
pub use resolve::{ConfigError, ResolveContext};
pub use types::*;
pub use ui::{expert_panel_schema, ExpertField, ExpertPanel};
