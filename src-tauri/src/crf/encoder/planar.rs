//! 三平面打包编码（frame_type=3）与 CfL 色度预测
//!
//! 将交错布局 [Y,Co,Cg,...] 拆为三个独立单分量平面，
//! 每平面独立走完整自适应管线（帧级模式选择 + 条带化竞争）。
//! 二次元插画差分场景特化：RCT 去相关后 Co/Cg 色度平面在赛璐璐
//! 上色下大面积恒定，独立编码使 RLE 零行程成倍增长。

use crate::crf::error::CrfResult;
use crate::crf::core::bitstream::constants::FRAME_HEADER_SIZE;
use crate::crf::core::domain::{ColorFormat, CompressionType, ImageData};

use super::frame::candidate::encode_frame_adaptive;
use super::frame::BandSteps;
use super::FrameQuant;

use crate::crf::performance::telemetry::Span;

/// 编码三平面打包载荷（frame_type=3）
///
/// 载荷布局：[cfl_flags u8][len1 u32 LE][sub_frame1][len2][sub_frame2][len3][sub_frame3]
/// 每个 sub_frame 为含自身帧头的完整帧缓冲。
///
/// CfL（Chroma-from-Luma，HEIF/AV1 同款思路）：
/// Co/Cg 平面先减去亮度线性预测 `⌊α·(Y−128) / 16⌋` 再编码，
/// α 从候选集抽样搜索；解码端用已重建的 Y 平面对称还原（无损）。
///
/// 递归安全性：子平面 components=1，不会再次进入 planar 分支。
///
/// band_steps 仅透传给 Y 平面子帧（行划分与原帧对齐）；
/// Co/Cg 子平面可能为半分辨率（条带错位）且已有 chroma_step 粗化，
/// 一律不启用逐条带自适应步长。
pub(crate) fn encode_planar_payload(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    fq: FrameQuant,
    band_steps: BandSteps<'_>,
) -> CrfResult<Vec<u8>> {
    encode_planar_payload_limited(image, compression_type, block_size, fq, band_steps, usize::MAX)
        .map(|opt| opt.expect("unlimited planar encoding always yields a payload"))
}

