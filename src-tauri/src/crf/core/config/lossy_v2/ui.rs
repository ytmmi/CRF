use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertField {
    pub path: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    pub choices: &'static [&'static str],
    pub impact: &'static str,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertPanel {
    pub id: &'static str,
    pub label: &'static str,
    pub experimental: bool,
    pub fields: Vec<ExpertField>,
}

/// Tauri/React 专家面板的唯一字段描述源。前端展示继承值/有效值时同时调用
/// `resolve_without_encoding`，不复制后端范围、单位或枚举默认。
pub fn expert_panel_schema() -> Vec<ExpertPanel> {
    vec![
        panel(
            "basic",
            "质量与码率",
            false,
            vec![
                field(
                    "base.qualityX100",
                    "质量",
                    "Q×100",
                    100,
                    10000,
                    &[],
                    "增大通常提高质量和体积",
                ),
                field(
                    "rate.targetBytes",
                    "目标字节",
                    "bytes",
                    1,
                    i64::MAX,
                    &[],
                    "限制完整 CRF 目标大小",
                ),
                field(
                    "performance.effort",
                    "编码努力",
                    "level",
                    0,
                    10,
                    &[],
                    "增大搜索范围和耗时",
                ),
            ],
        ),
        panel(
            "firstFrame",
            "首帧",
            false,
            vec![
                field(
                    "firstFrame.mode",
                    "首帧模式",
                    "",
                    0,
                    0,
                    &["match-sequence", "lossless", "quality-offset", "explicit"],
                    "控制首帧质量与码率分配",
                ),
                field(
                    "firstFrame.qualityOffsetX100",
                    "质量偏移",
                    "Q×100",
                    -2000,
                    2000,
                    &[],
                    "正值提高首帧质量",
                ),
            ],
        ),
        panel(
            "chroma",
            "色度",
            false,
            vec![
                field(
                    "chroma.sampling",
                    "色度采样",
                    "",
                    0,
                    0,
                    &["auto", "444", "422", "420"],
                    "420 显著降低色度码率",
                ),
                field(
                    "quant.chromaScaleX1000",
                    "色度量化倍率",
                    "×1000",
                    500,
                    4000,
                    &[],
                    "增大降低色度码率",
                ),
                field(
                    "quant.deadzoneChromaX256",
                    "色度死区",
                    "/256",
                    -128,
                    256,
                    &[],
                    "增大稀疏化强度",
                ),
            ],
        ),
        panel(
            "perceptual",
            "感知保护",
            false,
            vec![
                field(
                    "perceptual.flatAreaProtectionX100",
                    "平坦区保护",
                    "×100",
                    0,
                    200,
                    &[],
                    "增大降低渐变 banding 风险",
                ),
                field(
                    "perceptual.edgeProtectionX100",
                    "边缘保护",
                    "×100",
                    0,
                    200,
                    &[],
                    "增大保护文字和线稿",
                ),
                field(
                    "perceptual.activityMaskingX100",
                    "活动掩蔽",
                    "×100",
                    0,
                    200,
                    &[],
                    "增大允许纹理区更强量化",
                ),
            ],
        ),
        panel(
            "temporal",
            "时间预测",
            false,
            vec![
                field(
                    "temporal.referenceMode",
                    "参考策略",
                    "",
                    0,
                    0,
                    &["auto", "golden", "previous", "hybrid"],
                    "Hybrid 逐帧竞争重建参考",
                ),
                field(
                    "temporal.motionRange",
                    "位移范围",
                    "pixels",
                    0,
                    32,
                    &[],
                    "增大整数位移搜索范围",
                ),
                field(
                    "temporal.anchorInterval",
                    "锚点间隔",
                    "frames",
                    0,
                    65535,
                    &[],
                    "缩短间隔改善随机访问",
                ),
            ],
        ),
        panel(
            "experimental",
            "实验工具",
            true,
            vec![
                field(
                    "experimental.palette",
                    "局部调色板",
                    "",
                    0,
                    0,
                    &["auto", "off", "on"],
                    "低色数 UI/赛璐璐可能受益",
                ),
                field(
                    "experimental.rdoCandidateLimit",
                    "候选上限",
                    "count",
                    0,
                    65535,
                    &[],
                    "非零值可能改变率失真结果",
                ),
            ],
        ),
    ]
}

fn panel(
    id: &'static str,
    label: &'static str,
    experimental: bool,
    fields: Vec<ExpertField>,
) -> ExpertPanel {
    ExpertPanel {
        id,
        label,
        experimental,
        fields,
    }
}
fn field(
    path: &'static str,
    label: &'static str,
    unit: &'static str,
    minimum: i64,
    maximum: i64,
    choices: &'static [&'static str],
    impact: &'static str,
) -> ExpertField {
    ExpertField {
        path,
        label,
        unit,
        minimum: choices.is_empty().then_some(minimum),
        maximum: choices.is_empty().then_some(maximum),
        choices,
        impact,
    }
}
