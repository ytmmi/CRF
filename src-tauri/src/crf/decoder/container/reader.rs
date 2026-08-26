//! Bounded reader / bit reader —— 安全边界读取
//!
//! 规划文档 §3.3 / §5.2。为解码器提供安全的 bounded slice/reader，
//! 防止越界读取和截断导致的 panic。
//!
//! **迁移状态（P2）**：定义 [`BoundedSlice`] 和 [`BoundedReader`] 类型。
//! 当前解码器仍使用原始 slice 操作，后续 P2.b 阶段逐步接入本模块。

use crate::crf::error::{CrfError, CrfResult};

/// 边界保护的字节切片
///
/// 提供安全的范围检查，避免越界读取。与 `FramePacket.range` 配合使用。
#[derive(Debug, Clone, Copy)]
pub struct BoundedSlice<'a> {
    data: &'a [u8],
    offset: usize,
    len: usize,
}

impl<'a> BoundedSlice<'a> {
    /// 创建新的 bounded slice
    ///
    /// 在构造时即校验边界，尽早失败。
    pub fn new(data: &'a [u8], offset: usize, len: usize) -> CrfResult<Self> {
        let end = offset.checked_add(len).ok_or_else(|| {
            CrfError::InvalidCodingParams("offset + len 溢出".to_string())
        })?;
        if end > data.len() {
            return Err(CrfError::InsufficientData {
                expected: end,
                actual: data.len(),
            });
        }
        Ok(BoundedSlice { data, offset, len })
    }

    /// 获取内部字节切片
    pub fn as_slice(&self) -> &'a [u8] {
        &self.data[self.offset..self.offset + self.len]
    }

    /// 剩余长度
    pub fn remaining(&self) -> usize {
        self.len
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// 支持边界检查的位读取器（P2 骨架，P4 细化）
///
/// 当前 `decoder/rle_cabac.rs` 中的 `RangeDecoder` 为独立实现，
/// 后续 P4 熵编码层整理时统一接入本模块。
#[derive(Debug)]
pub struct BoundedReader<'a> {
    slice: BoundedSlice<'a>,
    pos: usize,
}

impl<'a> BoundedReader<'a> {
    /// 创建 bounded reader
    pub fn new(data: &'a [u8], offset: usize, len: usize) -> CrfResult<Self> {
        let slice = BoundedSlice::new(data, offset, len)?;
        Ok(BoundedReader { slice, pos: 0 })
    }

    /// 读取指定长度的字节
    pub fn read_bytes(&mut self, count: usize) -> CrfResult<&'a [u8]> {
        let end = self.pos.checked_add(count).ok_or_else(|| {
            CrfError::InvalidCodingParams("read_bytes 位置溢出".to_string())
        })?;
        if end > self.slice.len {
            return Err(CrfError::InsufficientData {
                expected: end,
                actual: self.slice.len,
            });
        }
        let result = &self.slice.data[self.slice.offset + self.pos..self.slice.offset + end];
        self.pos = end;
        Ok(result)
    }
}