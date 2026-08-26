//! 公共契约类型
//!
//! 定义 encoder/decoder 之间共享的稳定数据契约。这些类型是 P0 阶段冻结边界的核心
//! 产物——后续 P1~P6 迁移将逐步让现有实现引用这些契约，而非互相直接调用。
//!
//! 规划文档 §7 定义了四个关键契约：
//! - [`FramePacket`]（§7.2）：统一内部帧包；
//! - [`CandidateResult`]（§7.3）：候选结果与代价；
//! - [`ReferenceState`]（§7.4）：参考帧状态；
//! - [`ResolvedConfig`]：解析后的不可变有效配置。
//!
//! **P0 约束**：本文件只定义类型，不实现逻辑。字段类型引用现有 `format` 类型，
//! 避免引入未实现的依赖。所有类型标注 `#[allow(dead_code)]`——P1 起逐步接入。

use crate::crf::core::bitstream::header::CrfHeader;
use crate::crf::core::domain::{
    EncodeParams, FrameHeader, FrameIndexEntry, ImageData,
};

// ============================================================================
// §7.2 FramePacket —— 统一内部帧包
// ============================================================================

/// 字节范围（用于 bounded reader 的安全边界）
///
/// 编码器输出与解码器输入的 payload 均以此结构标记有效字节范围，
/// 避免 unbounded slice 读取。
#[derive(Debug, Clone, Copy)]
pub struct ByteRange {
    /// 起始偏移（含）
    pub start: usize,
    /// 长度
    pub len: usize,
}

impl ByteRange {
    /// 构造从 `start` 开始、长度 `len` 的范围
    pub fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    /// 结束偏移（不含）
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

/// feature set（payload 特性位）
///
/// 记录该帧 payload 启用的特性（如 RCT、lossy、CABAC 模式等），
/// 供 dispatcher 和 reconstruction 层决定处理路径。
#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureSet {
    /// 原始 flags 字节（与文件头 `Flags` 一致）
    pub flags_byte: u8,
    /// 帧级额外特性位（frame_header.coding_params 的高位语义）
    pub frame_feature_bits: u8,
}

/// 统一内部帧包（规划文档 §7.2）
///
/// 编码器输出 `EncodedFrame { header, payload, reconstructed }`，容器层只接收前两项；
/// 解码器输入 `FramePacket`，输出 `DecodedFrame`。`reconstructed` 不得序列化进隐藏字段，
/// 必须明确由 session 保存或释放。
#[derive(Debug)]
pub struct FramePacket<'a> {
    /// 帧头
    pub header: FrameHeader,
    /// payload 字节引用
    pub payload: &'a [u8],
    /// payload 在文件中的字节范围（bounded reader 边界）
    pub range: ByteRange,
    /// 特性位
    pub feature_set: FeatureSet,
}

impl<'a> FramePacket<'a> {
    /// 创建新的帧包（`file_offset` 为帧头在文件中的起始偏移）
    pub fn new(header: FrameHeader, payload: &'a [u8], file_offset: usize) -> Self {
        let total_len =
            crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE + payload.len();
        FramePacket {
            header,
            payload,
            range: ByteRange::new(file_offset, total_len),
            feature_set: FeatureSet::default(),
        }
    }
}

// ============================================================================
// §7.4 ReferenceState —— 参考帧状态
// ============================================================================

/// 重建帧
///
/// 由编码端本地重建或解码端恢复得到，用于后续帧的差分参考。
/// 区分 `golden`（首帧重建）、`previous`（前一帧重建）和 `anchor`（场景锚点）。
#[derive(Debug, Clone)]
pub struct ReconstructedFrame {
    /// 重建像素数据
    pub pixels: Vec<i32>,
    /// 帧宽度
    pub width: u16,
    /// 帧高度
    pub height: u16,
    /// 分量数
    pub components: usize,
}

/// 场景锚点映射
///
/// 非首帧参考的特殊锚点帧（如场景切换的关键帧）。当前实现为空占位，
/// P3 session 拆分时接入实际语义。
pub type AnchorMap = std::collections::HashMap<u16, ReconstructedFrame>;

/// 参考帧状态（规划文档 §7.4）
///
/// 只保存重建帧，禁止存储对后续解码不可得的原始 frame。
/// 首帧有损时必须先完成 `golden`；后续帧可以并行，但不能绕过该阶段依赖。
#[derive(Debug, Default)]
pub struct ReferenceState {
    /// golden 参考（首帧重建）
    pub golden: Option<ReconstructedFrame>,
    /// previous 参考（前一帧重建）
    pub previous: Option<ReconstructedFrame>,
    /// 场景锚点映射
    pub anchors: AnchorMap,
}

impl ReferenceState {
    /// 创建空的参考状态
    pub fn new() -> Self {
        Self::default()
    }
}

// ============================================================================
// ResolvedConfig —— 解析后的不可变有效配置
// ============================================================================

/// 解析后的有效配置
///
/// 由 `EncodeParams` 经继承、`Auto` 解析、冲突仲裁后得到的不可变配置。
/// 后续 session 全部使用同一份实例；批量和 streaming 不得分别解析（规划文档 §3.2）。
///
/// **P0 约束**：当前仅透传 `EncodeParams` 与文件头字段。P3 session 拆分时
/// 补全 `Auto` 解析与冲突仲裁逻辑。
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// 原始编码参数（不可变快照）
    pub params: EncodeParams,
    /// 文件头模板（编码端使用）
    pub header_template: CrfHeader,
    /// 是否启用 RCT（由 `rct_applicable` 解析）
    pub use_rct: bool,
    /// 量化步长（0=无损；>0 为有损死区量化步长）
    pub quant_step: u8,
}

