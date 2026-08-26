use super::types::PredictionMode;

/// MED（Median Edge Detection，JPEG-LS LOCO-I）边缘检测预测
///
/// 基于 left(W)/top(N)/top_left(NW) 三邻居的自适应中值预测：
/// - 若 NW >= max(W, N)：predicted = min(W, N)
/// - 若 NW <= min(W, N)：predicted = max(W, N)
/// - 否则：predicted = W + N - NW（平面外推）
///
/// 无需传输任何边信息，编码端与解码端使用完全相同的公式。
#[inline]
fn med_predict(left: i32, top: i32, top_left: i32) -> i32 {
    if top_left >= left.max(top) {
        left.min(top)
    } else if top_left <= left.min(top) {
        left.max(top)
    } else {
        // 平面外推：left + top - top_left（i32 范围内不会溢出）
        left.wrapping_add(top).wrapping_sub(top_left)
    }
}

/// PAETH 预测（AV1 predictor / PNG filter type 4）
///
/// 设 p = left + top - top_left（平面外推），比较三个邻居到 p 的距离：
/// 选与平面外推值最近的邻居作为预测值。
#[inline]
fn paeth_predict(left: i32, top: i32, top_left: i32) -> i32 {
    let p = left.wrapping_add(top).wrapping_sub(top_left);
    let pa = (p - left).abs(); // = |top - top_left|
    let pb = (p - top).abs(); // = |left - top_left|
    let pc = (p - top_left).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        top
    } else {
        top_left
    }
}

/// 计算位置 (x, y) 处的预测值（所有模式的统一核心）
///
/// buf 为参考缓冲：apply 端传入原始像素，undo 端传入"正在重建"的输出缓冲。
/// 预测只依赖 (x-1, y)、(x, y-1)、(x-1, y-1)、(x+1, y-1) 四个因果邻居，
/// 保证编解码两端在光栅顺序处理下得到完全一致的预测值。
#[inline]
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub(crate) fn predict_at(
    buf: &[i32],
    idx: usize,
    x: usize,
    y: usize,
    stride: usize,
    components: usize,
    width: usize,
    mode: PredictionMode,
) -> i32 {
    match mode {
        PredictionMode::Horizontal => {
            if x > 0 {
                buf[idx - components]
            } else {
                0
            }
        }
        PredictionMode::Vertical => {
            if y > 0 {
                buf[idx - stride]
            } else {
                0
            }
        }
        PredictionMode::Average => {
            let left = if x > 0 { buf[idx - components] } else { 0 };
            let top = if y > 0 { buf[idx - stride] } else { 0 };
            (left + top) / 2
        }
        PredictionMode::DC => {
            let left = if x > 0 {
                buf[idx - components] as i64
            } else {
                0
            };
            let top = if y > 0 { buf[idx - stride] as i64 } else { 0 };
            let top_left = if x > 0 && y > 0 {
                buf[idx - stride - components] as i64
            } else {
                0
            };
            let top_right = if x + 1 < width && y > 0 {
                buf[idx - stride + components] as i64
            } else {
                0
            };
            ((left + top + top_left + top_right) / 4) as i32
        }
        PredictionMode::Med => {
            let left = if x > 0 { buf[idx - components] } else { 0 };
            let top = if y > 0 { buf[idx - stride] } else { 0 };
            let top_left = if x > 0 && y > 0 {
                buf[idx - stride - components]
            } else {
                0
            };
            med_predict(left, top, top_left)
        }
        PredictionMode::Paeth => {
            let left = if x > 0 { buf[idx - components] } else { 0 };
            let top = if y > 0 { buf[idx - stride] } else { 0 };
            let top_left = if x > 0 && y > 0 {
                buf[idx - stride - components]
            } else {
                0
            };
            paeth_predict(left, top, top_left)
        }
        // 右上预测（v1.9，AV1 D45 因果简化版）：上行右移一位。
        // 最右列无右上邻居 → 回退 top；首行无上行 → 回退 left；原点回退 0。
        PredictionMode::TopRight => {
            if y > 0 {
                if x + 1 < width {
                    buf[idx - stride + components]
                } else {
                    buf[idx - stride]
                }
            } else if x > 0 {
                buf[idx - components]
            } else {
                0
            }
        }
        // 对角预测（v1.9，AV1 D135 因果简化版）：沿主对角线上溯 k=min(x,y)
        // 步至边界像素——45° 斜线（左上→右下走向）上预测值恒等于当前像素，
        // 残差为零。首行/首列分别回退 left/top，原点回退 0。
        PredictionMode::Diagonal => {
            let k = x.min(y);
            if k > 0 {
                buf[idx - k * (stride + components)]
            } else if y > 0 {
                buf[idx - stride]
            } else if x > 0 {
                buf[idx - components]
            } else {
                0
            }
        }
        // 多参考行预测（v1.13，第四批 #2）：上方第 2 行同列像素。
        // 与 Vertical 组成双行参考窗：2 像素周期横条纹/网点上
        // top-2 与当前像素周期对齐 → 残差恒为零（top 单行参考下
        // 该类纹理残差为满幅方波）。y==1 回退 top；y==0 回退 left；
        // 原点回退 0——边界行为与 TopRight/Diagonal 的平滑退化风格一致。
        PredictionMode::Vertical2 => {
            if y >= 2 {
                buf[idx - 2 * stride]
            } else if y == 1 {
                buf[idx - stride]
            } else if x > 0 {
                buf[idx - components]
            } else {
                0
            }
        }
        // 多参考列预测（v1.13，第四批 #2）：左侧第 2 列同行像素。
        // 2 像素周期竖条纹特化。x==1 回退 left；x==0 回退 top；原点回退 0。
        PredictionMode::Horizontal2 => {
            if x >= 2 {
                buf[idx - 2 * components]
            } else if x == 1 {
                buf[idx - components]
            } else if y > 0 {
                buf[idx - stride]
            } else {
                0
            }
        }
        PredictionMode::None => 0,
    }
}

