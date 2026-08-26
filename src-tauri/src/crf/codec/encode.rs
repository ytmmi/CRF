//! 编码 facade —— `EncodeRequest → EncodeReport`
//!
//! 职责（规划文档 §4.1）：
//! - 参数校验入口；
//! - `EncodeSession` 创建（P3 接入）；
//! - 取消/进度传播；
//! - 错误边界与结果报告。
//!
//! 禁止出现预测、变换、熵编码或设备 API。
//!
//! **迁移状态（P0）**：仅定义请求/报告类型。生产路径仍为
//! `crate::crf::encoder::encode_sequence`。P3 完成后将在此文件实现 facade 转发。

use crate::crf::format::{EncodeParams, ImageData};

/// 编码请求
///
/// 封装输入帧序列与编码参数。应用层构造此请求后交给 [`encode`] 执行。
/// `params` 为 [`EncodeParams`] 的不可变引用语义（此处克隆以隔离生命周期）。
#[derive(Debug, Clone)]
pub struct EncodeRequest {
    /// 输入帧序列（2~65535 帧；首帧为原图或差分基准，视 `params.input_original_frames`）
    pub frames: Vec<ImageData>,
    /// 编码参数
    pub params: EncodeParams,
}

/// 编码报告
///
/// 编码完成后的结构化结果。包含产物字节流与诊断信息。
#[derive(Debug)]
pub struct EncodeReport {
    /// CRF 码流字节
    pub bytes: Vec<u8>,
    /// 实际生效的配置（解析 `Auto`、冲突仲裁后的最终值，P1 接入 `ResolvedConfig`）
    pub resolved: crate::crf::core::contract::ResolvedConfig,
    /// 编码过程中的告警（如 backend 回退、候选裁剪等）
    pub warnings: Vec<String>,
}

/// 编码 facade 入口（P0 占位）
///
/// **当前未实现**。生产路径请使用 `crate::crf::encoder::encode_sequence`。
/// P3 阶段 session 拆分完成后，本函数将转为正式入口，旧函数转发到此处。
#[allow(dead_code)]
pub fn encode(request: EncodeRequest) -> Result<EncodeReport, super::CodecError> {
    // P0：仅声明签名，不接入生产路径。
    // P3 实现：resolve config → create EncodeSession → 编排帧管线 → 组装码流。
    Err(super::CodecError::NotImplemented("codec::encode facade (P3)"))
}

/// 流式编码 facade 入口（P0 占位）
///
/// 写入指定 writer。streaming 路径内存占用 O(golden + 单帧 + 码流)。
#[allow(dead_code)]
pub fn encode_to_writer(
    _request: EncodeRequest,
    _writer: &mut impl std::io::Write,
) -> Result<EncodeReport, super::CodecError> {
    Err(super::CodecError::NotImplemented("codec::encode_to_writer facade (P3)"))
}
