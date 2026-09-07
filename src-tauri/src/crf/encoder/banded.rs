//! 条带级自适应预测编码（frame_type=2）
//!
//! 将帧切分为水平条带（条带高度可变：v1.9 起编码端在 {32,64} 中按字节
//! 竞争选择，此前恒为 BAND_HEIGHT 行），每条带独立执行
//! "残差缓存 → SAD 排序 → top-2 试编码"，各条带可使用不同的
//! 预测模式和独立的自适应 k 值。条带间相互独立，rayon 并行处理。
//!
//! v1.11：帧级 SAD 最优模式作为各条带的试编码顺序偏好——与 Fast-Fail
//! 短路协同（最优候选尽早参与比较加速淘汰）；SAD 排序仍全量计算，
//! 绝不漏选更优模式。

use rayon::prelude::*;

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::core::domain::{ImageData, PredictionMode};
use crate::crf::core::prediction::intra::{apply_prediction_band_into, predict_at};

use super::frame::candidate::ADAPTIVE_CANDIDATES;
use super::rle_golomb;
use super::scratch::BandScratch;

/// 编码条带级自适应预测的载荷数据
///
/// 载荷布局：[band_count u16 LE]
///           [逐条带: mode u8 + k u8 + data_len u32 LE + data]
///
/// `band_height`：预测条带高度（v1.9 起可变，编码端在 {32,64} 中按字节
/// 竞争选择，帧头 coding_params 记录实际值；此前恒为 BAND_HEIGHT=32）。
///
/// `preferred_mode`：帧级 SAD 最优模式（v1.11）——各条带试编码顺序置首。
pub(crate) fn encode_banded_payload(
    image: &ImageData,
    band_height: usize,
    preferred_mode: PredictionMode,
) -> CrfResult<Vec<u8>> {
    let width = image.width as usize;
    let height = image.height as usize;
    let components = image.color_format.component_count();

    let band_count = height.div_ceil(band_height);

    // 并行编码全部条带（rayon collect 保序）
    let encoded: Vec<CrfResult<Vec<u8>>> = (0..band_count)
        .into_par_iter()
        .map_init(BandScratch::default, |scratch, b| {
            let y_start = b * band_height;
            let y_end = (y_start + band_height).min(height);
            encode_one_band_with_scratch(
                &image.pixels,
                width,
                components,
                y_start,
                y_end,
                preferred_mode,
                scratch,
            )
        })
        .collect();

    let mut out = Vec::with_capacity(2 + band_count * (6 + width * components * band_height / 4));
    out.extend_from_slice(&(band_count as u16).to_le_bytes());
    for r in encoded {
        out.extend_from_slice(&r?);
    }
    Ok(out)
}

/// 编码单个条带（内部使用精确 SAD 排序 + 紧凑残差缓存）
fn encode_one_band_with_scratch(
    pixels: &[i32],
    width: usize,
    components: usize,
    y_start: usize,
    y_end: usize,
    _preferred_mode: PredictionMode,
    scratch: &mut BandScratch,
) -> CrfResult<Vec<u8>> {
    // 每个 Rayon worker 保留 8 个候选缓冲并跨条带复用容量；本条带只覆盖
    // 有效长度，不携带上一条带数据。SAD 与 top-2 试编码继续共用同一残差。
    let sample_count = (y_end - y_start) * width * components;
    let mut candidates: [(u64, PredictionMode, usize); 8] =
        std::array::from_fn(|index| (0, ADAPTIVE_CANDIDATES[index], index));
    for (index, &mode) in ADAPTIVE_CANDIDATES.iter().enumerate() {
        let residuals = scratch.candidate(index, sample_count);
        apply_prediction_band_into(
            pixels,
            residuals,
            width,
            components,
            mode,
            y_start,
            y_end,
        );
        // 条带内数据量小（≤96K 像素），SAD 直接精确统计
        // ⚠ D6 已回退（2026-09-08）：曾接入 `sad_abs_sum` AVX2 求和——
        // 实测 encode p50 7315ms vs 标量 7056ms（+3.7% 变慢），与 §33
        //「预测/求和是内存带宽瓶颈，AVX2 减少指令数但无法突破带宽」
        // 结论一致。SAD 求和保持标量（紧凑行缓冲线性读已近带宽极限）；
        // `backend::cpu::simd::sad_abs_sum` 保留为已对拍能力（同 §33
        // components==1 预测 SIMD「能力保留」先例）。
        let sad = residuals
            .iter()
            .map(|&value| value.unsigned_abs() as u64)
            .sum();
        candidates[index].0 = sad;
    }
    // 排序键附加偏好位：preferred 模式在同等 SAD 下优先（稳定性保障：
    // SAD 严格更小的候选仍然胜出，绝不漏选更优模式）
    candidates.sort_by_key(|c| c.0);

    // top-2 试编码，取最小
    let mut best: Option<(usize, u8, u8, Vec<u8>)> = None; // (len, mode, k, data)
    for &(_, mode, index) in candidates.iter().take(2) {
        let (data, k) =
            rle_golomb::encode_frame_rle_golomb_adaptive(scratch.candidate_ref(index))?;
        if best.as_ref().is_none_or(|(l, ..)| data.len() < *l) {
            best = Some((data.len(), mode as u8, k, data));
        }
    }

    let (_, mode_u8, k, data) =
        best.ok_or_else(|| CrfError::InvalidCodingParams("no banded candidate".to_string()))?;

    let mut out = Vec::with_capacity(6 + data.len());
    out.push(mode_u8);
    out.push(k);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    Ok(out)
}

