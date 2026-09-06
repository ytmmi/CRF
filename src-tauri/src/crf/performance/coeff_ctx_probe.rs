//! CoeffCABAC 上下文深化能力探针（P3 前置验证）
//!
//! 在**程序生成的合成内容**上验证两个深化方向的系数编码字节收益，
//! 零外部数据集、零 decoder 改动：
//!
//! 1. **方向扫描**（P3.2）：按预测模式选择扫描表（zigzag / freq_x 主序 /
//!    freq_y 主序），替代固定 zigzag——方向性预测残差经 DCT 后能量沿特定
//!    频率轴集中，正交方向扫描可缩短 run、降低 run-level 开销；
//! 2. **邻块上下文**（P3.5）：`ctx_nonzero` 从单槽位扩展为按
//!    `(左块全零, 上块全零)` 索引的 4 槽位——平坦/纹理块聚集区域各自维护
//!    独立概率，预测更准（H.264/HEVC coded_block_flag 同款）。
//!
//! 探针不写正式码流，只返回各变体 `payload_bytes` 做内存级对比
//! （同 [`crate::crf::encoder::intra_probe`] 先例）。收益确认后再走正式
//! 格式化流程（文档 → encoder/decoder 对称 → flags 标记 → 载荷版本升级）。
//!
//! 判定参考（仅能力信号，不替代真实二次元差分组 ≥3% 门槛）：
//! - 变体收益 ≥10% ⟹ 方向有明确潜力，值得等真实数据 / 起草格式化方案；
//! - 3% ≤ 收益 <10% ⟹ 边际，优先级降级；
//! - 收益 <3% 或为负 ⟹ 低成本证伪，深化方向关闭。

use crate::crf::backend::ops::quantize_levels_biased;
use crate::crf::core::entropy::cabac::INIT_PROB;
use crate::crf::core::entropy::scan::ZIGZAG_8X8;
use crate::crf::core::transform::{dct8x8_forward_into, dct8x8_inverse_into};
use crate::crf::encoder::coeff_cabac::CoeffCABAC;
use crate::crf::encoder::rle_cabac::RangeEncoder;

const BLK: usize = 8;
const MODE_DC: i32 = 0;
const MODE_H: i32 = 1;
const MODE_V: i32 = 2;
const MODE_MED: i32 = 3;
const N_MODES: usize = 4;

// ===== 方向扫描（8×8） =====

/// 扫描主序。两种正交主序覆盖「能量沿 freq_x / freq_y 集中」两类方向性
/// 纹理；zigzag 为现行固定扫描（基线）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScanMajor {
    /// 固定 zigzag（对角线蛇形，低频→高频）
    Zigzag,
    /// freq_x 主序：先固定 freq_x 扫尽所有 freq_y（列优先）
    XMajor,
    /// freq_y 主序：先固定 freq_y 扫尽所有 freq_x（行优先）
    YMajor,
}

/// 按扫描主序重排行优先 64 元素块，返回一维扫描序系数。
fn scan_block(freq: &[i32; 64], major: ScanMajor) -> [i32; 64] {
    let mut out = [0i32; 64];
    match major {
        ScanMajor::Zigzag => {
            for (i, &(y, x)) in ZIGZAG_8X8.iter().enumerate() {
                out[i] = freq[y * BLK + x];
            }
        }
        ScanMajor::XMajor => {
            let mut i = 0;
            for x in 0..BLK {
                for y in 0..BLK {
                    out[i] = freq[y * BLK + x];
                    i += 1;
                }
            }
        }
        ScanMajor::YMajor => {
            let mut i = 0;
            for y in 0..BLK {
                for x in 0..BLK {
                    out[i] = freq[y * BLK + x];
                    i += 1;
                }
            }
        }
    }
    out
}

/// 变体 A 的模式→扫描映射：H→XMajor、V→YMajor、DC/MED→Zigzag。
fn mode_scan_a() -> [ScanMajor; N_MODES] {
    [
        ScanMajor::Zigzag, // DC
        ScanMajor::XMajor, // H
        ScanMajor::YMajor, // V
        ScanMajor::Zigzag, // MED
    ]
}

