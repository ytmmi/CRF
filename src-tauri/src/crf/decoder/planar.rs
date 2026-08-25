//! 三平面打包解码（frame_type=3）：子帧解码 + CfL 还原 + 色度上采样

use crate::crf::error::{CrfError, CrfResult};
use crate::crf::format::{ColorFormat, CrfHeader};

use super::decode_frame;

/// 双线性 2× 上采样（色度半分辨率还原）
///
/// 每个输出像素由输入四邻加权：奇数坐标落在采样点之间时线性插值，
/// 边缘 clamp。与编码端的 2×2 均值下采样配对构成标准 420 变换链。
fn upsample_2x_bilinear(small: &[i32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<i32> {
    let mut out = vec![0i32; dw * dh];
    // 输出像素中心映射回输入坐标：(x + 0.5) / 2 - 0.5
    for y in 0..dh {
        let sy = (y as f64 + 0.5) / 2.0 - 0.5;
        let y0 = sy.floor().max(0.0) as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f64;
        for x in 0..dw {
            let sx = (x as f64 + 0.5) / 2.0 - 0.5;
            let x0 = sx.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f64;

            let v00 = small[y0 * sw + x0] as f64;
            let v10 = small[y0 * sw + x1] as f64;
            let v01 = small[y1 * sw + x0] as f64;
            let v11 = small[y1 * sw + x1] as f64;
            let top = v00 * (1.0 - fx) + v10 * fx;
            let bot = v01 * (1.0 - fx) + v11 * fx;
            out[y * dw + x] = (top * (1.0 - fy) + bot * fy).round() as i32;
        }
    }
    out
}

/// 解码三平面打包帧（frame_type=3）
///
/// 载荷布局与 encode_planar_payload 对应：
/// [ss_cfl u16 LE]   低字节=[cfl_flags]（αc/αg）；高字节 bit0=色度半分辨率标志
/// [len1 u32 LE][sub_frame1(Gray, 全分辨率)]
/// [len2 u32 LE][sub_frame2(Gray, 视标志而定)]
/// [len3 u32 LE][sub_frame3(Gray, 同上)]
///
/// CfL 还原：Co/Cg 平面加回 `⌊α·(Y−128)/16⌋`（Y 已先解码重建，因果安全）；
/// 半分辨率时 Co/Cg 先双线性上采样到全尺寸再做 CfL 与交错。
pub(crate) fn decode_planar(data: &[u8], header: &CrfHeader) -> CrfResult<Vec<i32>> {
    let full_w = header.width as usize;
    let full_h = header.height as usize;
    let pixel_count = full_w * full_h;
    if data.len() < 2 {
        return Err(CrfError::InsufficientData {
            expected: 2,
            actual: data.len(),
        });
    }
    // 字节序：低字节 cfl_flags，高字节 ss_flags（bit0=半分辨率）
    let ss_flags = data[1];
    let half_res = ss_flags & 0x01 != 0;

    let cfl_byte = data[0];
    let alpha_c = ((cfl_byte >> 4) as i32) - 8;
    let alpha_g = ((cfl_byte & 0x0F) as i32) - 8;

    let cw = full_w.div_ceil(2);
    let ch = full_h.div_ceil(2);
    let _chroma_pixels = if half_res { cw * ch } else { pixel_count };

    let mut planes: Vec<Vec<i32>> = Vec::with_capacity(3);
    let mut offset = 2;
    eprintln!(
        "DBG planar: half={} cw={} ch={} data_len={}",
        half_res,
        cw,
        ch,
        data.len()
    );

    for plane_idx in 0..3 {
        if offset + 4 > data.len() {
            return Err(CrfError::InvalidCodingParams(format!(
                "planar: sub{} len-hdr OOB (off={} total={})",
                plane_idx,
                offset,
                data.len()
            )));
        }
        let sub_len = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        eprintln!(
            "DBG sub{}: raw_len={} bytes=[{:02x} {:02x} {:02x} {:02x}]",
            plane_idx,
            sub_len,
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3]
        );
        offset += 4;

        if offset + sub_len > data.len() {
            return Err(CrfError::InvalidCodingParams(format!(
                "planar: sub{} payload OOB (len={} off={} total={})",
                plane_idx,
                sub_len,
                offset,
                data.len()
            )));
        }

        // 构造单分量虚拟文件头，让子帧复用整条解码管线
        // Y 平面全分辨率；Co/Cg 在半分辨率模式下为 (cw)×(ch)
        let mut sub_header = header.clone();
        sub_header.color_format = ColorFormat::Gray;
        if half_res && plane_idx > 0 {
            sub_header.width = cw as u16;
            sub_header.height = ch as u16;
        }
        let sub_frame = decode_frame(&data[offset..offset + sub_len], &sub_header)?;
        planes.push(sub_frame.pixels);
        eprintln!("DBG plane {}: {} px", plane_idx, planes[plane_idx].len());
        offset += sub_len;
    }

    if planes.len() < 3 || planes.iter().any(|p| p.is_empty()) {
        return Err(CrfError::InvalidCodingParams(
            "planar payload plane size mismatch".to_string(),
        ));
    }

    // 半分辨率：Co/Cg 双线性上采样到全尺寸
    if half_res {
        for plane_idx in [1usize, 2] {
            let small = std::mem::take(&mut planes[plane_idx]);
            planes[plane_idx] = upsample_2x_bilinear(&small, cw, ch, full_w, full_h);
        }
    }

    if planes.iter().any(|p| p.len() != pixel_count) {
        return Err(CrfError::InvalidCodingParams(
            "planar payload plane size mismatch".to_string(),
        ));
    }

    // CfL 还原：色度平面加回亮度线性预测（Y 已重建，因果安全）
    for (plane_idx, alpha) in [(1usize, alpha_c), (2, alpha_g)] {
        if alpha != 0 {
            #[allow(clippy::needless_range_loop)] // 双平面按下标同步遍历，range 写法最清晰
            for i in 0..pixel_count {
                let pred = (alpha * (planes[0][i] - 128)) >> 4;
                planes[plane_idx][i] += pred;
            }
        }
    }

    // 交错还原：[Y,Co,Cg] 逐像素拼接
    let mut out = vec![0i32; pixel_count * 3];
    for i in 0..pixel_count {
        out[i * 3] = planes[0][i];
        out[i * 3 + 1] = planes[1][i];
        out[i * 3 + 2] = planes[2][i];
    }
    Ok(out)
}
