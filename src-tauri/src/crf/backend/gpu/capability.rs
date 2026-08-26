//! NVIDIA 能力发现。

use std::process::Command;

/// NVIDIA 设备的最小能力报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaDeviceInfo {
    pub device_id: u32,
    pub name: String,
    pub driver_version: Option<String>,
    pub memory_bytes: Option<u64>,
    pub compute_capability: Option<(u16, u16)>,
}

impl NvidiaDeviceInfo {
    pub fn supports_cuda(&self) -> bool {
        self.driver_version.is_some()
    }
}

/// 查询指定 NVIDIA 设备；设备不存在、驱动不可用或命令失败时返回 `None`。
pub fn probe_nvidia(device_id: u32) -> Option<NvidiaDeviceInfo> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,driver_version,memory.total,compute_cap",
            "--format=csv,noheader,nounits",
            "--id",
        ])
        .arg(device_id.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_nvidia_smi_line(&String::from_utf8_lossy(&output.stdout), device_id)
}

fn parse_nvidia_smi_line(output: &str, device_id: u32) -> Option<NvidiaDeviceInfo> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    let fields: Vec<_> = line.split(',').map(str::trim).collect();
    if fields.len() < 4 || fields[0].is_empty() || fields[0].eq_ignore_ascii_case("n/a") {
        return None;
    }
    Some(NvidiaDeviceInfo {
        device_id,
        name: fields[0].to_owned(),
        driver_version: parse_optional_string(fields[1]),
        memory_bytes: parse_memory_bytes(fields[2]),
        compute_capability: parse_compute_capability(fields[3]),
    })
}

fn parse_optional_string(value: &str) -> Option<String> {
    (!value.is_empty() && !value.eq_ignore_ascii_case("n/a")).then(|| value.to_owned())
}

fn parse_memory_bytes(value: &str) -> Option<u64> {
    let number = value.split_whitespace().next()?.parse::<f64>().ok()?;
    Some((number * 1024.0 * 1024.0) as u64)
}

fn parse_compute_capability(value: &str) -> Option<(u16, u16)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_smi_output() {
        let info = parse_nvidia_smi_line("RTX 4060 Laptop GPU, 610.88, 8188, 8.9\n", 0).unwrap();
        assert_eq!(info.name, "RTX 4060 Laptop GPU");
        assert_eq!(info.memory_bytes, Some(8188 * 1024 * 1024));
        assert_eq!(info.compute_capability, Some((8, 9)));
    }

    #[test]
    fn rejects_malformed_output() {
        assert!(parse_nvidia_smi_line("N/A, N/A, N/A, N/A\n", 0).is_none());
    }
}