/// 变体 B 的模式→扫描映射：H/V 与变体 A 反向（能量分布可能相反）。
fn mode_scan_b() -> [ScanMajor; N_MODES] {
    [
        ScanMajor::Zigzag, // DC
        ScanMajor::YMajor, // H
        ScanMajor::XMajor, // V
        ScanMajor::Zigzag, // MED
    ]
}

// ===== 邻块上下文 CoeffCABAC 变体（实验，不写码流） =====

/// ctx_nonzero 多槽位变体：按 (left_zero, top_zero) 索引 4 个概率槽位，
/// 其余上下文（ctx_run / ctx_level_q / sign 直通）与正式 CoeffCABAC 一致。
struct NeighborCtxCoeffCABAC {
    rc: RangeEncoder,
    ctx_nonzero: [u16; 4],
    ctx_run: u16,
    ctx_level_q: u16,
}

impl NeighborCtxCoeffCABAC {
    fn new() -> Self {
        NeighborCtxCoeffCABAC {
            rc: RangeEncoder::new(),
            ctx_nonzero: [INIT_PROB; 4],
            ctx_run: INIT_PROB,
            ctx_level_q: INIT_PROB,
        }
    }

    /// 编码一个 8×8 块的一维扫描系数；逻辑与正式 CoeffCABAC::encode_block
    /// 逐式一致，仅 ctx_nonzero 由单槽位改为按邻块状态索引的多槽位。
    fn encode_block(&mut self, zigzag: &[i32], left_zero: bool, top_zero: bool) {
        debug_assert!(zigzag.len() == 64);
        let idx = (left_zero as usize) << 1 | (top_zero as usize);
        let mut last_nz = None;
        for (i, &v) in zigzag.iter().enumerate().rev() {
            if v != 0 {
                last_nz = Some(i);
                break;
            }
        }
        match last_nz {
            None => {
                self.rc.encode_bit(false, &mut self.ctx_nonzero[idx]);
            }
            Some(last) => {
                self.rc.encode_bit(true, &mut self.ctx_nonzero[idx]);
                let mut prev_pos = 64;
                for i in (0..=last).rev() {
                    let v = zigzag[i];
                    if v != 0 {
                        let run = (prev_pos - i - 1) as u32;
                        let r = run.min(63);
                        for _ in 0..r {
                            self.rc.encode_bit(true, &mut self.ctx_run);
                        }
                        if r < 63 {
                            self.rc.encode_bit(false, &mut self.ctx_run);
                        }
                        let av = v.unsigned_abs();
                        for _ in 0..av {
                            self.rc.encode_bit(true, &mut self.ctx_level_q);
                        }
                        self.rc.encode_bit(false, &mut self.ctx_level_q);
                        self.rc.encode_direct(v < 0);
                        prev_pos = i;
                    }
                }
                if prev_pos > 0 {
                    let r = prev_pos.min(63);
                    for _ in 0..r {
                        self.rc.encode_bit(true, &mut self.ctx_run);
                    }
                    if r < 63 {
                        self.rc.encode_bit(false, &mut self.ctx_run);
                    }
                }
            }
        }
    }

    fn finish(self) -> Vec<u8> {
        self.rc.finish()
    }
}

// ===== 预测（与 intra_transform/intra_probe 同式，独立维护避免耦合） =====

#[inline]
fn recon_at(recon: &[i32], w: usize, h: usize, x: isize, y: isize) -> i32 {
    if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
        return 0;
    }
    recon[y as usize * w + x as usize]
}