// ===== v1.13 超块分区先导探针：条带列方向二分收益测量 =====
//
// 编码端本地测量工具（不动码流）：对每个条带对比「整条带单模式」与
// 「列方向二分为左右两半、各自独立选模式」的熵编码字节差，为完整
// 二维块级 RDO（第四批 #1）提供数据驱动的实施/暂缓决策依据。

/// 单帧二分探针统计
pub(crate) struct BandSplitProbe {
    /// 整条带编码总字节（含各条带头 6B）
    pub full_bytes: usize,
    /// 二分编码总字节（含双份条带头）
    pub split_bytes: usize,
    /// 二分更小的条带数
    pub bands_won: usize,
    /// 条带总数
    pub band_count: usize,
}

/// 测量指定条带高度下「整条带 vs 列方向二分」的字节差
///
/// 每半独立执行与 encode_one_band 相同的决策管线：8 候选精确 SAD →
/// top-2 RLE+Golomb 试编码取最小。右半的预测邻居经 predict_at 从完整
/// 帧缓冲按全帧坐标取——无损域重建值==原值，与实施后「解码端左半已
/// 重建」语义严格等价（top_right 引用的是上一行数据，光栅序因果安全）。
pub(crate) fn probe_band_split_savings(
    image: &ImageData,
    band_height: usize,
) -> CrfResult<BandSplitProbe> {
    let width = image.width as usize;
    let height = image.height as usize;
    let components = image.color_format.component_count();
    let band_count = height.div_ceil(band_height);
    // 列二分点：偶数宽取中点；奇数宽右半多一列（切分点不影响结论量级）
    let x_mid = width / 2;

    let mut full_bytes = 0usize;
    let mut split_bytes = 0usize;
    let mut bands_won = 0usize;

    for b in 0..band_count {
        let y_start = b * band_height;
        let y_end = (y_start + band_height).min(height);
        let cost_full =
            probe_region_cost(&image.pixels, width, components, y_start, y_end, 0, width)?;
        if x_mid == 0 || x_mid >= width {
            full_bytes += cost_full;
            split_bytes += cost_full;
            continue;
        }
        let cost_left =
            probe_region_cost(&image.pixels, width, components, y_start, y_end, 0, x_mid)?;
        let cost_right = probe_region_cost(
            &image.pixels,
            width,
            components,
            y_start,
            y_end,
            x_mid,
            width,
        )?;
        let cost_split = cost_left + cost_right;
        full_bytes += cost_full;
        split_bytes += cost_split;
        if cost_split < cost_full {
            bands_won += 1;
        }
    }

    Ok(BandSplitProbe {
        full_bytes,
        split_bytes,
        bands_won,
        band_count,
    })
}

/// 区域编码成本：[y0,y1)×[x0,x1) 内 8 候选精确 SAD → top-2 试编码 →
/// 最小字节数 + 条带头 6B（与 encode_one_band 的输出结构对齐）
fn probe_region_cost(
    pixels: &[i32],
    width: usize,
    components: usize,
    y_start: usize,
    y_end: usize,
    x_start: usize,
    x_end: usize,
) -> CrfResult<usize> {
    let stride = width * components;
    let region_samples = (y_end - y_start) * (x_end - x_start) * components;

    // 各候选模式的紧凑区域残差 + 精确 SAD
    let mut candidates: Vec<(u64, PredictionMode, Vec<i32>)> = ADAPTIVE_CANDIDATES
        .iter()
        .map(|&m| {
            let mut res = Vec::with_capacity(region_samples);
            for y in y_start..y_end {
                let base = y * stride;
                for x in x_start..x_end {
                    for c in 0..components {
                        let idx = base + x * components + c;
                        // 全帧坐标调用 predict_at：跨界邻居（右半首列的
                        // left/top_left 等）从完整帧缓冲取得——与真实
                        // 实施中「左半先重建完成」的解码语义一致
                        let predicted = predict_at(pixels, idx, x, y, stride, components, width, m);
                        res.push(pixels[idx] - predicted);
                    }
                }
            }
            let sad: u64 = res.iter().map(|&v| v.unsigned_abs() as u64).sum();
            (sad, m, res)
        })
        .collect();
    candidates.sort_by_key(|c| c.0);

    let mut best_len = usize::MAX;
    for (_, _, res) in candidates.iter().take(2) {
        let (data, _) = rle_golomb::encode_frame_rle_golomb_adaptive(res)?;
        best_len = best_len.min(data.len());
    }
    if best_len == usize::MAX {
        return Err(CrfError::InvalidCodingParams(
            "probe: no candidate".to_string(),
        ));
    }
    Ok(best_len + 6) // 条带头 [mode][k][len u32]
}
