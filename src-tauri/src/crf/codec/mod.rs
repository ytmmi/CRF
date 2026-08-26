//! 公共 codec facade —— 对外稳定入口层
//!
//! 本模块是应用层（Tauri commands / CLI / batch / streaming）与内部编解码实现
//! 之间的唯一稳定边界。职责严格限定为：
//!
//! - 输入请求校验与转换（[`EncodeRequest`] / [`DecodeRequest`]）；
//! - 配置解析与冻结（[`crate::crf::core::config::ResolvedConfig`]）；
//! - 编码器/解码器 session 生命周期编排；
//! - 取消、进度、错误边界与结果出口。
//!
//! 禁止在本层出现预测、变换、熵编码、容器读写或设备 API（CUDA/HIP/Vulkan）。
//! 算法实现全部下沉到 [`crate::crf::encoder`]、[`crate::crf::decoder`] 与
//! [`crate::crf::core`]。
//!
//! **迁移状态（P0）**：当前为空骨架，仅定义请求类型与 re-export。
//! 旧入口 `crate::crf::encoder::encode_sequence`、`crate::crf::decoder::decode_from_bytes`
//! 仍为生产路径；P3 阶段 session 拆分完成后，旧入口将转发到本 facade。
//!
//! 参考：[编解码器分层重构规划](../../../../docs/codec-architecture-refactor-plan.md) §4.1 / §5.1

pub mod decode;
pub mod encode;
pub mod error;

pub use decode::{decode_from_bytes, decode_from_reader, DecodeRequest};
pub use encode::{encode, encode_to_writer, EncodeRequest, EncodeReport};

// 对外错误类型（公共 API 面）
pub use error::CodecError;
