//! CLI 用户入口：常用 V2 覆盖、专家 JSON 与 resolved/schema 导出。

use std::path::PathBuf;

use crate::crf::{ChromaSampling, LossyOptionsV2, LossyOptionsV2Builder};

pub struct CliLossyConfig {
    pub lossy: Option<LossyOptionsV2>,
    pub dump_resolved: Option<PathBuf>,
    pub dump_schema: Option<PathBuf>,
}

pub fn parse(args: &[String]) -> Result<CliLossyConfig, String> {
    let mut quality = None;
    let mut target_bytes = None;
    let mut first_offset = None;
    let mut chroma = None;
    let mut perceptual = None;
    let mut effort = None;
    let mut expert = None;
    let mut dump_resolved = None;
    let mut dump_schema = None;
    let mut i = 1usize;
    while i < args.len() {
        let flag = args[i].as_str();
        let takes_value = matches!(
            flag,
            "--lossy-quality"
                | "--target-bytes"
                | "--first-frame-quality-offset"
                | "--chroma-sampling"
                | "--perceptual-strength"
                | "--effort"
                | "--expert-config"
                | "--dump-resolved-config"
                | "--dump-expert-schema"
        );
        if !takes_value {
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--lossy-quality" => quality = Some(parse_fixed(value, 2, 100, 10000, flag)? as u16),
            "--target-bytes" => {
                target_bytes = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid {flag}"))?,
                )
            }
            "--first-frame-quality-offset" => {
                first_offset = Some(parse_fixed_signed(value, 2, -2000, 2000, flag)? as i16)
            }
            "--chroma-sampling" => {
                chroma = Some(match value.as_str() {
                    "auto" => ChromaSampling::Auto,
                    "444" => ChromaSampling::Cs444,
                    "422" => ChromaSampling::Cs422,
                    "420" => ChromaSampling::Cs420,
                    _ => return Err(format!("invalid {flag}: {value}")),
                })
            }
            "--perceptual-strength" => {
                perceptual = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| format!("invalid {flag}"))?,
                )
            }
            "--effort" => {
                effort = Some(value.parse::<u8>().map_err(|_| format!("invalid {flag}"))?)
            }
            "--expert-config" => expert = Some(PathBuf::from(value)),
            "--dump-resolved-config" => dump_resolved = Some(PathBuf::from(value)),
            "--dump-expert-schema" => dump_schema = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
        i += 2;
    }
    let has_direct = quality.is_some()
        || target_bytes.is_some()
        || first_offset.is_some()
        || chroma.is_some()
        || perceptual.is_some()
        || effort.is_some();
    if expert.is_some() && has_direct {
        return Err("--expert-config cannot be combined with individual lossy flags".into());
    }
    let lossy = if let Some(path) = expert {
        let json =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Some(LossyOptionsV2::from_json_str(&json).map_err(|e| e.to_string())?)
    } else if has_direct {
        let mut b = LossyOptionsV2Builder::preset(quality.unwrap_or(9600));
        if let Some(v) = target_bytes {
            b = b.target_bytes(v);
        }
        if let Some(v) = first_offset {
            b = b.first_frame_offset(v);
        }
        if let Some(v) = chroma {
            b = b.chroma_sampling(v);
        }
        if let Some(v) = perceptual {
            b = b.perceptual_strength(v);
        }
        if let Some(v) = effort {
            b = b.effort(v);
        }
        Some(b.build().map_err(|e| e.to_string())?)
    } else {
        None
    };
    Ok(CliLossyConfig {
        lossy,
        dump_resolved,
        dump_schema,
    })
}

pub fn write_requested_exports(config: &CliLossyConfig) -> Result<(), String> {
    if let Some(path) = &config.dump_schema {
        let json = serde_json::to_string_pretty(&crate::crf::expert_panel_schema())
            .map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(path), Some(lossy)) = (&config.dump_resolved, &config.lossy) {
        let json = lossy
            .resolve_without_encoding()
            .map_err(|e| e.to_string())?
            .to_json_pretty()
            .map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))?;
    } else if config.dump_resolved.is_some() {
        return Err("--dump-resolved-config requires a lossy configuration".into());
    }
    Ok(())
}

fn parse_fixed(
    input: &str,
    scale_digits: usize,
    min: i64,
    max: i64,
    flag: &str,
) -> Result<i64, String> {
    let v = parse_fixed_signed(input, scale_digits, min, max, flag)?;
    if v < 0 {
        Err(format!("{flag} must be positive"))
    } else {
        Ok(v)
    }
}
fn parse_fixed_signed(
    input: &str,
    digits: usize,
    min: i64,
    max: i64,
    flag: &str,
) -> Result<i64, String> {
    let negative = input.starts_with('-');
    let raw = input.trim_start_matches(['-', '+']);
    let (whole, frac) = raw.split_once('.').unwrap_or((raw, ""));
    if frac.len() > digits
        || whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("{flag} accepts at most {digits} decimal places"));
    }
    let scale = 10i64.pow(digits as u32);
    let mut padded = frac.to_string();
    while padded.len() < digits {
        padded.push('0');
    }
    let mut value = whole
        .parse::<i64>()
        .map_err(|_| format!("invalid {flag}"))?
        .checked_mul(scale)
        .and_then(|x| x.checked_add(padded.parse::<i64>().unwrap_or(0)))
        .ok_or_else(|| format!("{flag} overflow"))?;
    if negative {
        value = -value;
    }
    if !(min..=max).contains(&value) {
        return Err(format!("{flag} outside supported range"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_flags_build_v2_fixed_point_config() {
        let args = [
            "crf",
            "--lossy-quality",
            "96.50",
            "--first-frame-quality-offset",
            "0.50",
            "--chroma-sampling",
            "420",
            "--effort",
            "8",
        ]
        .map(str::to_string);
        let parsed = parse(&args).unwrap().lossy.unwrap();
        assert!(matches!(
            parsed.base,
            crate::crf::LossyBase::Preset {
                quality_x100: 9650,
                ..
            }
        ));
        assert_eq!(parsed.first_frame.quality_offset_x100, Some(50));
        assert_eq!(parsed.performance.effort, 8);
    }

    #[test]
    fn rejects_excess_decimal_precision() {
        let args = ["crf", "--lossy-quality", "96.501"].map(str::to_string);
        assert!(parse(&args).is_err());
    }
}
