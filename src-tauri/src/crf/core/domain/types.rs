//! domain —— 公共领域类型（原 format/types.rs，P4 架构迁移）
//!
//! 规划文档 §3.1 / §6.3。编解码器共享的数据模型与不变量：
//! 图像、色彩格式、压缩类型、预测模式、文件标志、帧头、索引项、
//! 解码结果与编码参数。
//!
//! **迁移说明（P4）**：本模块原位于 `format/types.rs`，现迁入 `core/domain`。
//! 打破 core → format 反向依赖（format 依赖 core 常量，core 又依赖 format
//! 类型形成的逻辑循环）。旧 `format/types.rs` 不再保留。

use crate::crf::error::{CrfError, CrfResult};

/// 色彩格式枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorFormat {
    /// 灰度
    Gray = 0,
    /// YUV 4:4:4
    Yuv444 = 1,
    /// YUV 4:2:2
    Yuv422 = 2,
    /// YUV 4:2:0
    Yuv420 = 3,
    /// RGB
    Rgb = 4,
}

impl ColorFormat {
    /// 从 u8 值转换
    pub fn from_u8(value: u8) -> CrfResult<Self> {
        match value {
            0 => Ok(ColorFormat::Gray),
            1 => Ok(ColorFormat::Yuv444),
            2 => Ok(ColorFormat::Yuv422),
            3 => Ok(ColorFormat::Yuv420),
            4 => Ok(ColorFormat::Rgb),
            _ => Err(CrfError::UnsupportedColorFormat(value)),
        }
    }

    /// 获取每个像素的分量数
    pub fn component_count(&self) -> usize {
        match self {
            ColorFormat::Gray => 1,
            ColorFormat::Yuv444 | ColorFormat::Yuv422 | ColorFormat::Yuv420 | ColorFormat::Rgb => 3,
        }
    }
}

/// 压缩类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionType {
    /// Golomb-Rice 编码
    GolombRice = 0,
    /// 指数哥伦布编码
    ExpGolomb = 1,
    /// 变换 + 熵编码
    Transform = 2,
}

impl CompressionType {
    /// 从 u8 值转换
    pub fn from_u8(value: u8) -> CrfResult<Self> {
        match value {
            0 => Ok(CompressionType::GolombRice),
            1 => Ok(CompressionType::ExpGolomb),
            2 => Ok(CompressionType::Transform),
            _ => Err(CrfError::UnsupportedCompressionType(value)),
        }
    }
}

/// 帧内预测模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PredictionMode {
    /// 无预测（原始数据）
    None = 0,
    /// 水平预测：predicted = left neighbor
    Horizontal = 1,
    /// 垂直预测：predicted = top neighbor
    Vertical = 2,
    /// 平均预测：predicted = (left + top) / 2
    Average = 3,
    /// DC预测：predicted = (left + top + top_left + top_right) / 4
    DC = 4,
    /// MED预测（JPEG-LS LOCO-I）：基于 left/top/top_left 的边缘检测中值预测
    Med = 5,
    /// PAETH预测（AV1 predictor / PNG filter type 4）：三邻居梯度比较选最近者
    Paeth = 6,
    /// 右上预测（AV1 D45 因果简化版，v1.9 新增）：
    /// predicted = 上行右移一位（右上邻居）；边界回退 top / left / 0。
    /// 对"/"走向斜线（头发丝、裙褶）残差恒为零。
    TopRight = 7,
    /// 对角预测（AV1 D135 因果简化版，v1.9 新增）：
    /// 沿主对角线上溯至边界的像素；首行/首列回退 left/top。
    /// 对"\"走向斜线残差恒为零。
    Diagonal = 8,
    /// 多参考行预测（v1.13，第四批 #2）：pred = 上方第 2 行同列像素。
    /// 与 top 组成 2 行参考窗——2 像素周期网点/横细条纹上残差恒为零。
    /// y==1 回退 top；y==0 回退 left；原点回退 0。
    Vertical2 = 9,
    /// 多参考列预测（v1.13，第四批 #2）：pred = 左侧第 2 列同行像素。
    /// 2 像素周期网点/竖细条纹特化。x==1 回退 left；x==0 回退 top；
    /// 原点回退 0。
    Horizontal2 = 10,
}