/// 带字节预算的 planar 载荷编码（Fast-Fail）。
///
/// 与 [`encode_planar_payload`] 逐字节等价，但当「已累计载荷体积 +
/// 剩余子平面的最小可能体积（长度前缀 4 + 帧头 11 + 载荷 1 字节）」
/// 超过 `byte_limit` 时提前返回 `Ok(None)`——planar 必败，跳过剩余
/// 子平面编码。判定只使用数学下界，绝不改变胜出候选与最终字节。
pub(crate) fn encode_planar_payload_limited(
    image: &ImageData,
    compression_type: CompressionType,
    block_size: u16,
    fq: FrameQuant,
    band_steps: BandSteps<'_>,
    byte_limit: usize,
) -> CrfResult<Option<Vec<u8>>> {
    // CfL α 候选集（P1 精细化：补齐 ±3 填充稀疏区间）
    // 存储仍为 4 bit（α+8 ∈ [4,12]），载荷格式不变、解码端对称无须修改。
    // CfL 为无损整数预测扣除——更准的 α 只影响残差分布（更小残差→更好
    // 压缩），对无损与有损管线均为潜在正收益，不改变任何码流语义。
    const CFL_CANDIDATES: [i32; 9] = [-4, -3, -2, -1, 0, 1, 2, 3, 4];

    let pixel_count = image.width as usize * image.height as usize;
    let mut planes: [Vec<i32>; 3] = std::array::from_fn(|_| Vec::with_capacity(pixel_count));
    for px in image.pixels.chunks_exact(3) {
        planes[0].push(px[0]);
        planes[1].push(px[1]);
        planes[2].push(px[2]);
    }

    // CfL α 搜索（抽样 1/16 加速）
    // α=0（关闭 CfL）必须参与竞争：当色度与亮度不相关或色度本为常数时，
    // 强制的非零 α 会向平坦平面注入亮度纹理，显著劣化压缩率。
    fn search_alpha(y: &[i32], chroma: &[i32]) -> i32 {
        let step = (chroma.len() / 16).max(1);
        let mut best_a = 0i32;
        let mut best_sad = u64::MAX;
        for &a in &CFL_CANDIDATES {
            let mut sad = 0u64;
            let mut i = 0;
            while i < chroma.len() {
                let pred = (a * (y[i] - 128)) >> 4;
                sad += (chroma[i] - pred).unsigned_abs() as u64;
                i += step;
            }
            if sad < best_sad {
                best_sad = sad;
                best_a = a;
            }
        }
        best_a
    }

    let cfl_span = Span::begin("encode.adaptive.planar.cfl");
    let alpha_c = search_alpha(&planes[0], &planes[1]);
    let alpha_g = search_alpha(&planes[0], &planes[2]);
    drop(cfl_span);
    // 应用 CfL 预测扣除
    let apply_cfl = |chroma: &[i32], alpha: i32| -> Vec<i32> {
        if alpha == 0 {
            return chroma.to_vec();
        }
        let mut out = vec![0i32; chroma.len()];
        crate::crf::backend::ops::cfl_luma_subtract(chroma, &planes[0], alpha, &mut out);
        out
    };
    let co_adj = apply_cfl(&planes[1], alpha_c);
    let cg_adj = apply_cfl(&planes[2], alpha_g);

    // 载荷头：
    //   [0] 高 4 位 αc+8，低 4 位 αg+8（α∈[-4,4] → 存储值 ∈[4,12]，无符号安全）
    //   [1] bit0 = 色度半分辨率标志（Co/Cg 为 (h/2)×(w/2) 下采样平面）
    let half_res = fq.is_lossy() && fq.chroma_half_res;
    let mut out = vec![
        (((alpha_c + 8) as u8) << 4) | ((alpha_g + 8) as u8 & 0x0F),
        if half_res { 0x01 } else { 0x00 },
    ];

    // 色度半分辨率：Co/Cg 平面 2×2 下采样（对标 AVIF/HEVC yuv420p）
    // P4 边缘保护（§5-P4）：2×2 块内方差大于阈值时用中值替代均值——
    // 色度边界处均值会产生渗色（跨区域混合），中值选择主侧值保持锐度。
    // 无码流变化（编码端预处理，解码端只看到下采样后数据）。
    let (co_enc, cg_enc, cw, ch) = if half_res {
        let ds_span = Span::begin("encode.adaptive.planar.downsample");
        let full_w = image.width as usize;
        let full_h = image.height as usize;
        let cw = full_w.div_ceil(2);
        let ch = full_h.div_ceil(2);
        const DS_EDGE_THRESHOLD: i32 = 8; // 2×2 块内 range > 此值判定为边界
        let ds = |p: &[i32]| -> Vec<i32> {
            let mut small = vec![0i32; cw * ch];
            for sy in 0..ch {
                for sx in 0..cw {
                    let x0 = sx * 2;
                    let y0 = sy * 2;
                    let mut vals: [i32; 4] = [0; 4];
                    let mut cnt = 0usize;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let px = x0 + dx;
                            let py = y0 + dy;
                            if px < full_w && py < full_h {
                                vals[cnt] = p[py * full_w + px];
                                cnt += 1;
                            }
                        }
                    }
                    if cnt == 0 {
                        small[sy * cw + sx] = 0;
                    } else if cnt == 1 {
                        small[sy * cw + sx] = vals[0];
                    } else {
                        let lo = vals[..cnt].iter().min().copied().unwrap();
                        let hi = vals[..cnt].iter().max().copied().unwrap();
                        if hi - lo > DS_EDGE_THRESHOLD {
                            // 边界：用中值（排序后取中），避免均值渗色
                            let mut sorted: [i32; 4] = vals;
                            sorted[..cnt].sort_unstable();
                            let mid = cnt / 2;
                            small[sy * cw + sx] = sorted[mid];
                        } else {
                            // 平坦：均值
                            let sum: i64 = vals[..cnt].iter().map(|&v| v as i64).sum();
                            small[sy * cw + sx] = (sum / cnt as i64) as i32;
                        }
                    }
                }
            }
            small
        };
        let result = (ds(&co_adj), ds(&cg_adj), cw, ch);
        drop(ds_span);
        result
    } else {
        (co_adj, cg_adj, image.width as usize, image.height as usize)
    };

    // 色度平面量化步长：chroma_step>0 时用之（人眼对色度失真不敏感）。
    // 死区偏置用 chroma_bias（P1 独立通道；构造点已解析继承语义）
    let fq_c = if fq.chroma_step > 0 {
        FrameQuant {
            step: fq.chroma_step,
            bias: fq.chroma_bias,
            chroma_step: fq.chroma_step,
            chroma_bias: fq.chroma_bias,
            chroma_half_res: half_res,
            q1_matrix_scale: fq.q1_matrix_scale,
        }
    } else {
        FrameQuant {
            step: fq.step,
            bias: fq.bias,
            chroma_step: 0,
            chroma_bias: fq.chroma_bias,
            chroma_half_res: half_res,
            q1_matrix_scale: fq.q1_matrix_scale,
        }
    };

    let mut preferred_sub: Option<crate::crf::core::domain::PredictionMode> = None;
    // (平面数据, 平面宽, 平面高)：Y 全分辨率；Co/Cg 视 half_res 而定。
    // P1：子平面数据转为独占所有权（move 而非 clone）——planes[0]/co_enc/cg_enc
    // 此时均已构建完成且循环后不再使用，move 进 ImageData 省去 3 次全帧克隆。
    // 注意 planes[1]/planes[2]（CfL 前的原始色度）已被 co_enc/cg_enc 取代，随
    // 数组解构丢弃。
    let [y_plane, _, _] = planes;
    let owned_planes: [(Vec<i32>, usize, usize); 3] = [
        (y_plane, image.width as usize, image.height as usize),
        (co_enc, cw, ch),
        (cg_enc, cw, ch),
    ];
    // 色度条带步长（P1 精细化，评审 §11.1 第二项）：
    // band_steps 按 BAND_HEIGHT(32) 行分段对 Y 估计；色度半分辨率 ch=h/2
    // 用同 BAND_HEIGHT 分段需 ceil(ch/32)=ceil(h/64) 步——恰为 Y 表
    // ceil(h/32) 的一半，step_by(2) 完美对齐。YCoCg-R 的 Co/Cg 噪声
    // 与 Y 空间同源（JPEG 源），从 Y 表下采样映射是合理近似。
    // 全分辨率色度（half_res=false）时与 Y 同高，直接继承。
    let chroma_band: Vec<u8> = match band_steps {
        Some(y_steps) if half_res => y_steps.iter().step_by(2).copied().collect(),
        Some(y_steps) if !half_res => y_steps.to_vec(),
        _ => Vec::new(),
    };
    for (pi, (plane, pw, ph)) in owned_planes.into_iter().enumerate() {
        let sub_span = Span::begin(match pi {
            0 => "encode.adaptive.planar.subplane_y",
            1 => "encode.adaptive.planar.subplane_co",
            _ => "encode.adaptive.planar.subplane_cg",
        });
        let pfq = if pi == 0 { fq } else { fq_c };
        let sub_steps: BandSteps<'_> = if pi == 0 {
            band_steps
        } else if !chroma_band.is_empty() {
            Some(&chroma_band)
        } else {
            None
        };
        let plane_image = ImageData {
            width: pw as u16,
            height: ph as u16,
            bit_depth: image.bit_depth,
            color_format: ColorFormat::Gray,
            pixels: plane,
        };
        let sub_out = encode_frame_adaptive(
            &plane_image,
            compression_type,
            block_size.min(pw as u16).max(4),
            false,
            pfq,
            preferred_sub,
            sub_steps,
        )?;
        preferred_sub = sub_out.pred_mode;
        out.extend_from_slice(&(sub_out.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&sub_out.data);
        drop(sub_span);

        // Fast-Fail：剩余子平面每个至少 4(长度前缀)+11(帧头)+1(载荷)=16 字节。
        // 若已累计体积 + 剩余最小体积已超预算，planar 必败，提前终止。
        let remaining = 3 - (pi + 1);
        const MIN_SUB_PLANE_BYTES: usize = 4 + FRAME_HEADER_SIZE + 1;
        if out.len() + remaining * MIN_SUB_PLANE_BYTES > byte_limit {
            return Ok(None);
        }
    }
    Ok(Some(out))
}
