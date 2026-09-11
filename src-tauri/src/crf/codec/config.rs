//! 用户侧配置 API。Tauri command/SDK/CLI 共用，不包含编码算法或文件 I/O。

// codec facade 配置 API：供未来 CLI/前端接线，当前暂无调用方；冻结期允许 dead_code。
#![allow(dead_code)]

/// 校验并解析 V2 JSON，返回包含 effective/warnings/fingerprint 的 canonical JSON。
pub fn resolve_lossy_json(input: &str) -> Result<String, super::CodecError> {
    let request = crate::crf::core::config::lossy_v2::LossyOptionsV2::from_json_str(input)
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))?;
    request
        .resolve_without_encoding()
        .and_then(|x| x.to_json_pretty())
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))
}

/// 返回高级/实验面板字段、范围、单位、选项和影响方向。
pub fn lossy_expert_schema_json() -> Result<String, super::CodecError> {
    serde_json::to_string_pretty(&crate::crf::core::config::lossy_v2::expert_panel_schema())
        .map_err(|e| super::CodecError::InvalidInput(e.to_string()))
}