impl PredictionMode {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => PredictionMode::None,
            1 => PredictionMode::Horizontal,
            2 => PredictionMode::Vertical,
            3 => PredictionMode::Average,
            4 => PredictionMode::DC,
            5 => PredictionMode::Med,
            6 => PredictionMode::Paeth,
            7 => PredictionMode::TopRight,
            8 => PredictionMode::Diagonal,
            9 => PredictionMode::Vertical2,
            10 => PredictionMode::Horizontal2,
            _ => PredictionMode::None,
        }
    }

    /// 帧头 pred_mode 字段的特殊值：表示该帧未指定预测模式（跟随文件头全局设置）
    pub const PRED_MODE_UNSET: u8 = 0xFF;
}

/// 文件头标志位
#[derive(Debug, Clone, Copy)]
pub struct Flags(u8);

impl Flags {
    /// 创建新的标志位
    pub fn new() -> Self {
        Flags(0)
    }

    /// 是否包含帧索引
    pub fn has_index(&self) -> bool {
        self.0 & 0x01 != 0
    }

    /// 设置是否包含帧索引
    pub fn set_has_index(&mut self, has: bool) {
        if has {
            self.0 |= 0x01;
        } else {
            self.0 &= !0x01;
        }
    }

    /// 帧数据是否经过 YCoCg-R 可逆色彩变换（参考 AV1/HEVC RExt 无损做法）
    pub fn has_rct(&self) -> bool {
        self.0 & 0x02 != 0
    }

    /// 设置是否经过可逆色彩变换
    pub fn set_has_rct(&mut self, has: bool) {
        if has {
            self.0 |= 0x02;
        } else {
            self.0 &= !0x02;
        }
    }

    /// 是否为真有损编码（残差经死区标量量化，量化步长见文件头 lossy_quant）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn has_lossy_quant(&self) -> bool {
        self.0 & 0x04 != 0
    }

    /// 设置是否为有损编码
    pub fn set_has_lossy_quant(&mut self, has: bool) {
        if has {
            self.0 |= 0x04;
        } else {
            self.0 &= !0x04;
        }
    }

    /// 首帧是否绕过 YCoCg-R 存储（v1.13 RCT 首帧自适应）
    ///
    /// 仅当 has_rct=1 时有意义：true 表示 frame0 以 RGB 直通形式存储——
    /// 编码端对首帧做「RCT 域 vs RGB 原域」双路完整管线竞争（字节最小者
    /// 胜出），高饱和纯色场景 RGB 域更稀疏时直通胜出。解码端出口须按本
    /// 标志跳过 frame0 的 rct_inverse。差分帧不受影响（其差分基准是首帧
    /// 像素值而非存储格式）。bit3 于 v1.13 前恒为 0，旧文件天然兼容。
    pub fn first_frame_no_rct(&self) -> bool {
        self.0 & 0x08 != 0
    }

    /// 设置首帧绕过 YCoCg-R 标志
    pub fn set_first_frame_no_rct(&mut self, has: bool) {
        if has {
            self.0 |= 0x08;
        } else {
            self.0 &= !0x08;
        }
    }

    /// 从 u8 值创建
    pub fn from_u8(value: u8) -> Self {
        Flags(value)
    }

    /// 转换为 u8
    pub fn as_u8(&self) -> u8 {
        self.0
    }
}

/// 帧信息（用于帧索引）
#[derive(Debug, Clone)]
pub struct FrameIndexEntry {
    /// 帧数据在文件中的起始偏移量
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub offset: u32,
    /// 帧压缩数据大小
    pub size: u32,
}