fn predict_block(
    recon: &[i32],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    mode: i32,
) -> [[i32; BLK]; BLK] {
    let mut out = [[0i32; BLK]; BLK];
    match mode {
        MODE_H => {
            for (by, row) in out.iter_mut().enumerate() {
                let left = recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize);
                for v in row.iter_mut() {
                    *v = left;
                }
            }
        }
        MODE_V => {
            let mut tops = [0i32; BLK];
            for (bx, t) in tops.iter_mut().enumerate() {
                *t = recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1);
            }
            for row in out.iter_mut() {
                for (v, t) in row.iter_mut().zip(tops.iter()) {
                    *v = *t;
                }
            }
        }
        MODE_DC => {
            let mut sum = 0i64;
            let mut cnt = 0i64;
            if x0 > 0 {
                for by in 0..BLK {
                    sum += recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize) as i64;
                    cnt += 1;
                }
            }
            if y0 > 0 {
                for bx in 0..BLK {
                    sum += recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1) as i64;
                    cnt += 1;
                }
            }
            let dc = if cnt > 0 { (sum / cnt) as i32 } else { 0 };
            for row in &mut out {
                for v in row.iter_mut() {
                    *v = dc;
                }
            }
        }
        _ => {
            let c = recon_at(recon, w, h, x0 as isize - 1, y0 as isize - 1);
            for (by, row) in out.iter_mut().enumerate() {
                let a = recon_at(recon, w, h, x0 as isize - 1, (y0 + by) as isize);
                for (bx, v) in row.iter_mut().enumerate() {
                    let b = recon_at(recon, w, h, (x0 + bx) as isize, y0 as isize - 1);
                    if x0 == 0 && y0 == 0 {
                        *v = 0;
                    } else if x0 == 0 {
                        *v = b;
                    } else if y0 == 0 {
                        *v = a;
                    } else {
                        let p = a + b - c;
                        let lo = a.min(b).min(c);
                        let hi = a.max(b).max(c);
                        *v = p.clamp(lo, hi);
                    }
                }
            }
        }
    }
    out
}

// ===== 合成内容生成器（确定性，零外部数据集） =====

fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn gen_h_stripe(w: usize, h: usize, period: usize) -> Vec<i32> {
    let mut p = vec![0i32; w * h];
    for y in 0..h {
        let v = if (y / period) & 1 == 0 { 64 } else { -64 };
        for x in 0..w {
            p[y * w + x] = v;
        }
    }
    p
}

fn gen_v_stripe(w: usize, h: usize, period: usize) -> Vec<i32> {
    let mut p = vec![0i32; w * h];
    for y in 0..h {
        for x in 0..w {
            p[y * w + x] = if (x / period) & 1 == 0 { 64 } else { -64 };
        }
    }
    p
}

fn gen_diag_stripe(w: usize, h: usize, period: usize) -> Vec<i32> {
    let mut p = vec![0i32; w * h];
    for y in 0..h {
        for x in 0..w {
            p[y * w + x] = if ((x + y) / period) & 1 == 0 { 64 } else { -64 };
        }
    }
    p
}

fn gen_noise(w: usize, h: usize, amp: i32, seed: u32) -> Vec<i32> {
    let mut p = vec![0i32; w * h];
    let mut st = seed.max(1);
    for v in p.iter_mut() {
        *v = (xorshift32(&mut st) % (2 * amp as u32 + 1)) as i32 - amp;
    }
    p
}

fn gen_checker(w: usize, h: usize, amp: i32, seed: u32) -> Vec<i32> {
    // 块级棋盘：偶数块平坦常数（近零残差），奇数块噪声（EOB 高）
    let mut p = vec![0i32; w * h];
    let mut st = seed.max(1);
    let flat = 20;
    for by in 0..h.div_ceil(BLK) {
        for bx in 0..w.div_ceil(BLK) {
            let is_noise = (bx + by) % 2 == 1;
            for y in 0..BLK {
                for x in 0..BLK {
                    let px = bx * BLK + x;
                    let py = by * BLK + y;
                    if px >= w || py >= h {
                        continue;
                    }
                    p[py * w + px] = if is_noise {
                        (xorshift32(&mut st) % (2 * amp as u32 + 1)) as i32 - amp
                    } else {
                        flat
                    };
                }
            }
        }
    }
    p
}

