//! backend —— 性能后端层
//!
//! 规划文档 §2。包含三个实现：
//! - [`scalar`]：唯一正确性基准（标量参考实现）
//! - [`cpu`]：Rayon、AVX2/AVX-512/NEON
//! - [`gpu`]：CUDA、HIP、Vulkan/wgpu（可选 feature）
//!
//! 每个 kernel 必须有 scalar、CPU SIMD 和可选 GPU 实现；后端错误由 scheduler 处理，
//! 不能由算法层捕获后静默改变质量语义。
//!
//! 依赖方向（规划文档 §2）：backend 只实现 kernel trait，不得依赖具体
//! encoder/decoder session。任何底层模块不得依赖 Tauri、UI、测试运行器或文件路径。
//!
//! **P0 状态**：定义 [`BackendKernel`] trait 骨架。scalar/cpu/gpu 子模块为空声明。
//! P5 阶段接入性能后端时实现具体 kernel。

// backend 契约层：BackendKernel trait / KernelInput / KernelOutput / KernelLayout /
// BackendError 为 P5 接入具体 kernel 前的预留 API，暂无调用方；契约冻结期允许 dead_code。
#![allow(dead_code)]

/// scalar 参考实现（唯一正确性基准）
pub mod scalar;

/// CPU SIMD/threads 后端（AVX2/AVX-512/NEON/Rayon）
pub mod cpu;

/// GPU 后端（CUDA/HIP/Vulkan/wgpu，可选 feature）
pub mod gpu;

/// ops：后端统一运算入口（算法层的唯一后端依赖边界）
pub mod ops;

/// BackendKernel 输入
#[derive(Debug, Clone)]
pub struct KernelInput {
    /// 数据布局
    pub layout: KernelLayout,
    /// 维度
    pub dimensions: (usize, usize),
    /// 位深
    pub bit_depth: u8,
    /// 分量索引
    pub plane: usize,
}

/// BackendKernel 输出
#[derive(Debug, Clone)]
pub struct KernelOutput {
    /// 输出值
    pub values: Vec<i32>,
    /// 代价（可选，用于 RDO）
    pub costs: Option<Vec<u64>>,
    /// 重建 tile（可选，编码端使用）
    pub reconstructed_tile: Option<Vec<i32>>,
}

/// 数据布局
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLayout {
    /// 交织布局（RGBRGB...）
    Interleaved,
    /// 平面布局（RRR...GGG...BBB...）
    Planar,
    /// Tile 布局
    Tile,
}

/// BackendKernel trait（规划文档 §7.5）
///
/// 每个 kernel 必须有 scalar、CPU SIMD 和可选 GPU 实现。
/// 后端错误由 scheduler 处理，不能由算法层捕获后静默改变质量语义。
pub trait BackendKernel: Send + Sync {
    /// 执行 kernel
    fn execute(&self, input: &KernelInput) -> Result<KernelOutput, BackendError>;

    /// 后端标识（用于 capability report）
    fn backend_id(&self) -> &'static str;
}

/// 后端错误
#[derive(Debug)]
pub enum BackendError {
    /// 不支持的操作
    Unsupported(&'static str),
    /// 内存分配失败
    AllocFailed,
    /// 设备错误（GPU）
    DeviceError(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unsupported(what) => write!(f, "backend unsupported: {}", what),
            BackendError::AllocFailed => write!(f, "backend alloc failed"),
            BackendError::DeviceError(msg) => write!(f, "backend device error: {}", msg),
        }
    }
}

impl std::error::Error for BackendError {}
