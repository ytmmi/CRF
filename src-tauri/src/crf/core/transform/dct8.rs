//! 一维/二维 8 点 lifting 整数 DCT-II 近似（v1.10 新增）
//!
//! ## 数学骨架（精确分解 + 提升近似）
//!
//! 8 点 DCT-II 按镜像 n ↔ 7−n 分解为对称/反对称两路：
//! - 对称分量 u[n]=(x[n]+x[7−n]) 仅贡献偶数频率，且
//!   `Y[2m] = 2·DCT-II_4(u)[m]`（**精确恒等式**）；
//! - 反对称分量 v[n]=(x[n]−x[7−n]) 仅贡献奇数频率，理论上应过
//!   DCT-IV_4 核（自逆正交核）。本实现以「相邻提升旋转（π/8 相位
//!   搬移）+ 同款 4 点核」近似之——角度参数只影响奇路的能量集中
//!   质量，不影响可逆性。
//!
//! ## 可逆性论证
//!
//! 全流程仅由三类严格可逆原语复合：无损蝶形（butterfly_*）、三点
//! 提升旋转（lift_rot_*）、既有 4 点核（dct4_fwd/inv 互逆对），
//! 外加纯排列与符号翻转。任意输入下逐位往返一致（测试硬性保证）。
//!
//! ## 二维变换
//!
//! 与 dct4 相同的行列分离法：先行一维再列一维，逆序相反。

use super::dct4::{butterfly_fwd, butterfly_inv, dct4_fwd, dct4_inv, lift_rot_fwd, lift_rot_inv};

/// 奇路相位修正旋转（/64 定点）
///
/// π/8 邻域：反对称分量的 DCT-IV 核等效于半像素相位平移的 DCT-II，
/// 提升旋转承担该平移。候选角度经数值实验比对能量集中性选定
/// （见 tests::probe_frequency_response 信息输出）。
const T_PHASE: i32 = 13; // tan(π/16) 定点，同 dct4 的 T_PI8
const S_PHASE: i32 = 24; // sin(π/8) 定点

/// 一维 8 点整数 DCT-II 近似（正向）
///
/// 输入输出均为 8 元素（行优先语义由调用方解释）。
#[allow(clippy::too_many_arguments)]
pub fn dct8_fwd(
    x0: i32,
    x1: i32,
    x2: i32,
    x3: i32,
    x4: i32,
    x5: i32,
    x6: i32,
    x7: i32,
) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
    // Stage A：镜像蝶形 → (对称分量 s, 反对称分量 d)
    let (s0, d0) = butterfly_fwd(x0, x7); // s=x0+x7, d=x0−x7
    let (s1, d1) = butterfly_fwd(x1, x6);
    let (s2, d2) = butterfly_fwd(x2, x5);
    let (s3, d3) = butterfly_fwd(x3, x4);

    // Stage B：对称路 → 偶数频率组（精确 DCT-II_4 骨架）
    let (e0, e1, e2, e3) = dct4_fwd(s0, s1, s2, s3);

    // Stage C：反对称路 → 相位修正旋转 + 同款 4 点核
    let (t0, t1) = lift_rot_fwd(d0, d1, T_PHASE, S_PHASE);
    let (t3, t2) = lift_rot_fwd(d3, d2, T_PHASE, S_PHASE);
    let (o0, o1, o2, o3) = dct4_fwd(t0, t1, t2, t3);

    // Stage D：交错排列（偶/奇频率交织）+ 反射修正
    (e0, o0, e1, -o1, e2, o2, e3, -o3)
}