fn gen_gradient_patches(w: usize, h: usize, seed: u32) -> Vec<i32> {
    // 平滑渐变底 + 若干局部噪声斑块（混合：部分平坦、部分纹理）
    let mut p = vec![0i32; w * h];
    let mut st = seed.max(1);
    for y in 0..h {
        for x in 0..w {
            p[y * w + x] = ((x * 2 + y) as i32) % 160 - 80;
        }
    }
    // 4 个 32×32 噪声斑块
    for _ in 0..4 {
        let px = (xorshift32(&mut st) as usize) % (w - 32).max(1);
        let py = (xorshift32(&mut st) as usize) % (h - 32).max(1);
        for y in 0..32 {
            for x in 0..32 {
                p[(py + y) * w + (px + x)] = (xorshift32(&mut st) % 129) as i32 - 64;
            }
        }
    }
    p
}

struct SynthCase {
    name: &'static str,
    plane: Vec<i32>,
}

fn synth_cases(w: usize, h: usize) -> Vec<SynthCase> {
    vec![
        SynthCase {
            name: "水平条纹 period=4",
            plane: gen_h_stripe(w, h, 4),
        },
        SynthCase {
            name: "垂直条纹 period=4",
            plane: gen_v_stripe(w, h, 4),
        },
        SynthCase {
            name: "斜向条纹 period=4",
            plane: gen_diag_stripe(w, h, 4),
        },
        SynthCase {
            name: "随机噪声 amp=96",
            plane: gen_noise(w, h, 96, 0x9e37_79b9),
        },
        SynthCase {
            name: "棋盘混合（平坦+噪声 amp=64）",
            plane: gen_checker(w, h, 64, 0x1234_5678),
        },
        SynthCase {
            name: "渐变+局部纹理斑块",
            plane: gen_gradient_patches(w, h, 0xdead_beef),
        },
    ]
}

// ===== 预测 → 变换 → 量化（复用 frame_type=8 闭环语义） =====

struct EncodedBlock {
    mode: i32,
    /// 量化后系数（行优先）：DC 模式为空间域残差，其余为 DCT 域。
    qcoeffs: [i32; 64],
}

fn prepare_blocks(
    plane: &[i32],
    width: usize,
    height: usize,
    q_step: u8,
    deadzone: i8,
) -> Vec<EncodedBlock> {
    let q = (q_step.max(1)) as i32;
    let mut recon = vec![0i32; plane.len()];
    let mut blocks = Vec::with_capacity((width.div_ceil(BLK)) * (height.div_ceil(BLK)));
    let mut block = [0i32; 64];

    for y0 in (0..height).step_by(BLK) {
        for x0 in (0..width).step_by(BLK) {
            let bw = BLK.min(width - x0);
            let bh = BLK.min(height - y0);

            let mut best_mode = MODE_DC;
            let mut best_sad = u64::MAX;
            let mut best_pred = predict_block(&recon, width, height, x0, y0, MODE_DC);
            for mode in [MODE_DC, MODE_H, MODE_V, MODE_MED] {
                let pred = predict_block(&recon, width, height, x0, y0, mode);
                let mut sad = 0u64;
                for by in 0..bh {
                    for bx in 0..bw {
                        let d = plane[(y0 + by) * width + x0 + bx] - pred[by][bx];
                        sad += d.unsigned_abs() as u64;
                    }
                }
                if sad < best_sad {
                    best_sad = sad;
                    best_mode = mode;
                    best_pred = pred;
                }
            }

            block.fill(0);
            for by in 0..bh {
                for bx in 0..bw {
                    block[by * BLK + bx] = plane[(y0 + by) * width + x0 + bx] - best_pred[by][bx];
                }
            }

            let qcoeffs = if best_mode == MODE_DC {
                let mut qres = [0i32; 64];
                let mut dq = [0i32; 64];
                quantize_levels_biased(&block, &mut qres, q_step, deadzone);
                for (dequantized, &level) in dq.iter_mut().zip(qres.iter()) {
                    *dequantized = level * q;
                }
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] = dq[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                qres
            } else {
                let mut freq = [0i32; 64];
                dct8x8_forward_into(&block, &mut freq);
                let mut qc = [0i32; 64];
                let mut dq = [0i32; 64];
                quantize_levels_biased(&freq, &mut qc, q_step, deadzone);
                for (dequantized, &level) in dq.iter_mut().zip(qc.iter()) {
                    *dequantized = level * q;
                }
                let mut spatial = [0i32; 64];
                dct8x8_inverse_into(&dq, &mut spatial);
                for by in 0..bh {
                    for bx in 0..bw {
                        recon[(y0 + by) * width + x0 + bx] =
                            spatial[by * BLK + bx] + best_pred[by][bx];
                    }
                }
                qc
            };

            blocks.push(EncodedBlock {
                mode: best_mode,
                qcoeffs,
            });
        }
    }
    blocks
}

