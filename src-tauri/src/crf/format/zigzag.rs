/// Zigzag 扫描顺序（4x4 块）
pub const ZIGZAG_4X4: [(usize, usize); 16] = [
    (0, 0),
    (0, 1),
    (1, 0),
    (2, 0),
    (1, 1),
    (0, 2),
    (0, 3),
    (1, 2),
    (2, 1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (2, 3),
    (3, 2),
    (3, 3),
];

/// Zigzag 扫描顺序（8x8 块）
pub const ZIGZAG_8X8: [(usize, usize); 64] = [
    (0, 0),
    (0, 1),
    (1, 0),
    (2, 0),
    (1, 1),
    (0, 2),
    (0, 3),
    (1, 2),
    (2, 1),
    (3, 0),
    (4, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 4),
    (0, 5),
    (1, 4),
    (2, 3),
    (3, 2),
    (4, 1),
    (5, 0),
    (6, 0),
    (5, 1),
    (4, 2),
    (3, 3),
    (2, 4),
    (1, 5),
    (0, 6),
    (0, 7),
    (1, 6),
    (2, 5),
    (3, 4),
    (4, 3),
    (5, 2),
    (6, 1),
    (7, 0),
    (7, 1),
    (6, 2),
    (5, 3),
    (4, 4),
    (3, 5),
    (2, 6),
    (1, 7),
    (2, 7),
    (3, 6),
    (4, 5),
    (5, 4),
    (6, 3),
    (7, 2),
    (7, 3),
    (6, 4),
    (5, 5),
    (4, 6),
    (3, 7),
    (4, 7),
    (5, 6),
    (6, 5),
    (7, 4),
    (7, 5),
    (6, 6),
    (5, 7),
    (6, 7),
    (7, 6),
    (7, 7),
];

/// Zigzag 扫描：将二维块转换为一维数组
pub fn zigzag_scan(block: &[i32], block_size: usize) -> Vec<i32> {
    let mut result = Vec::with_capacity(block_size * block_size);
    match block_size {
        4 => {
            for &(y, x) in ZIGZAG_4X4.iter() {
                result.push(block[y * block_size + x]);
            }
        }
        _ => {
            for &(y, x) in ZIGZAG_8X8.iter().take(block_size * block_size) {
                if y < block_size && x < block_size {
                    result.push(block[y * block_size + x]);
                }
            }
        }
    }
    result
}

/// Zigzag 逆扫描：将一维数组恢复为二维块
pub fn zigzag_inverse(data: &[i32], block_size: usize) -> Vec<i32> {
    let mut block = vec![0i32; block_size * block_size];
    match block_size {
        4 => {
            for (i, &(y, x)) in ZIGZAG_4X4.iter().enumerate() {
                if i < data.len() {
                    block[y * block_size + x] = data[i];
                }
            }
        }
        _ => {
            for (i, &(y, x)) in ZIGZAG_8X8.iter().enumerate().take(block_size * block_size) {
                if y < block_size && x < block_size && i < data.len() {
                    block[y * block_size + x] = data[i];
                }
            }
        }
    }
    block
}

/// Zigzag 映射：有符号整数 -> 无符号整数
pub fn zigzag_encode(value: i32) -> u32 {
    if value >= 0 {
        (value as u32) << 1
    } else {
        ((-value) as u32) << 1 | 1
    }
}

/// Zigzag 逆映射：无符号整数 -> 有符号整数
pub fn zigzag_decode(value: u32) -> i32 {
    if value & 1 == 0 {
        (value >> 1) as i32
    } else {
        -((value >> 1) as i32)
    }
}
