//! 一维/二维 4 点 lifting 整数 DCT-II 近似
//!
//! 数学构造保证**严格可逆**，与系数精度无关：
//! - 蝶形使用无损提升形式：`d1 = x0 − x3; d0 = x3 + ⌊d1/2⌋`，
//!   正逆变换调用同一舍入函数 `div_euclid(2)`，逐位可逆；
//! - 平面旋转使用三点提升分解，每一步形如 `v += w·k` 的提升，
//!   其逆恰为 `v −= w·k`——定点系数的舍入误差不影响可逆性。
//!
//! 频域特性近似 DCT-II（未归一化），能量集中性接近浮点 DCT。

/// 定点乘法（/64 精度，四舍五入；正逆共用同一函数）
#[inline]
pub(crate) fn mul_r6(v: i32, k: i32) -> i32 {
    (v * k + 32) >> 6
}

/// 无损蝶形：(x0, x3) → (d0, d1)
///
/// forward: d1 = x0 − x3;  d0 = x3 + ⌊d1/2⌋
/// inverse: x3 = d0 − ⌊d1/2⌋;  x0 = x3 + d1
#[inline]
pub(crate) fn butterfly_fwd(x0: i32, x3: i32) -> (i32, i32) {
    let d1 = x0 - x3;
    let d0 = x3 + d1.div_euclid(2);
    (d0, d1)
}

#[inline]
pub(crate) fn butterfly_inv(d0: i32, d1: i32) -> (i32, i32) {
    let x3 = d0 - d1.div_euclid(2);
    let x0 = d1 + x3;
    (x0, x3)
}

// 旋转参数（/64 定点）
// rot(π/4)：tan(π/8)=0.41421→27；sin(π/4)=0.70711→45
const T_PI4: i32 = 27;
const S_PI4: i32 = 45;
// rot(π/8)：tan(π/16)=0.19891→13；sin(π/8)=0.38268→24
const T_PI8: i32 = 13;
const S_PI8: i32 = 24;

/// 三点提升旋转（正向）
///
/// 近似 [[cosθ, sinθ], [−sinθ, cosθ]] · (a, b)，整体增益 ≈1+ε 且正逆一致。
#[inline]
pub(crate) fn lift_rot_fwd(a: i32, b: i32, t: i32, s: i32) -> (i32, i32) {
    let b1 = b - mul_r6(a, t);
    let a1 = a + mul_r6(b1, s);
    let b2 = b1 - mul_r6(a1, t);
    (a1, b2)
}

/// 三点提升旋转（逆向）：按正向相反顺序逐步减去提升量
#[inline]
pub(crate) fn lift_rot_inv(a1: i32, b2: i32, t: i32, s: i32) -> (i32, i32) {
    let b1 = b2 + mul_r6(a1, t);
    let a = a1 - mul_r6(b1, s);
    let b = b1 + mul_r6(a, t);
    (a, b)
}

/// 一维 4 点整数 DCT-II 近似（正向）
///
/// 结构：两组无损蝶形 + rot(π/4) 与 rot(π/8)；
/// 第二个旋转的第二输出取负号以匹配 DCT-II 系数符号（反射修正，可逆）。
pub fn dct4_fwd(x0: i32, x1: i32, x2: i32, x3: i32) -> (i32, i32, i32, i32) {
    // 蝶形：(x0,x3) 与 (x1,x2) 各自成对
    let (d0, d1) = butterfly_fwd(x0, x3);
    let (d2, d3) = butterfly_fwd(x1, x2);

    // rot(π/4) 处理直流组合对 (d0,d2)；rot(π/8) 处理交流对 (d1,d3)
    let (y0, r2) = lift_rot_fwd(d0, d2, T_PI4, S_PI4);
    let y2 = -r2; // 反射修正
    let (y1, r3) = lift_rot_fwd(d1, d3, T_PI8, S_PI8);
    let y3 = -r3;

    (y0, y1, y2, y3)
}