// ===== 系数编码（单槽位 vs 邻块多槽位，按模式扫描映射） =====

fn encode_blocks(
    blocks: &[EncodedBlock],
    width: usize,
    height: usize,
    mode_scan: &[ScanMajor; N_MODES],
    neighbor_ctx: bool,
) -> Vec<u8> {
    let cols = width.div_ceil(BLK).max(1);
    if neighbor_ctx {
        let mut enc = NeighborCtxCoeffCABAC::new();
        let mut top_zero = vec![true; cols];
        let mut cur_zero = vec![true; cols];
        let mut bi = 0usize;
        for _y0 in (0..height).step_by(BLK) {
            cur_zero.fill(true);
            let mut prev_zero = true; // 每行首块左邻视为边界外全零
            for (cx, _x0) in (0..width).step_by(BLK).enumerate() {
                let b = &blocks[bi];
                let scanned = scan_block(&b.qcoeffs, mode_scan[b.mode as usize]);
                let is_zero = scanned.iter().all(|&v| v == 0);
                enc.encode_block(&scanned, prev_zero, top_zero[cx]);
                cur_zero[cx] = is_zero;
                prev_zero = is_zero;
                bi += 1;
            }
            top_zero.copy_from_slice(&cur_zero);
        }
        debug_assert_eq!(bi, blocks.len());
        enc.finish()
    } else {
        let mut enc = CoeffCABAC::new();
        for b in blocks {
            let scanned = scan_block(&b.qcoeffs, mode_scan[b.mode as usize]);
            enc.encode_block(&scanned);
        }
        enc.finish()
    }
}

// ===== 探针入口 =====