impl ResolvedConfig {
    /// 从 `EncodeParams` 与首帧信息解析不可变有效配置（规划文档 §3.2）
    ///
    /// 只做配置解析与冲突仲裁，不执行像素循环、不分配编码 buffer：
    /// - 压缩类型字符串 → [`CompressionType`]（非法值返回错误）；
    /// - 文件头模板：帧数、尺寸、位深、色彩格式、block_size、预测模式、索引标志；
    /// - `use_rct`：由分量数经 [`rct_applicable`] 解析；
    /// - `quant_step`：由 V2 resolved config 映射（`lossy=None` 时为 0 无损）。
    ///
    /// 批量和 streaming 都必须共用同一份解析结果（不得分别解析）。
    pub fn resolve(
        params: &EncodeParams,
        frames: &[ImageData],
    ) -> crate::crf::error::CrfResult<Self> {
        use crate::crf::core::color::rct::rct_applicable;
        use crate::crf::core::domain::{CompressionType, Flags};
        use crate::crf::error::CrfError;

        let first = &frames[0];
        let frame_count = frames.len() as u16;

        // 压缩类型解析（与 encoder/sequence.rs 保持同一映射）
        let compression_type = match params.compression_type.as_str() {
            "golomb-rice" | "golomb" => CompressionType::GolombRice,
            "exp-golomb" | "exp_golomb" | "egc" => CompressionType::ExpGolomb,
            "transform" | "dct" => CompressionType::Transform,
            _ => {
                return Err(CrfError::InvalidCodingParams(
                    params.compression_type.clone(),
                ))
            }
        };

        // 文件头模板
        let mut header = CrfHeader::new(
            frame_count,
            first.width,
            first.height,
            first.bit_depth,
            first.color_format,
            compression_type,
        );
        header.block_size = params.block_size.unwrap_or(8) as u16;
        header.prediction_mode = params.prediction_mode;
        let mut flags = Flags::new();
        flags.set_has_index(true);
        header.flags = flags;
        if let Some(ref user_data) = params.user_metadata {
            header.user_data = user_data.clone();
        }

        // RCT 适用性（分量数判定）
        let components = first.color_format.component_count();
        let use_rct = rct_applicable(components);

        // 有损量化步长（None=无损）
        let lossy = crate::crf::core::config::lossy_v2::KernelLossyConfig::from_options(
            params.lossy.as_ref(),
            crate::crf::core::config::lossy_v2::ResolveContext {
                components: Some(components),
                frame_count: Some(frames.len()),
            },
        )
        .map_err(|e| CrfError::InvalidCodingParams(e.to_string()))?;
        let quant_step = lossy.enabled.then_some(lossy.global_step).unwrap_or(0);

        Ok(ResolvedConfig {
            params: params.clone(),
            header_template: header,
            use_rct,
            quant_step,
        })
    }
}

// ============================================================================
// §7.3 CandidateResult —— 候选结果与代价
// ============================================================================

/// 候选语法描述
///
/// 标识候选使用的预测模式、变换类型、熵编码方式等组合。
/// P3 frame pipeline 拆分时细化为枚举。
#[derive(Debug, Clone)]
pub struct CandidateSyntax {
    /// 帧类型（与 `FrameHeader.frame_type` 一致）
    pub frame_type: u8,
    /// 预测模式（0xFF=跟随全局）
    pub pred_mode: u8,
    /// 熵编码 k 值或特性位
    pub coding_params: u8,
}

/// 失真度量
///
/// RDO 决策使用的失真值。当前为 SAD 标量；P3 可扩展为 SATD/SSIM 等。
#[derive(Debug, Clone, Copy, Default)]
pub struct Distortion {
    /// 失真绝对值之和（SAD）
    pub sad: u64,
}

/// 候选结果（规划文档 §7.3）
///
/// 候选不得直接写全局文件或修改其他候选状态。RDO 层负责 `D+λR`、
/// 硬质量约束和 tie-break；胜者才交给 payload writer。
#[derive(Debug)]
pub struct CandidateResult {
    /// 候选语法
    pub syntax: CandidateSyntax,
    /// 失真
    pub distortion: Distortion,
    /// 码率（比特数）
    pub rate_bits: u64,
    /// 重建数据（tile 或整帧）
    pub reconstructed: TileOrFrame,
}

/// 重建数据载体
///
/// 候选重建结果可以是单个 tile 或整帧。P3 拆分时接入 tile 语义。
#[derive(Debug, Clone)]
pub enum TileOrFrame {
    /// 整帧重建
    Frame(ReconstructedFrame),
    /// Tile 重建（P3 接入）
    Tile {
        /// tile 索引
        index: usize,
        /// 像素数据
        pixels: Vec<i32>,
    },
}

// ============================================================================
// 编码/解码结果契约（facade 层引用）
// ============================================================================

/// 编码帧产物
///
/// 编码器 FrameEncoder 输出。`reconstructed` 不序列化进码流，
/// 由 session 保存到 `ReferenceState`。
#[derive(Debug)]
pub struct EncodedFrame {
    /// 帧头
    pub header: FrameHeader,
    /// payload 字节
    pub payload: Vec<u8>,
    /// 本地重建帧（用于参考闭环）
    pub reconstructed: ReconstructedFrame,
}

/// 解码帧产物
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// 解码后的图像数据
    pub image: ImageData,
    /// 是否为 golden 参考
    pub is_golden: bool,
}

/// 帧索引（容器层使用）
pub type FrameIndex = Vec<FrameIndexEntry>;
