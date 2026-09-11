use super::*;

impl LossyOptionsV2 {
    /// 严格读取专家 JSON。未知字段、错误类型和非定点单位均返回路径明确的错误。
    pub fn from_json_str(input: &str) -> Result<Self, ConfigError> {
        let mut raw: serde_json::Value = serde_json::from_str(input).map_err(|e| ConfigError {
            field: "lossy",
            message: e.to_string(),
        })?;
        if raw.get("lossy").is_some() {
            let object = raw.as_object_mut().ok_or_else(|| ConfigError {
                field: "lossy",
                message: "must be a JSON object".into(),
            })?;
            if object.len() != 1 {
                return Err(ConfigError {
                    field: "lossy",
                    message: "the wrapped form may only contain the lossy field".into(),
                });
            }
            raw = object.remove("lossy").expect("lossy key was checked");
        }
        normalize_human_json(&mut raw)?;
        let value: Self = serde_json::from_value(raw).map_err(|e| ConfigError {
            field: "lossy",
            message: e.to_string(),
        })?;
        value.validate()?;
        Ok(value)
    }

    #[allow(dead_code)] // V2 JSON 预留序列化 API，待 CLI/前端接线
    pub fn to_json_pretty(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(self).map_err(|e| ConfigError {
            field: "lossy",
            message: e.to_string(),
        })
    }
}

fn normalize_human_json(root: &mut serde_json::Value) -> Result<(), ConfigError> {
    let Some(root) = root.as_object_mut() else {
        return Err(ConfigError {
            field: "lossy",
            message: "must be a JSON object".into(),
        });
    };
    if let Some(x) = root.get_mut("base").and_then(|x| x.as_object_mut()) {
        scaled(x, "quality", "qualityX100", 100, 2)?;
    }
    if let Some(x) = root.get_mut("rate").and_then(|x| x.as_object_mut()) {
        scaled(x, "minQuality", "minQualityX100", 100, 2)?;
    }
    if let Some(x) = root.get_mut("firstFrame").and_then(|x| x.as_object_mut()) {
        scaled(x, "quality", "qualityX100", 100, 2)?;
        scaled(x, "qualityOffset", "qualityOffsetX100", 100, 2)?;
    }
    if let Some(x) = root.get_mut("quant").and_then(|x| x.as_object_mut()) {
        scaled(x, "lumaStep", "lumaStepQ8", 256, 3)?;
        scaled(x, "chromaStep", "chromaStepQ8", 256, 3)?;
    }
    if let Some(x) = root.get_mut("chroma").and_then(|x| x.as_object_mut()) {
        scaled(x, "edgeProtection", "edgeProtectionX100", 100, 2)?;
    }
    if let Some(x) = root.get_mut("perceptual").and_then(|x| x.as_object_mut()) {
        for (human, fixed) in [
            ("activityMasking", "activityMaskingX100"),
            ("flatAreaProtection", "flatAreaProtectionX100"),
            ("edgeProtection", "edgeProtectionX100"),
            ("ringingControl", "ringingControlX100"),
        ] {
            scaled(x, human, fixed, 100, 2)?;
        }
    }
    Ok(())
}

fn scaled(
    map: &mut serde_json::Map<String, serde_json::Value>,
    human: &'static str,
    fixed: &'static str,
    multiplier: i64,
    max_digits: usize,
) -> Result<(), ConfigError> {
    let Some(value) = map.remove(human) else {
        return Ok(());
    };
    if map.contains_key(fixed) {
        return Err(ConfigError {
            field: fixed,
            message: format!("cannot combine {human} with {fixed}"),
        });
    }
    let text = match value {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s,
        _ => {
            return Err(ConfigError {
                field: human,
                message: "must be a decimal number".into(),
            })
        }
    };
    let fixed_value =
        decimal_to_fixed(&text, multiplier, max_digits).map_err(|message| ConfigError {
            field: human,
            message,
        })?;
    map.insert(fixed.into(), serde_json::Value::Number(fixed_value.into()));
    Ok(())
}

