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

use crate::crf::core::domain::{EncodeParams, ImageData};

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
    /// V2 有损配置解析报告；无损请求为 None。
    pub lossy: Option<crate::crf::core::config::lossy_v2::ResolvedLossyReport>,
}

/// 编码 facade 入口
///
/// 转发到 `crate::crf::encoder::encode_sequence`（迁移期旧入口保持生产）。
/// P3 之后旧入口将反转为转发到本函数。
pub fn encode(request: EncodeRequest) -> Result<EncodeReport, super::CodecError> {
    let lossy = request
        .params
        .lossy
        .as_ref()
        .map(|v| {
            v.resolve_for_input(crate::crf::core::config::lossy_v2::ResolveContext {
                components: request.frames.first().map(|f| f.color_format.component_count()),
                frame_count: Some(request.frames.len()),
            })
        })
        .transpose()
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))?;
    let warnings = lossy
        .as_ref()
        .map(|r| r.warnings.iter().map(|w| w.message.clone()).collect())
        .unwrap_or_default();
    // 解析并冻结不可变配置（批量和 streaming 共用同一解析逻辑）
    let resolved = crate::crf::core::contract::ResolvedConfig::resolve(&request.params, &request.frames)
        .map_err(super::CodecError::from)?;

    // 通过 EncodeSession 编排（规划文档 §4.2）
    let bytes =
        crate::crf::encoder::session::session::EncodeSession::encode_sequence(&request.frames, &request.params)
            .map_err(super::CodecError::from)?;

    Ok(EncodeReport {
        bytes,
        resolved,
        warnings,
        lossy,
    })
}

/// 流式编码 facade 入口
///
/// 写入指定 writer。streaming 路径内存占用 O(golden + 单帧 + 码流)。
/// 当前转发到批量路径（streaming 语义统一为整改第 6 条，暂未拆分）。
pub fn encode_to_writer(
    request: EncodeRequest,
    writer: &mut impl std::io::Write,
) -> Result<EncodeReport, super::CodecError> {
    let report = encode(request)?;
    writer
        .write_all(&report.bytes)
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))?;
    Ok(report)
}