/// 应用帧内预测（整帧，将原始数据转换为预测残差）
///
/// 水平预测：residual[i] = pixel[i] - left
/// 垂直预测：residual[i] = pixel[i] - top
/// 平均预测：residual[i] = pixel[i] - (left + top) / 2
/// DC预测：residual[i] = pixel[i] - (left + top + top_left + top_right) / 4
/// MED预测：JPEG-LS LOCO-I 边缘检测中值预测
pub fn apply_prediction(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> Vec<i32> {
    let mut out = vec![0i32; pixels.len()];
    apply_prediction_range_into(pixels, &mut out, width, height, components, mode, 0, height);
    out
}

/// 应用帧内预测到指定行范围 [y_start, y_end)，返回整帧长度的残差向量
///
/// 范围外行为零填充。用于条带级自适应编码。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn apply_prediction_range(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    y_start: usize,
    y_end: usize,
) -> Vec<i32> {
    let mut out = vec![0i32; pixels.len()];
    apply_prediction_range_into(
        pixels, &mut out, width, height, components, mode, y_start, y_end,
    );
    out
}

/// 应用帧内预测到指定行范围并写入调用方提供的缓冲区
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
fn apply_prediction_range_into(
    pixels: &[i32],
    residuals: &mut [i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    y_start: usize,
    y_end: usize,
) {
    let y_end = y_end.min(height);
    let stride = width * components;

    if mode == PredictionMode::None {
        // 仅拷贝目标行范围（保持范围外内容不被破坏）
        let start = y_start * stride;
        let stop = y_end * stride;
        residuals[start..stop].copy_from_slice(&pixels[start..stop]);
        return;
    }

    for y in y_start..y_end {
        for x in 0..width {
            for c in 0..components {
                let idx = (y * stride) + (x * components) + c;
                let predicted = predict_at(pixels, idx, x, y, stride, components, width, mode);
                residuals[idx] = pixels[idx] - predicted;
            }
        }
    }
}

/// 撤销帧内预测（整帧，将预测残差还原为原始数据）
pub fn undo_prediction(
    residuals: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> Vec<i32> {
    let mut pixels = vec![0i32; residuals.len()];
    undo_prediction_range_into(residuals, &mut pixels, width, components, mode, 0, height);
    pixels
}

/// 撤销帧内预测到指定行范围 [y_start, y_end)
///
/// pixels_buf 必须是整帧长度且包含上文已重建行（前序条带的输出保留在内），
/// 本函数只重建 [y_start, y_end) 行。用于条带级自适应解码。
pub fn undo_prediction_range(
    residuals: &[i32],
    pixels_buf: &mut [i32],
    width: usize,
    components: usize,
    mode: PredictionMode,
    y_start: usize,
    y_end: usize,
) {
    undo_prediction_range_into(
        residuals, pixels_buf, width, components, mode, y_start, y_end,
    );
}

/// 撤销帧内预测到指定行范围并写入调用方提供的缓冲区
fn undo_prediction_range_into(
    residuals: &[i32],
    pixels: &mut [i32],
    width: usize,
    components: usize,
    mode: PredictionMode,
    y_start: usize,
    y_end: usize,
) {
    let stride = width * components;
    let height = residuals.len() / stride.max(1);
    let y_end = y_end.min(height);

    if mode == PredictionMode::None {
        // 仅拷贝目标行范围（保护前序条带已重建的上文）
        let start = y_start * stride;
        let stop = y_end * stride;
        pixels[start..stop].copy_from_slice(&residuals[start..stop]);
        return;
    }

    for y in y_start..y_end {
        for x in 0..width {
            for c in 0..components {
                let idx = (y * stride) + (x * components) + c;
                let predicted = predict_at(pixels, idx, x, y, stride, components, width, mode);
                pixels[idx] = residuals[idx] + predicted;
            }
        }
    }
}

/// 计算指定行范围 [y_start, y_end) 的**紧凑**残差向量（长度 = 行数 × stride）
///
/// 与 apply_prediction_range 的区别：只分配范围长度的缓冲，
/// 供条带级编码并行处理时降低内存占用。
pub fn apply_prediction_band(
    pixels: &[i32],
    width: usize,
    components: usize,
    mode: PredictionMode,
    y_start: usize,
    y_end: usize,
) -> Vec<i32> {
    let stride = width * components;
    let height = pixels.len() / stride.max(1);
    let y_end = y_end.min(height);
    let mut out = vec![0i32; (y_end.saturating_sub(y_start)) * stride];
    if y_start >= y_end {
        return out;
    }

    if mode == PredictionMode::None {
        out.copy_from_slice(&pixels[y_start * stride..y_end * stride]);
        return out;
    }

    for (row, y) in (y_start..y_end).enumerate() {
        let base_in = y * stride;
        let base_out = row * stride;
        for x in 0..width {
            for c in 0..components {
                let idx = base_in + x * components + c;
                let predicted = predict_at(pixels, idx, x, y, stride, components, width, mode);
                out[base_out + x * components + c] = pixels[idx] - predicted;
            }
        }
    }
    out
}

/// 死区标量量化单点（round-to-nearest，输出为 Q 的倍数）
#[inline]
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
fn quant_scalar(v: i32, q: i32) -> i32 {
    let half = q / 2;
    if v >= 0 {
        (v + half) / q * q
    } else {
        -(((-v) + half) / q * q)
    }
}

/// 带死区偏置的单点量化：bias 为 /64 定点偏置。
/// **正 bias 单侧加宽负残差死区（负向 ±1~±3 映射为 0），负 bias 单侧加宽正向死区——与批量路径 quantize_residuals_tuned 的双向语义相反，标定以实测为准（optimization-review §12）。**
#[inline]
fn quant_scalar_biased(v: i32, q: i32, bias_r6: i32) -> i32 {
    let denom = q * 64;
    let half_r6 = denom / 2;
    let b = if v >= 0 { bias_r6 } else { -bias_r6 };
    // 对绝对值做 round-to-nearest 再恢复符号（避免负数截断非对称）
    let level = (v.wrapping_abs() * 64 + half_r6 + b) / denom;
    let sign = if v < 0 { -1 } else { 1 };
    sign * level * q
}

/// 闭环预测 + 死区量化（真有损模式核心）
///
/// 编码端逐像素从**重建缓冲**取预测邻居（与解码端完全一致），
/// 从根源上杜绝开环预测的误差级联漂移。
/// 每像素重建误差恰为该点残差的量化误差，≤ ⌊Q/2⌋。
///
/// 返回（量化残差流, 重建帧）；熵编码量化残差，逆预测作用于其上即得重建帧。
#[allow(dead_code)] // 编解码器对称 API/测试路径依赖，当前入口未直接调用
pub fn closed_loop_predict_quant(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    q_step: u8,
    deadzone_bias: i8,
) -> (Vec<i32>, Vec<i32>) {
    closed_loop_predict_quant_banded(
        pixels,
        width,
        height,
        components,
        mode,
        q_step,
        deadzone_bias,
        None,
    )
}

/// 闭环预测 + 死区量化 + **逐条带自适应步长**（噪声归一化执行器）
///
/// [`closed_loop_predict_quant`] 的超集：`band_steps` 提供每
/// BAND_HEIGHT 行条带的有效量化步长覆盖表——失真水平高的条带
/// （天气颗粒/色调漂移）死区自动加宽，失真低的条带保持基础步长
/// 精细度。空间预测天然完成"中心化"（常数漂移被邻居预测抵消），
/// 因此无需零中心性判定，直接按条带放大死区即可。
///
/// 正确性约束：
/// - 条带按光栅顺序处理，与编码端条带划分（BAND_HEIGHT 行）对齐；
/// - `band_steps.len()` 须为 ceil(height / BAND_HEIGHT)，越界部分回退基础步长；
/// - 解码端无感：输出仍为各条带 Q_eff 倍数的自描述残差，undo_prediction
///   不需要知道任何步长信息，格式零改动。
#[allow(clippy::too_many_arguments)] // 编码器领域函数，参数为算法固有维度
pub fn closed_loop_predict_quant_banded(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
    q_step: u8,
    deadzone_bias: i8,
    band_steps: Option<&[u8]>,
) -> (Vec<i32>, Vec<i32>) {
    let stride = width * components;
    let n = pixels.len();
    let mut residuals = vec![0i32; n];
    let mut recon = vec![0i32; n];

    // 行 → 条带有效步长的快速查表；None 时全帧统一
    let band_h = crate::crf::core::bitstream::constants::BAND_HEIGHT;
    let step_of_band = |y: usize| -> u8 {
        match band_steps {
            Some(t) if !t.is_empty() => t[(y / band_h).min(t.len() - 1)].max(1),
            _ => q_step.max(1),
        }
    };

    if mode == PredictionMode::None {
        // 无空间预测：直接对原始值量化
        for y in 0..height {
            let q = step_of_band(y);
            for x in 0..width {
                for c in 0..components {
                    let idx = y * stride + x * components + c;
                    let vq = quant_scalar_biased(pixels[idx], q as i32, deadzone_bias as i32);
                    residuals[idx] = vq;
                    recon[idx] = vq;
                }
            }
        }
        return (residuals, recon);
    }

    for y in 0..height {
        let q_row = step_of_band(y);
        for x in 0..width {
            for c in 0..components {
                let idx = y * stride + x * components + c;
                let predicted = predict_at(&recon, idx, x, y, stride, components, width, mode);
                let raw = pixels[idx] - predicted;
                let quantized = quant_scalar_biased(raw, q_row as i32, deadzone_bias as i32);
                residuals[idx] = quantized;
                recon[idx] = predicted + quantized;
            }
        }
    }
    (residuals, recon)
}

/// 行采样 SAD 快速估计（用于预测模式排序，非精确值）
/// 每隔 4 行统计一行的残差绝对值和并按比例放大。
/// 排序用途下与精确 SAD 的相对序高度一致，评估开销降为约 1/4。
pub fn sad_for_mode_sampled(
    pixels: &[i32],
    width: usize,
    height: usize,
    components: usize,
    mode: PredictionMode,
) -> u64 {
    const ROW_STRIDE: usize = 4;
    if mode == PredictionMode::None {
        return 0;
    }
    let stride = width * components;
    let mut sad: u64 = 0;
    let mut sampled_rows = 0usize;
    let mut y = 0usize;
    while y < height {
        for x in 0..width {
            for c in 0..components {
                let idx = y * stride + x * components + c;
                let predicted = predict_at(pixels, idx, x, y, stride, components, width, mode);
                sad += pixels[idx].wrapping_sub(predicted).unsigned_abs() as u64;
            }
        }
        sampled_rows += 1;
        y += ROW_STRIDE;
    }
    if sampled_rows == 0 {
        0
    } else {
        // 按采样行占比放大回全帧量级
        sad.saturating_mul(height as u64 / sampled_rows as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带渐变和边缘的测试图像
    fn make_test_image(width: usize, height: usize) -> Vec<i32> {
        let mut pixels = vec![0i32; width * height];
        for y in 0..height {
            for x in 0..width {
                // 渐变 + 竖直边缘 + 噪声样图案
                pixels[y * width + x] = if x < width / 2 {
                    ((x * 2 + y) % 251) as i32
                } else {
                    (200 - (x + y * 3) % 200) as i32
                };
            }
        }
        pixels
    }

    #[test]
    fn test_med_roundtrip() {
        let width = 33; // 非对齐尺寸覆盖边界分支
        let height = 17;
        let original = make_test_image(width, height);

        for mode in [
            PredictionMode::None,
            PredictionMode::Horizontal,
            PredictionMode::Vertical,
            PredictionMode::Average,
            PredictionMode::DC,
            PredictionMode::Med,
            PredictionMode::Paeth,
            PredictionMode::TopRight,
            PredictionMode::Diagonal,
            PredictionMode::Vertical2,
            PredictionMode::Horizontal2,
        ] {
            let residuals = apply_prediction(&original, width, height, 1, mode);
            let restored = undo_prediction(&residuals, width, height, 1, mode);
            assert_eq!(original, restored, "模式 {:?} 往返失败", mode);
        }
    }

    #[test]
    fn test_paeth_matches_png_reference() {
        // PNG 规范参考值：left=10, top=12, topleft=5
        // p = 17; pa=|17-10|=7, pb=|17-12|=5, pc=|17-5|=12 → pb 最小 → top(12)
        assert_eq!(paeth_predict(10, 12, 5), 12);

        // left=15, top=10, topleft=10: p=15; pa=|15-15|=0 最小 → left
        assert_eq!(paeth_predict(15, 10, 10), 15);

        // 梯度上升：left=8, top=9, topleft=6: p=11; pa=3, pb=2, pc=5 → top(9)
        assert_eq!(paeth_predict(8, 9, 6), 9);

        // 平面外推精确时选 top_left：left=20, top=30, topleft=10
        // p=40; pa=20, pb=10, pc=30 → pb 最小 → top(30)
        assert_eq!(paeth_predict(20, 30, 10), 30);
    }

    #[test]
    fn test_med_predict_logic() {
        // NW >= max(W,N) → min(W,N)
        assert_eq!(med_predict(10, 20, 30), 10);
        // NW <= min(W,N) → max(W,N)
        assert_eq!(med_predict(10, 20, 5), 20);
        // 否则平面外推 W + N - NW
        assert_eq!(med_predict(10, 20, 15), 15);
        assert_eq!(med_predict(20, 10, 15), 15);
    }

    /// 斜向预测（v1.9）精确值验证：
    /// 3×3 图像，像素值 = 行列坐标线性组合，便于手工推演。
    /// 布局（行优先）：[0, 1, 2] / [3, 4, 5] / [6, 7, 8]
    #[test]
    fn test_directional_modes_exact_values() {
        let width = 3usize;
        let pixels: Vec<i32> = (0..9).map(|i| i as i32).collect();

        // Diagonal 内部点 (2,2)：k=min(2,2)=2 → 上溯 2*(stride+1)=idx-8 → 像素(0,0)=0
        assert_eq!(
            predict_at(&pixels, 8, 2, 2, width, 1, width, PredictionMode::Diagonal),
            0
        );
        // Diagonal 边缘点 (2,1)：k=1 → 上溯 1 步 → top_left=(1,0)=1
        assert_eq!(
            predict_at(&pixels, 5, 2, 1, width, 1, width, PredictionMode::Diagonal),
            1
        );
        // Diagonal 首行 (2,0)：k=0 且 y==0 → 回退 left
        assert_eq!(
            predict_at(&pixels, 2, 2, 0, width, 1, width, PredictionMode::Diagonal),
            1
        );
        // Diagonal 首列 (0,2)：k=0 且 x==0 → 回退 top
        assert_eq!(
            predict_at(&pixels, 6, 0, 2, width, 1, width, PredictionMode::Diagonal),
            3
        );
        // Diagonal 原点：全边界回退 0
        assert_eq!(
            predict_at(&pixels, 0, 0, 0, width, 1, width, PredictionMode::Diagonal),
            0
        );

        // TopRight 内部点 (0,1)：右上邻居 = (1,0)=1
        assert_eq!(
            predict_at(&pixels, 3, 0, 1, width, 1, width, PredictionMode::TopRight),
            1
        );
        // TopRight 最右列 (2,1)：无右上 → 回退 top=(2,0)=2
        assert_eq!(
            predict_at(&pixels, 5, 2, 1, width, 1, width, PredictionMode::TopRight),
            2
        );
        // TopRight 首行 (1,0)：无上行 → 回退 left
        assert_eq!(
            predict_at(&pixels, 1, 1, 0, width, 1, width, PredictionMode::TopRight),
            0
        );
        // TopRight 原点：回退 0
        assert_eq!(
            predict_at(&pixels, 0, 0, 0, width, 1, width, PredictionMode::TopRight),
            0
        );
    }

    /// 斜向预测在精确斜线图案上应产生零残差（存在价值验证）：
    /// - Diagonal 覆盖"\"走向（常值沿主对角线）；
    /// - TopRight 覆盖"/"走向（常值沿反对角线）。
    #[test]
    fn test_directional_modes_zero_residual_on_diagonals() {
        let n = 16usize;
        // "\"走向：pixel(x,y) = x - y（沿主对角线恒定）
        let bs: Vec<i32> = (0..n * n)
            .map(|i| {
                let x = (i % n) as i32;
                let y = (i / n) as i32;
                x - y
            })
            .collect();
        let res_bs = apply_prediction(&bs, n, n, 1, PredictionMode::Diagonal);
        // 首行/首列边界回退不归零，其余必须全部为零
        for y in 1..n {
            for x in 1..n {
                assert_eq!(
                    res_bs[y * n + x],
                    0,
                    "Diagonal 在 \\ 斜线内部点 ({},{}) 残差非零",
                    x,
                    y
                );
            }
        }

        // "/"走向：pixel(x,y) = x + y（沿反对角线恒定）
        let fs: Vec<i32> = (0..n * n)
            .map(|i| {
                let x = (i % n) as i32;
                let y = (i / n) as i32;
                x + y
            })
            .collect();
        let res_fs = apply_prediction(&fs, n, n, 1, PredictionMode::TopRight);
        for y in 1..n {
            for x in 0..n - 1 {
                assert_eq!(
                    res_fs[y * n + x],
                    0,
                    "TopRight 在 / 斜线内部点 ({},{}) 残差非零",
                    x,
                    y
                );
            }
        }
    }

    /// 多参考行/列预测（v1.13，第四批 #2）精确值与存在价值验证：
    /// - Vertical2 在 2px 周期**横**条纹上内部点残差恒为零；
    /// - Horizontal2 在 2px 周期**竖**条纹上内部点残差恒为零
    ///   （单行/单列参考下该类纹理残差为满幅方波）；
    /// - 边界回退链（y==1→top / y==0→left 等）手工推演验证。
    #[test]
    fn test_multi_ref_modes_exact_and_stripes() {
        let w = 6usize;
        let h = 5usize;
        // 竖条纹（严格 2px 周期）：pixel = (x % 2) * 80 —— 逐列交替 0/80
        let vstripes: Vec<i32> = (0..w * h)
            .map(|i| {
                let x = i % w;
                (x % 2) as i32 * 80
            })
            .collect();
        // Horizontal2 内部点：x≥2 时引用 x−2 同列同行——周期 2 对齐 → 残差零。
        // 该分支不依赖 y（含首行），全帧有效。
        let res_h2 = apply_prediction(&vstripes, w, h, 1, PredictionMode::Horizontal2);
        for y in 0..h {
            for x in 2..w {
                assert_eq!(
                    res_h2[y * w + x],
                    0,
                    "Horizontal2 在竖条纹内部点 ({},{}) 残差非零",
                    x,
                    y
                );
            }
        }
        // 边界回退手工验证：
        // 原点 (0,0)：x==0 且 y==0 → 预测 0；像素 (0%2)*80=0 → 残差 0
        assert_eq!(res_h2[0], 0);
        // (0,1)：x==0 回退 top=(0,0)=0；像素 0 → 残差 0
        assert_eq!(res_h2[w + 0], 0);
        // (1,0)：y==0 且 x==1 回退 left=(0,0)=0；像素 80 → 残差 80
        //（首两行/列的边界代价——内部点恒零才是模式价值所在）
        assert_eq!(res_h2[1], 80);

        // 横条纹（严格 2px 周期）：pixel = (y % 2) * 60 —— 逐行交替 0/60
        let hstripes: Vec<i32> = (0..w * h)
            .map(|i| {
                let y = i / w;
                (y % 2) as i32 * 60
            })
            .collect();
        // Vertical2 内部点：y≥2 时引用 y−2 同列——周期 2 对齐 → 残差零。
        // 该分支不依赖 x，整行有效。
        let res_v2 = apply_prediction(&hstripes, w, h, 1, PredictionMode::Vertical2);
        for y in 2..h {
            for x in 0..w {
                assert_eq!(
                    res_v2[y * w + x],
                    0,
                    "Vertical2 在横条纹内部点 ({},{}) 残差非零",
                    x,
                    y
                );
            }
        }
        // y==0 行：x>0 回退 left（行 0 全零）→ 残差 = 像素 = 0；原点亦零
        for x in 0..w {
            assert_eq!(res_v2[x], 0, "Vertical2 行0 ({},0) 应归零", x);
        }
        // y==1 行：回退 top（行 0 全零）→ 残差 = 像素 = 60
        for x in 0..w {
            assert_eq!(
                res_v2[w + x],
                60,
                "Vertical2 行1 ({},1) 边界残差应为满幅",
                x
            );
        }

        // 单调性对照：单行参考 Vertical 在行 3（60 值区）残差为满幅方波，
        // 证明 V2 的收益来自双行参考窗而非巧合
        let res_v1 = apply_prediction(&hstripes, w, h, 1, PredictionMode::Vertical);
        assert!(
            res_v1[3 * w].unsigned_abs() == 60,
            "单行参考在 2px 条纹上应产生满幅方波残差（对照基准）"
        );
    }

    #[test]
    fn test_med_residual_entropy_beats_average_on_smooth() {
        // 平滑渐变图像上 MED 残差绝对值和应不高于平均预测
        let width: usize = 64;
        let height: usize = 64;
        let original: Vec<i32> = (0..width * height)
            .map(|i| {
                let x = i % width;
                let y = i / width;
                (100 + x / 4 + y / 8) as i32
            })
            .collect();

        let sad_med: u64 = apply_prediction(&original, width, height, 1, PredictionMode::Med)
            .iter()
            .map(|&v| v.unsigned_abs() as u64)
            .sum();
        let sad_avg: u64 = apply_prediction(&original, width, height, 1, PredictionMode::Average)
            .iter()
            .map(|&v| v.unsigned_abs() as u64)
            .sum();

        assert!(
            sad_med <= sad_avg,
            "平滑图上 MED SAD({}) 应不超过 Average SAD({})",
            sad_med,
            sad_avg
        );
    }

    #[test]
    fn test_range_versions_match_full_frame() {
        // 行范围版本分块处理的结果必须与整帧一次处理逐字节一致（条带化正确性的根基）
        let width = 37;
        let height = 53;
        let components = 3;
        let original = make_test_image(width * components, height);

        for mode in [
            PredictionMode::Horizontal,
            PredictionMode::Vertical,
            PredictionMode::Average,
            PredictionMode::DC,
            PredictionMode::Med,
            PredictionMode::TopRight,
            PredictionMode::Diagonal,
            PredictionMode::Vertical2,
            PredictionMode::Horizontal2,
        ] {
            // 整帧参考结果
            let ref_res = apply_prediction(&original, width, height, components, mode);
            let ref_undo = undo_prediction(&ref_res, width, height, components, mode);
            assert_eq!(original, ref_undo, "{:?} 整帧往返失败", mode);

            // 按 16 行条带分段 apply
            let band = 16;
            let mut banded_res = vec![0i32; original.len()];
            let mut y = 0;
            while y < height {
                let y_end = (y + band).min(height);
                let seg =
                    apply_prediction_range(&original, width, height, components, mode, y, y_end);
                let start = y * width * components;
                let stop = y_end * width * components;
                banded_res[start..stop].copy_from_slice(&seg[start..stop]);
                y = y_end;
            }
            assert_eq!(ref_res, banded_res, "{:?} 条带化 apply 与整帧不一致", mode);

            // 按 16 行条带分段 undo（模拟解码端的渐进重建）
            let mut banded_undo = vec![0i32; original.len()];
            let mut y = 0;
            while y < height {
                let y_end = (y + band).min(height);
                undo_prediction_range(
                    &banded_res,
                    &mut banded_undo,
                    width,
                    components,
                    mode,
                    y,
                    y_end,
                );
                y = y_end;
            }
            assert_eq!(original, banded_undo, "{:?} 条带化 undo 往返失败", mode);
        }
    }
}