impl FrameIndexEntry {
    /// 从字节缓冲区解析
    pub fn from_bytes(data: &[u8]) -> CrfResult<Self> {
        if data.len() < 8 {
            return Err(CrfError::InsufficientData {
                expected: 8,
                actual: data.len(),
            });
        }

        Ok(FrameIndexEntry {
            offset: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            size: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    /// 写入字节缓冲区
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn write_bytes(&self, writer: &mut impl std::io::Write) -> CrfResult<()> {
        writer.write_all(&self.offset.to_le_bytes())?;
        writer.write_all(&self.size.to_le_bytes())?;
        Ok(())
    }
}

/// 帧头结构
#[derive(Debug, Clone)]
pub struct FrameHeader {
    /// 本帧压缩数据的总大小（不含本字段）
    pub frame_size: u32,
    /// 本帧数据总长度（像素数 × 分量数）
    pub pixel_count: u32,
    /// 帧类型：0=标准Golomb, 1=RLE+Golomb, 0xFF=块级自适应k
    pub frame_type: u8,
    /// 编码参数（如 Golomb-Rice 的 k 值）
    pub coding_params: u8,
    /// 本帧预测模式：0xFF=跟随文件头全局模式，其他值见 PredictionMode
    pub pred_mode: u8,
    /// v1.15：参考类型（0=golden / 1=previous / 2=prev2 / 3=保留）。
    /// 替代旧 coding_params.bit7 的单 bit golden 语义，支持多参考帧
    /// （golden 首帧差分、前帧还原 prev、前前帧还原 prev2）。
    pub reference_type: u8,
}

impl FrameHeader {
    /// 创建新的帧头（pred_mode 默认为未指定，跟随全局模式）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn new(frame_size: u32, pixel_count: u32, coding_params: u8) -> Self {
        FrameHeader {
            frame_size,
            pixel_count,
            frame_type: 0,
            coding_params,
            pred_mode: PredictionMode::PRED_MODE_UNSET,
            reference_type: 0, // 默认 golden
        }
    }

    /// 创建指定帧类型的帧头
    pub fn with_type(frame_size: u32, pixel_count: u32, coding_params: u8, frame_type: u8) -> Self {
        FrameHeader {
            frame_size,
            pixel_count,
            frame_type,
            coding_params,
            pred_mode: PredictionMode::PRED_MODE_UNSET,
            reference_type: 0, // 默认 golden
        }
    }

    /// 从字节缓冲区解析
    pub fn from_bytes(data: &[u8]) -> CrfResult<Self> {
        if data.len() < crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE {
            return Err(CrfError::InsufficientData {
                expected: crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE,
                actual: data.len(),
            });
        }

        Ok(FrameHeader {
            frame_size: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            pixel_count: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            frame_type: data[8],
            coding_params: data[9],
            pred_mode: data[10],
            reference_type: data[11],
        })
    }

    /// 写入字节缓冲区
    pub fn write_bytes(&self, writer: &mut impl std::io::Write) -> CrfResult<()> {
        writer.write_all(&self.frame_size.to_le_bytes())?;
        writer.write_all(&self.pixel_count.to_le_bytes())?;
        writer.write_all(&[self.frame_type])?;
        writer.write_all(&[self.coding_params])?;
        writer.write_all(&[self.pred_mode])?;
        writer.write_all(&[self.reference_type])?;
        Ok(())
    }

    /// 帧是否以首帧（golden）为差分参考基准（reference_type == 0）
    ///
    /// v1.15：golden 语义从 coding_params.bit7 迁移到独立 reference_type 字段，
    /// 帧类型 0~8 均可承载参考语义（不再有 frame_type=0 例外）。
    pub fn is_golden_ref(&self) -> bool {
        self.reference_type == 0
    }

    /// 帧是否以前前帧（prev2）为差分参考基准（reference_type == 2）
    pub fn is_prev2_ref(&self) -> bool {
        self.reference_type == 2
    }

    /// 读取 Golomb k 值（coding_params.bit7 已不再承载 golden 标志，mask 保留防御）
    pub fn golomb_k(&self) -> u8 {
        self.coding_params & 0x7F
    }
    /// 设置参考类型（0=golden / 1=previous / 2=prev2）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn set_reference_type(&mut self, reference_type: u8) {
        self.reference_type = reference_type;
    }
}

/// 图像数据
#[derive(Debug, Clone)]
pub struct ImageData {
    /// 图像宽度
    pub width: u16,
    /// 图像高度
    pub height: u16,
    /// 像素位深
    pub bit_depth: u8,
    /// 色彩格式
    pub color_format: ColorFormat,
    /// 像素数据（行优先存储）
    pub pixels: Vec<i32>,
}

impl ImageData {
    /// 计算像素总数（每分量）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// 计算数据总长度（所有分量）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub fn data_len(&self) -> usize {
        self.pixel_count() * self.color_format.component_count()
    }
}