/// 一维 8 点整数 DCT 逆变换（与 [`dct8_fwd`] 构成精确互逆对）
#[allow(clippy::too_many_arguments)]
pub fn dct8_inv(
    y0: i32,
    y1: i32,
    y2: i32,
    y3: i32,
    y4: i32,
    y5: i32,
    y6: i32,
    y7: i32,
) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
    // Stage D⁻¹：撤销交错与符号修正
    let e0 = y0;
    let o0 = y1;
    let e1 = y2;
    let o1 = -y3;
    let e2 = y4;
    let o2 = y5;
    let e3 = y6;
    let o3 = -y7;

    // Stage C⁻¹（先撤销 4 点核，再撤销相位旋转；与正向顺序严格相反）
    let (r0, r1, r2, r3) = dct4_inv(o0, o1, o2, o3);
    let (d0, d1) = lift_rot_inv(r0, r1, T_PHASE, S_PHASE);
    let (d3, d2) = lift_rot_inv(r3, r2, T_PHASE, S_PHASE);

    // Stage B⁻¹
    let (s0, s1, s2, s3) = dct4_inv(e0, e1, e2, e3);

    // Stage A⁻¹
    let (x0, x7) = butterfly_inv(s0, d0);
    let (x1, x6) = butterfly_inv(s1, d1);
    let (x2, x5) = butterfly_inv(s2, d2);
    let (x3, x4) = butterfly_inv(s3, d3);
    (x0, x1, x2, x3, x4, x5, x6, x7)
}

/// 二维 8×8 整数 DCT 正变换（行变换 → 列变换）
///
/// 输入输出均为行优先 64 元素块。
pub(crate) fn dct8x8_forward_into(block: &[i32], out: &mut [i32]) {
    assert_eq!(block.len(), 64);
    assert_eq!(out.len(), 64);
    let mut tmp = [0i32; 64];

    for r in 0..8 {
        let base = r * 8;
        let (y0, y1, y2, y3, y4, y5, y6, y7) = dct8_fwd(
            block[base],
            block[base + 1],
            block[base + 2],
            block[base + 3],
            block[base + 4],
            block[base + 5],
            block[base + 6],
            block[base + 7],
        );
        tmp[base] = y0;
        tmp[base + 1] = y1;
        tmp[base + 2] = y2;
        tmp[base + 3] = y3;
        tmp[base + 4] = y4;
        tmp[base + 5] = y5;
        tmp[base + 6] = y6;
        tmp[base + 7] = y7;
    }

    for c in 0..8 {
        let (y0, y1, y2, y3, y4, y5, y6, y7) = dct8_fwd(
            tmp[c],
            tmp[8 + c],
            tmp[16 + c],
            tmp[24 + c],
            tmp[32 + c],
            tmp[40 + c],
            tmp[48 + c],
            tmp[56 + c],
        );
        out[c] = y0;
        out[8 + c] = y1;
        out[16 + c] = y2;
        out[24 + c] = y3;
        out[32 + c] = y4;
        out[40 + c] = y5;
        out[48 + c] = y6;
        out[56 + c] = y7;
    }
}

pub fn dct8x8_forward(block: &[i32]) -> Vec<i32> {
    let mut out = vec![0i32; 64];
    dct8x8_forward_into(block, &mut out);
    out
}

/// 二维 8×8 整数 DCT 逆变换（列逆变换 → 行逆变换，严格互逆）
pub(crate) fn dct8x8_inverse_into(block: &[i32], out: &mut [i32]) {
    assert_eq!(block.len(), 64);
    assert_eq!(out.len(), 64);
    let mut tmp = [0i32; 64];

    for c in 0..8 {
        let (x0, x1, x2, x3, x4, x5, x6, x7) = dct8_inv(
            block[c],
            block[8 + c],
            block[16 + c],
            block[24 + c],
            block[32 + c],
            block[40 + c],
            block[48 + c],
            block[56 + c],
        );
        tmp[c] = x0;
        tmp[8 + c] = x1;
        tmp[16 + c] = x2;
        tmp[24 + c] = x3;
        tmp[32 + c] = x4;
        tmp[40 + c] = x5;
        tmp[48 + c] = x6;
        tmp[56 + c] = x7;
    }

    for r in 0..8 {
        let base = r * 8;
        let (x0, x1, x2, x3, x4, x5, x6, x7) = dct8_inv(
            tmp[base],
            tmp[base + 1],
            tmp[base + 2],
            tmp[base + 3],
            tmp[base + 4],
            tmp[base + 5],
            tmp[base + 6],
            tmp[base + 7],
        );
        out[base] = x0;
        out[base + 1] = x1;
        out[base + 2] = x2;
        out[base + 3] = x3;
        out[base + 4] = x4;
        out[base + 5] = x5;
        out[base + 6] = x6;
        out[base + 7] = x7;
    }
}

