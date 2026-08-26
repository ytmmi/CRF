//! 对外错误映射
//!
//! 将内部 `CrfError` 映射为 facade 层稳定错误类型，避免内部错误变体直接暴露给应用层。
//!
/// **迁移状态（P0）**：定义错误枚举。映射逻辑在 P2/P3 facade 接入时补全。

use crate::crf::error::CrfError;

/// codec facade 错误类型
#[derive(Debug)]
pub enum CodecError {
    /// 内部 CRF 错误（透传，后续阶段可细化为稳定错误码）
    Internal(CrfError),
    /// 功能尚未实现（迁移期占位）
    NotImplemented(&'static str),
    /// 输入校验失败
    InvalidInput(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Internal(e) => write!(f, "codec internal error: {}", e),
            CodecError::NotImplemented(what) => write!(f, "not implemented: {}", what),
            CodecError::InvalidInput(msg) => write!(f, "invalid input: {}", msg),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<CrfError> for CodecError {
    fn from(e: CrfError) -> Self {
        CodecError::Internal(e)
    }
}
