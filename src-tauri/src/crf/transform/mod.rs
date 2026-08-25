//! 可逆整数变换核库（lifting-based，严格可逆）
//!
//! 所有变换核共用同一套可逆原语（无损蝶形 + 三点提升旋转），数学上
//! 保证与系数精度无关的逐位可逆性；定点角度参数只影响能量集中质量，
//! 不影响正确性。
//!
//! 模块划分：
//! - [`dct4`]：一维/二维 4 点 lifting DCT-II 近似（v1.3 起的有损路径基核）
//! - [`dct8`]：一维/二维 8 点 lifting DCT-II 近似（v1.10 新增，对称/反对称
//!   分离骨架 + 双 4 点核结构，服务于平坦区域的大块变换收益）

pub mod dct4;
pub mod dct8;
pub mod rect;

pub use dct4::{dct4x4_forward, dct4x4_inverse};
pub use dct8::{dct8x8_forward, dct8x8_inverse};
pub use rect::{dct_rect_forward, dct_rect_inverse, is_valid_rect};