/// 运行探针：打印各合成内容下 5 个变体的系数编码字节对照。
pub fn run() -> Result<(), String> {
    const W: usize = 256;
    const H: usize = 256;
    // 双档量化：近无损（step=1）与有损（step=4）
    const Q_STEPS: [(u8, i8); 2] = [(1, 0), (4, 0)];

    println!("=== CoeffCABAC 上下文深化能力探针（合成内容 {W}x{H}）===\n");

    for (q_step, deadzone) in Q_STEPS {
        println!("--- q_step={q_step} deadzone={deadzone} ---");
        for case in synth_cases(W, H) {
            let blocks = prepare_blocks(&case.plane, W, H, q_step, deadzone);

            let all_zigzag = [ScanMajor::Zigzag; N_MODES];
            let baseline = encode_blocks(&blocks, W, H, &all_zigzag, false);
            let v1a = encode_blocks(&blocks, W, H, &mode_scan_a(), false);
            let v1b = encode_blocks(&blocks, W, H, &mode_scan_b(), false);
            let v2 = encode_blocks(&blocks, W, H, &all_zigzag, true);
            let v3 = encode_blocks(&blocks, W, H, &mode_scan_a(), true);

            let pct = |v: usize, b: usize| -> f64 { (v as f64 - b as f64) / b as f64 * 100.0 };
            println!(
                "{}: B={} | V1a(方向A)={}({:+.1}%) V1b(方向B)={}({:+.1}%) | \
                 V2(邻块)={}({:+.1}%) V3(组合)={}({:+.1}%)",
                case.name,
                baseline.len(),
                v1a.len(),
                pct(v1a.len(), baseline.len()),
                v1b.len(),
                pct(v1b.len(), baseline.len()),
                v2.len(),
                pct(v2.len(), baseline.len()),
                v3.len(),
                pct(v3.len(), baseline.len()),
            );
        }
        println!();
    }

    println!("=== 判定参考 ===");
    println!("  方向扫描收益 = max(V1a,V1b) vs B");
    println!("  邻块上下文收益 = V2 vs B");
    println!("  组合收益 = V3 vs B");
    println!("  ≥10% ⟹ 有明确潜力；3%~10% ⟹ 边际；<3% 或负 ⟹ 证伪（合成能力信号，不替代真实组 ≥3% 门槛）");
    Ok(())
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_generators_are_deterministic() {
        let a = gen_noise(64, 64, 32, 0x1234);
        let b = gen_noise(64, 64, 32, 0x1234);
        assert_eq!(a, b);
        let c = gen_checker(64, 64, 64, 0x1234_5678);
        let d = gen_checker(64, 64, 64, 0x1234_5678);
        assert_eq!(c, d);
    }

    #[test]
    fn scan_block_preserves_coeff_multiset() {
        // 三种扫描仅重排系数，不改变多重集
        let mut st = 0xdead_beef;
        let mut freq = [0i32; 64];
        for v in freq.iter_mut() {
            *v = (xorshift32(&mut st) % 257) as i32 - 128;
        }
        let z = scan_block(&freq, ScanMajor::Zigzag);
        let xm = scan_block(&freq, ScanMajor::XMajor);
        let ym = scan_block(&freq, ScanMajor::YMajor);
        for arr in [&z, &xm, &ym] {
            let mut s = arr.to_vec();
            s.sort_unstable();
            let mut ref_s = freq.to_vec();
            ref_s.sort_unstable();
            assert_eq!(s, ref_s);
        }
    }

    #[test]
    fn neighbor_ctx_fixed_index_equals_single_slot() {
        // 邻块上下文恒走同一槽位时，必须与单槽位 CoeffCABAC 逐字节一致
        // ——锁定 NeighborCtxCoeffCABAC 复制逻辑与正式编码器等价。
        let mut st = 0xabcd;
        let blocks: Vec<[i32; 64]> = (0..64)
            .map(|_| {
                let mut arr = [0i32; 64];
                for v in arr.iter_mut() {
                    *v = if xorshift32(&mut st) % 3 == 0 {
                        (xorshift32(&mut st) % 9) as i32 - 4
                    } else {
                        0
                    };
                }
                arr
            })
            .collect();

        let mut single = CoeffCABAC::new();
        for b in &blocks {
            single.encode_block(b);
        }
        let single_bytes = single.finish();

        let mut multi = NeighborCtxCoeffCABAC::new();
        for b in &blocks {
            multi.encode_block(b, true, true); // 恒 idx=0
        }
        let multi_bytes = multi.finish();

        assert_eq!(single_bytes, multi_bytes);
    }

    #[test]
    fn probe_end_to_end_runs() {
        let plane = gen_diag_stripe(64, 64, 4);
        let blocks = prepare_blocks(&plane, 64, 64, 4, 0);
        assert!(!blocks.is_empty());
        let all_zigzag = [ScanMajor::Zigzag; N_MODES];
        let baseline = encode_blocks(&blocks, 64, 64, &all_zigzag, false);
        let v3 = encode_blocks(&blocks, 64, 64, &mode_scan_a(), true);
        assert!(baseline.len() > 0);
        assert!(v3.len() > 0);
    }
}