fn decimal_to_fixed(input: &str, multiplier: i64, max_digits: usize) -> Result<i64, String> {
    let negative = input.starts_with('-');
    let raw = input.trim_start_matches(['-', '+']);
    let (whole, frac) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || frac.len() > max_digits
        || !whole.bytes().all(|x| x.is_ascii_digit())
        || !frac.bytes().all(|x| x.is_ascii_digit())
    {
        return Err(format!("accepts at most {max_digits} decimal places"));
    }
    let denominator = 10i64.pow(frac.len() as u32);
    let numerator = whole
        .parse::<i64>()
        .map_err(|_| "decimal overflow".to_string())?
        .checked_mul(denominator)
        .and_then(|x| {
            x.checked_add(if frac.is_empty() {
                0
            } else {
                frac.parse().ok()?
            })
        })
        .ok_or_else(|| "decimal overflow".to_string())?;
    let mut out = numerator
        .checked_mul(multiplier)
        .ok_or_else(|| "decimal overflow".to_string())?
        .checked_add(denominator / 2)
        .ok_or_else(|| "decimal overflow".to_string())?
        / denominator;
    if negative {
        out = -out;
    }
    Ok(out)
}

impl ResolvedLossyReport {
    pub fn to_json_pretty(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(self).map_err(|e| ConfigError {
            field: "resolved",
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_strict_and_roundtrips() {
        let cfg = LossyOptionsV2Builder::preset(9650)
            .effort(8)
            .chroma_sampling(ChromaSampling::Cs420)
            .build()
            .unwrap();
        let json = cfg.to_json_pretty().unwrap();
        assert_eq!(LossyOptionsV2::from_json_str(&json).unwrap(), cfg);
        assert!(LossyOptionsV2::from_json_str(
            r#"{"apiVersion":2,"base":{"type":"explicit"},"unknown":1}"#
        )
        .is_err());
        let human = r#"{"apiVersion":2,"base":{"type":"preset","quality":96.5},"firstFrame":{"mode":"quality-offset","qualityOffset":0.5},"chroma":{"sampling":"420","edgeProtection":1.2}}"#;
        let parsed = LossyOptionsV2::from_json_str(human).unwrap();
        assert!(matches!(
            parsed.base,
            LossyBase::Preset {
                quality_x100: 9650,
                ..
            }
        ));
        assert_eq!(parsed.first_frame.quality_offset_x100, Some(50));

        let wrapped = format!(r#"{{"lossy":{human}}}"#);
        assert_eq!(LossyOptionsV2::from_json_str(&wrapped).unwrap(), parsed);
        assert!(LossyOptionsV2::from_json_str(
            r#"{"lossy":{"apiVersion":2,"base":{"type":"preset","quality":96.5}},"extra":1}"#
        )
        .is_err());

        let explicit = r#"{
            "lossy": {
                "apiVersion": 2,
                "base": { "type": "explicit" },
                "rate": {
                    "mode": "constrained-quality",
                    "targetBytes": 3370055,
                    "minQuality": 96.0
                },
                "quant": {
                    "mode": "explicit-steps",
                    "lumaStep": 1.375,
                    "chromaStep": 1.75,
                    "matrix": "edge-preserving",
                    "rdoq": "full"
                },
                "firstFrame": { "mode": "quality-offset", "qualityOffset": 0.5 },
                "temporal": { "referenceMode": "hybrid", "changeMask": "auto" }
            }
        }"#;
        let parsed = LossyOptionsV2::from_json_str(explicit).unwrap();
        assert_eq!(parsed.quant.luma_step_q8, Some(352));
        assert_eq!(parsed.quant.chroma_step_q8, Some(448));
        assert_eq!(parsed.rate.min_quality_x100, Some(9600));
    }
}