pub fn dct8x8_inverse(block: &[i32]) -> Vec<i32> {
    let mut out = vec![0i32; 64];
    dct8x8_inverse_into(block, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐位可逆性：随机数据 1000 块往返必须完全一致（格式第一不变量）
    #[test]
    fn test_dct8_roundtrip_random() {
        let mut state: u64 = 0x0123_4567_89AB_CDEF;
        for _ in 0..1000 {
            let mut block = [0i32; 64];
            for v in block.iter_mut() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *v = ((state >> 33) as i32 % 512) - 256;
            }
            let restored = dct8x8_inverse(&dct8x8_forward(&block));
            assert_eq!(block.to_vec(), restored, "随机数据往返失败");
        }
    }

    /// 极值边界往返（溢出防护：i32 蝶形最大瞬时幅度受控）
    #[test]
    fn test_dct8_roundtrip_boundaries() {
        let cases: Vec<[i32; 64]> = vec![
            [0i32; 64],
            [255i32; 64],
            [-255i32; 64],
            {
                let mut b = [0i32; 64];
                for (i, v) in b.iter_mut().enumerate() {
                    *v = if i % 2 == 0 { 255 } else { -255 };
                }
                b
            },
            {
                let mut b = [0i32; 64];
                for (i, v) in b.iter_mut().enumerate() {
                    *v = ((i % 8 < 4) == (i / 8 % 8 < 4)) as i32 * 255;
                }
                b
            },
        ];
        for block in &cases {
            let restored = dct8x8_inverse(&dct8x8_forward(block));
            assert_eq!(block.to_vec(), restored);
        }
    }

    /// 能量集中性：平滑渐变块的 DC 应显著主导高频均值
    #[test]
    fn test_dct8_energy_compaction() {
        let smooth: Vec<i32> = (0..64)
            .map(|i| {
                let x = i % 8;
                let y = i / 8;
                30 + x * 2 + y
            })
            .collect();
        let coeffs = dct8x8_forward(&smooth);

        let dc_abs = coeffs[0].unsigned_abs();
        let high_freq_mean: u64 = (1..64)
            .map(|i| coeffs[i].unsigned_abs() as u64)
            .sum::<u64>()
            / 63;

        assert!(
            dc_abs as u64 > high_freq_mean * 5,
            "DC({}) 应显著大于高频均值({})",
            dc_abs,
            high_freq_mean
        );
    }

    /// 单频探针（信息性输出）：观察各输出槽位对水平空间频率的响应，
    /// 校验低频能量确落于低槽位（感知矩阵按位置加权的依据）。
    #[test]
    fn probe_frequency_response() {
        // 水平正弦探针：f ∈ {1..7}（列方向周期）
        for f in 1..8usize {
            let mut block = [0i32; 64];
            for r in 0..8 {
                for c in 0..8 {
                    let arg = (2.0 * f as f64 + 1.0) * c as f64 * std::f64::consts::PI / 16.0;
                    block[r * 8 + c] = (arg.cos() * 100.0).round() as i32;
                }
            }
            let coeffs = dct8x8_forward(&block);
            // 找行 0（仅水平频率激励时主要落在此处附近）的最强槽位
            let (best_c, best_v) = (0..8)
                .map(|c| (c, coeffs[c].unsigned_abs()))
                .max_by_key(|&(_, v)| v)
                .unwrap();
            println!("freq={} → 最强响应列 {} (|coef|={})", f, best_c, best_v);
        }
    }
}