/// 解码结果
#[derive(Debug)]
pub struct DecodeResult {
    /// 文件头
    pub header: crate::crf::core::bitstream::header::CrfHeader,
    /// 帧索引（如果有）
    #[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
    pub frame_index: Vec<FrameIndexEntry>,
    /// 解码后的帧数据
    ///
    /// 语义由 `frame_golden_refs` / `frame_prev2_refs` 决定：
    /// - golden=true（首帧参考）：该帧数据加首帧像素即为还原帧；
    /// - prev2=true（前前帧参考）：该帧数据加前前帧还原像素；
    /// - 均 false（链式）：该帧为相对前一帧的残差，需累加还原。
    pub frames: Vec<ImageData>,
    /// 每帧的差分参考模式（与 frames 一一对应）：
    /// true = 首帧参考（golden）；false = 见 frame_prev2_refs
    pub frame_golden_refs: Vec<bool>,
    /// v1.15：每帧是否以前前帧（prev2）为参考（与 frames 一一对应）。
    /// 仅当 frame_golden_refs[i]==false 时有意义：true=prev2，false=previous。
    pub frame_prev2_refs: Vec<bool>,
}

/// 编码参数
#[derive(Debug, Clone)]
pub struct EncodeParams {
    /// 压缩类型: "golomb-rice", "exp-golomb", "transform"
    pub compression_type: String,
    /// 变换块大小（仅 transform 模式有效）
    pub block_size: Option<usize>,
    /// 帧内预测模式（adaptive_prediction=true 时作为回退/基准设置）
    pub prediction_mode: PredictionMode,
    /// 是否启用逐帧自适应预测模式选择
    ///
    /// 启用后编码器对每帧独立评估各候选预测模式的残差代价，
    /// 选出最优模式并在帧头记录；解码端自动跟随帧头指示。
    pub adaptive_prediction: bool,
    /// 唯一有损配置入口。None = 无损管线。
    pub lossy: Option<crate::crf::core::config::lossy_v2::LossyOptionsV2>,
    /// @deprecated 仅为源码迁移期保留；新调用必须使用 `lossy`。
    /// @deprecated 仅为源码迁移期保留；新调用必须使用 `lossy`。
    /// 用户自定义元数据
    pub user_metadata: Option<String>,
    /// 输入序列语义（默认 false = 兼容旧约定：frames[0] 为首帧原图、frames[1..] 为预差分残差帧）
    ///
    /// true = frames 即原始帧序列：编码器内部执行**闭环时间预测**——
    /// 每帧残差以「前一帧的解码重建帧」为参考计算，量化误差不再沿链累积
    /// （对标 H.264/HEVC/AV1 的 P 帧参考重建帧机制）。推荐有损模式启用。
    pub input_original_frames: bool,
}