/// 一维 4 点整数 DCT 逆变换（与 dct4_fwd 构成精确可逆对）
pub fn dct4_inv(y0: i32, y1: i32, y2: i32, y3: i32) -> (i32, i32, i32, i32) {
    let (d0, d2) = lift_rot_inv(y0, -y2, T_PI4, S_PI4);
    let (d1, d3) = lift_rot_inv(y1, -y3, T_PI8, S_PI8);

    let (x0, x3) = butterfly_inv(d0, d1);
    let (x1, x2) = butterfly_inv(d2, d3);

    (x0, x1, x2, x3)
}

/// 二维 4×4 整数 DCT 正变换（行变换 → 列变换）
///
/// 输入输出均为行优先 16 元素块。
pub fn dct4x4_forward(block: &[i32]) -> Vec<i32> {
    assert_eq!(block.len(), 16);
    let mut tmp = [0i32; 16];

    // 行变换
    for r in 0..4 {
        let base = r * 4;
        let (y0, y1, y2, y3) = dct4_fwd(
            block[base],
            block[base + 1],
            block[base + 2],
            block[base + 3],
        );
        tmp[base] = y0;
        tmp[base + 1] = y1;
        tmp[base + 2] = y2;
        tmp[base + 3] = y3;
    }

    // 列变换
    let mut out = vec![0i32; 16];
    for c in 0..4 {
        let (y0, y1, y2, y3) = dct4_fwd(tmp[c], tmp[4 + c], tmp[8 + c], tmp[12 + c]);
        out[c] = y0;
        out[4 + c] = y1;
        out[8 + c] = y2;
        out[12 + c] = y3;
    }
    out
}

/// 二维 4×4 整数 DCT 逆变换（列逆变换 → 行逆变换，与正变换严格互逆）
pub fn dct4x4_inverse(block: &[i32]) -> Vec<i32> {
    assert_eq!(block.len(), 16);
    let mut tmp = [0i32; 16];

    // 列逆变换（先撤销后执行的列正变换）
    for c in 0..4 {
        let (x0, x1, x2, x3) = dct4_inv(block[c], block[4 + c], block[8 + c], block[12 + c]);
        tmp[c] = x0;
        tmp[4 + c] = x1;
        tmp[8 + c] = x2;
        tmp[12 + c] = x3;
    }

    // 行逆变换
    let mut out = vec![0i32; 16];
    for r in 0..4 {
        let base = r * 4;
        let (x0, x1, x2, x3) = dct4_inv(tmp[base], tmp[base + 1], tmp[base + 2], tmp[base + 3]);
        out[base] = x0;
        out[base + 1] = x1;
        out[base + 2] = x2;
        out[base + 3] = x3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dct4_roundtrip_random() {
        let mut state: u64 = 0xDEADBEEFCAFEBABE;
        for _ in 0..200 {
            let mut block = [0i32; 16];
            for v in block.iter_mut() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *v = ((state >> 33) as i32 % 512) - 256;
            }
            let restored = dct4x4_inverse(&dct4x4_forward(&block));
            assert_eq!(block.to_vec(), restored, "随机数据往返失败");
        }
    }

    #[test]
    fn test_dct4_roundtrip_boundaries() {
        // 边界极值数据往返
        let cases: Vec<[i32; 16]> = vec![[0i32; 16], [255i32; 16], [-255i32; 16], {
            let mut b = [0i32; 16];
            for (i, v) in b.iter_mut().enumerate() {
                *v = if i % 2 == 0 { 255 } else { -255 };
            }
            b
        }];
        for block in cases {
            let restored = dct4x4_inverse(&dct4x4_forward(&block));
            assert_eq!(block.to_vec(), restored);
        }
    }

    #[test]
    fn test_dct4_energy_compaction() {
        // 平滑渐变块经变换后，低频系数绝对值应显著大于高频均值
        let smooth: Vec<i32> = (0..16).map(|i| 40 + i * 3).collect();
        let coeffs = dct4x4_forward(&smooth);

        let dc_abs = coeffs[0].unsigned_abs();
        let high_freq_mean: u64 = (1..16)
            .map(|i| coeffs[i].unsigned_abs() as u64)
            .sum::<u64>()
            / 15;

        assert!(
            dc_abs as u64 > high_freq_mean * 10,
            "DC({}) 应远大于高频均值({})",
            dc_abs,
            high_freq_mean
        );
    }
}
