use schemars::{Schema, SchemaGenerator, json_schema};
use serde_json::{Map, Value, json};

pub(crate) const PASS_PRESETS: &[&str] = &["lossless", "none"];
pub(crate) const MOD_OPTIFINE: &str = "optifine";
pub(crate) const MOD_MTR3: &str = "mtr3";
pub(crate) const MODS: &[&str] = &[MOD_OPTIFINE, MOD_MTR3];
pub(crate) const ZIP_CONFORMANT: &str = "conformant";
pub(crate) const ZIP_LEAN: &str = "lean";
pub(crate) const ZIP_MODES: &[&str] = &[ZIP_CONFORMANT, ZIP_LEAN];
pub(crate) const ALLOWED_MODES: &[&str] = &[ZIP_LEAN];
pub(crate) const MAX_EFFORT: &str = "max";
pub(crate) const MAX_EFFORT_LEVEL: u64 = u8::MAX as u64;

pub(crate) fn pass_preset(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "string", "enum": PASS_PRESETS})
}

pub(crate) fn mods(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "array", "items": {"type": "string", "enum": MODS}})
}

pub(crate) fn zip_mode(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"anyOf": [{"type": "string", "enum": ZIP_MODES}, {"type": "null"}]})
}

pub(crate) fn allowed_modes(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "array", "items": {"type": "string", "enum": ALLOWED_MODES}})
}

pub(crate) fn effort(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"anyOf": [
        {"type": "integer", "minimum": 0, "maximum": MAX_EFFORT_LEVEL},
        {"type": "string", "enum": [MAX_EFFORT]}, {"type": "null"}
    ]})
}

struct Metric {
    name: &'static str,
    min: Option<f64>,
    max: Option<f64>,
}

const METRICS: &[Metric] = &[
    Metric {
        name: "ssim_min",
        min: Some(0.0),
        max: Some(1.0),
    },
    Metric {
        name: "max_channel_delta",
        min: Some(0.0),
        max: Some(255.0),
    },
    Metric {
        name: "lsd_max",
        min: Some(0.0),
        max: None,
    },
    Metric {
        name: "snr_min",
        min: None,
        max: None,
    },
];

pub(crate) fn lossy(schema: &mut Schema) {
    let properties: Map<String, Value> = METRICS
        .iter()
        .map(|metric| {
            let mut constraint = json!({"type": "number"});
            if let Some(min) = metric.min {
                constraint["minimum"] = json!(min);
            }
            if let Some(max) = metric.max {
                constraint["maximum"] = json!(max);
            }
            (metric.name.into(), constraint)
        })
        .collect();
    schema.ensure_object()["properties"].as_object_mut().expect("derived object properties")
        .insert("max_loss".into(), json!({"type": "object", "minProperties": 1, "additionalProperties": false, "properties": properties}));
}

pub(crate) fn check_thresholds(value: &Value) -> Result<(), (Option<&str>, String)> {
    let thresholds = value
        .as_object()
        .filter(|map| !map.is_empty())
        .ok_or_else(|| (None, "expected a nonempty table of loss thresholds".into()))?;
    for (name, value) in thresholds {
        let metric = METRICS
            .iter()
            .find(|metric| metric.name == name)
            .ok_or_else(|| {
                (
                    Some(name.as_str()),
                    format!(
                        "expected one of {}",
                        METRICS
                            .iter()
                            .map(|metric| metric.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })?;
        if !value.as_f64().is_some_and(|n| {
            n.is_finite()
                && metric.min.is_none_or(|min| n >= min)
                && metric.max.is_none_or(|max| n <= max)
        }) {
            let min = metric
                .min
                .map_or_else(String::new, |min| format!(", minimum {min}"));
            let max = metric
                .max
                .map_or_else(String::new, |max| format!(", maximum {max}"));
            return Err((Some(name), format!("expected a finite number{min}{max}")));
        }
    }
    Ok(())
}
