//! Source-compatible settings boundary. Never render resolved secrets to stdout.
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::Path};
pub mod versioned;
pub type Environment = BTreeMap<String, String>;
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("gateway configuration file unavailable or oversized")]
    File,
    #[error("invalid gateway configuration format")]
    Format,
    #[error("invalid gateway setting: {0}")]
    Setting(String),
    #[error("GATEWAY_NATIVE_CONFIG must identify an explicit native deployment configuration")]
    NativeRequired,
}
type Result<T> = std::result::Result<T, ConfigError>;
fn invalid(path: &str) -> ConfigError {
    ConfigError::Setting(path.into())
}
fn schema_ref<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|p| root.pointer(p))
        .unwrap_or(schema)
}
fn field_schema<'a>(mut schema: &'a Value, path: &[&str], root: &'a Value) -> Option<&'a Value> {
    for part in path {
        schema = schema_ref(schema, root);
        if let Some(options) = schema["anyOf"].as_array() {
            if let Some(object) = options
                .iter()
                .map(|s| schema_ref(s, root))
                .find(|s| s["type"] == "object" || s.get("properties").is_some())
            {
                schema = object;
            }
        }
        schema = schema
            .get("properties")
            .and_then(|p| p.get(*part))
            .or_else(|| schema.get("additionalProperties").filter(|v| v.is_object()))?;
    }
    Some(schema_ref(schema, root))
}
fn coerce(value: Value, schema: &Value, root: &Value, path: &str) -> Result<Value> {
    let schema = schema_ref(schema, root);
    if let Some(alternatives) = schema["anyOf"].as_array() {
        for candidate in alternatives {
            if let Ok(v) = coerce(value.clone(), candidate, root, path) {
                return Ok(v);
            }
        }
        return Err(invalid(path));
    }
    let kind = schema["type"].as_str().unwrap_or("");
    match kind {
        "null" => {
            if value.is_null() {
                Ok(value)
            } else {
                Err(invalid(path))
            }
        }
        "string" => {
            if value.is_string() {
                Ok(value)
            } else {
                Err(invalid(path))
            }
        }
        "boolean" => {
            if value.is_boolean() {
                return Ok(value);
            }
            let raw = if let Some(s) = value.as_str() {
                s.to_lowercase()
            } else {
                value.to_string()
            };
            match raw.as_str() {
                "true" | "1" | "yes" | "y" | "on" | "t" => Ok(json!(true)),
                "false" | "0" | "no" | "n" | "off" | "f" => Ok(json!(false)),
                _ => Err(invalid(path)),
            }
        }
        "integer" => {
            if let Some(i) = value.as_i64() {
                return Ok(json!(i));
            }
            if let Some(i) = value.as_u64() {
                return Ok(json!(i));
            }
            if let Some(b) = value.as_bool() {
                return Ok(json!(i64::from(b)));
            }
            let raw = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            if let Ok(i) = raw.trim().parse::<i64>() {
                return Ok(json!(i));
            }
            let number = raw.trim().parse::<f64>().map_err(|_| invalid(path))?;
            if number.is_finite()
                && number.fract() == 0.
                && number >= i64::MIN as f64
                && number < i64::MAX as f64
            {
                Ok(json!(number as i64))
            } else {
                Err(invalid(path))
            }
        }
        "number" => {
            let n = if let Some(n) = value.as_f64() {
                n
            } else {
                value
                    .as_str()
                    .ok_or_else(|| invalid(path))?
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| invalid(path))?
            };
            if n.is_finite() {
                Ok(json!(n))
            } else {
                Err(invalid(path))
            }
        }
        "array" => {
            let value = if let Some(s) = value.as_str() {
                serde_json::from_str(s).map_err(|_| invalid(path))?
            } else {
                value
            };
            let items = value.as_array().ok_or_else(|| invalid(path))?;
            Ok(Value::Array(
                items
                    .iter()
                    .map(|v| coerce(v.clone(), &schema["items"], root, path))
                    .collect::<Result<Vec<_>>>()?,
            ))
        }
        "object" => {
            let value = if let Some(s) = value.as_str() {
                serde_json::from_str(s).map_err(|_| invalid(path))?
            } else {
                value
            };
            let object = value.as_object().ok_or_else(|| invalid(path))?;
            let mut out = serde_json::Map::new();
            for (key, value) in object {
                let field = schema
                    .get("properties")
                    .and_then(|p| p.get(key))
                    .or_else(|| schema.get("additionalProperties").filter(|s| s.is_object()));
                if let Some(field) = field {
                    out.insert(
                        key.clone(),
                        coerce(value.clone(), field, root, &format!("{path}.{key}"))?,
                    );
                } else if schema.get("properties").is_none() {
                    out.insert(key.clone(), value.clone());
                }
            }
            if let Some(required) = schema["required"].as_array() {
                for field in required {
                    if !out.contains_key(field.as_str().ok_or(ConfigError::Format)?) {
                        return Err(invalid(path));
                    }
                }
            }
            Ok(Value::Object(out))
        }
        _ => Ok(value),
    }
}
fn merge(target: &mut Value, value: Value) {
    if let (Some(a), Some(b)) = (target.as_object_mut(), value.as_object()) {
        for (k, v) in b {
            merge(a.entry(k.clone()).or_insert(Value::Null), v.clone());
        }
    } else {
        *target = value;
    }
}
fn merge_settings(target: &mut Value, value: Value, field: &Value, root: &Value) {
    let mut field = schema_ref(field, root);
    if let Some(options) = field["anyOf"].as_array() {
        if let Some(object) = options
            .iter()
            .map(|s| schema_ref(s, root))
            .find(|s| s.get("properties").is_some())
        {
            field = object;
        }
    }
    if field.get("properties").is_some() && value.is_object() {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, item) in value.as_object().unwrap() {
            if let Some(child) = field["properties"].get(key) {
                merge_settings(&mut target[key], item.clone(), child, root);
            }
        }
    } else {
        *target = value;
    }
}
fn put(target: &mut Value, path: &[&str], value: Value) -> Result<()> {
    let Some((first, rest)) = path.split_first() else {
        return Err(ConfigError::Format);
    };
    if !target.is_object() {
        *target = json!({});
    }
    if rest.is_empty() {
        merge(&mut target[*first], value);
    } else {
        put(&mut target[*first], rest, value)?;
    }
    Ok(())
}
fn schema() -> Result<Value> {
    let mut schema: Value = serde_json::from_str(include_str!("settings_schema.json"))
        .map_err(|_| ConfigError::Format)?;
    for (alias, name) in [
        ("DEFAULT_MODEL", "default_model"),
        ("MODEL_SETUP_MODE", "model_setup_mode"),
    ] {
        let field = schema["properties"]
            .as_object_mut()
            .unwrap()
            .remove(alias)
            .ok_or(ConfigError::Format)?;
        schema["properties"][name] = field;
    }
    Ok(schema)
}
pub fn resolve_settings(environment: &Environment) -> Result<Value> {
    resolve_settings_with_base(environment, None)
}
pub(crate) fn resolve_settings_with_base(
    environment: &Environment,
    base: Option<Value>,
) -> Result<Value> {
    let mut settings: Value = serde_json::from_str(include_str!("settings_defaults.json"))
        .map_err(|_| ConfigError::Format)?;
    let schema = schema()?;
    if let Some(base) = base {
        versioned::validate_known_settings(&base)?;
        let base = coerce(base, &schema, &schema, "settings")?;
        merge_settings(&mut settings, base, &schema, &schema);
    }
    if let Some(value) = environment.get("CORS_ALLOWED_ORIGINS") {
        settings["cors"]["allow_origins"] = json!(
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        );
    }
    for (tier, key) in [
        ("anonymous", "RATE_LIMIT_ANONYMOUS_LIMIT"),
        ("normal", "RATE_LIMIT_NORMAL_LIMIT"),
        ("premium", "RATE_LIMIT_PREMIUM_LIMIT"),
        ("enterprise", "RATE_LIMIT_ENTERPRISE_LIMIT"),
    ] {
        if let Some(value) = environment.get(key).and_then(|v| v.parse::<i64>().ok()) {
            settings["langgraph"]["tier_limits"][tier]["requests"] = json!(value);
        }
    }
    let mut entries = environment
        .iter()
        .filter_map(|(key, value)| {
            let key = key.to_lowercase();
            let prefixed = key.starts_with("gateway_");
            let path = if key == "default_model" || key == "model_setup_mode" {
                key
            } else {
                key.strip_prefix("gateway_")?.to_owned()
            };
            if matches!(path.as_str(), "default_model" | "model_setup_mode") && prefixed {
                return None;
            }
            Some((path, value))
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| key.matches("__").count());
    let mut overrides = json!({});
    for (key, raw) in entries {
        let path = key.split("__").collect::<Vec<_>>();
        let Some(field) = field_schema(&schema, &path, &schema) else {
            continue;
        };
        let value = coerce(json!(raw), field, &schema, &key)?;
        put(&mut overrides, &path, value)?;
    }
    merge_settings(&mut settings, overrides, &schema, &schema);
    let mut settings = coerce(settings, &schema, &schema, "settings")?;
    if !matches!(
        settings["model_setup_mode"].as_str(),
        Some("ui" | "environment")
    ) {
        return Err(invalid("model_setup_mode"));
    }
    let languages = settings["knowledge"]["ocr_languages"]
        .as_str()
        .unwrap_or("");
    if languages.is_empty() {
        settings["knowledge"]["ocr_languages"] = json!("eng+ara");
    } else {
        let allowed = [
            "eng", "ara", "chi_sim", "chi_tra", "fra", "deu", "spa", "rus", "jpn", "kor", "por",
            "ita", "nld", "tur", "vie", "tha",
        ];
        if !languages.split('+').all(|p| allowed.contains(&p.trim())) {
            return Err(invalid("knowledge.ocr_languages"));
        }
    }
    if environment
        .get("GATEWAY_ENV")
        .is_some_and(|s| s.to_lowercase() == "production")
        && [
            "",
            "default-secret-change-me",
            "your-jwt-secret-key",
            "your-jwt-secret-key-change-in-production",
            "change-me-in-production-use-strong-secret",
            "secret",
            "changeme",
        ]
        .contains(
            &settings["authentication"]["jwt"]["secret"]
                .as_str()
                .unwrap_or(""),
        )
    {
        return Err(invalid("authentication.jwt.secret"));
    }
    Ok(settings)
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|_| ConfigError::File)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::File)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(ConfigError::File);
    }
    Ok(bytes)
}
pub fn document(environment: Environment, mut native: Value) -> Result<Value> {
    if !native.is_object() {
        return Err(ConfigError::Format);
    }
    let environment = versioned::resolve_secret_files(environment)?;
    versioned::validate_environment_settings(&environment)?;
    let settings = resolve_settings(&environment)?;
    if let Some(value) = environment
        .get("AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION")
        .filter(|v| !v.trim().is_empty())
    {
        native["kernel_revision"] = json!(value.trim());
    }
    if let Some(value) = environment.get("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED") {
        native["worker_writes_enabled"] = coerce(
            json!(value),
            &json!({"type":"boolean"}),
            &json!({}),
            "worker_writes_enabled",
        )?;
    }
    let document = json!({"schema_version":versioned::VERSION,"settings":settings,"environment":environment,"native":native});
    versioned::validate_document(&document)?;
    Ok(document)
}
pub fn load_gateway_document_from_process() -> Result<Value> {
    let process: Environment = std::env::vars().collect();
    let dotenv = process
        .get("GATEWAY_DOTENV_FILE")
        .map(String::as_str)
        .unwrap_or(".env");
    let mut environment = Environment::new();
    if !dotenv.is_empty() && Path::new(dotenv).exists() {
        let bytes = read(Path::new(dotenv))?;
        for pair in dotenvy::from_read_iter(bytes.as_slice()) {
            let (k, v) = pair.map_err(|_| ConfigError::Format)?;
            environment.insert(k, v);
        }
    }
    environment.extend(process);
    let native_path = environment
        .get("GATEWAY_NATIVE_CONFIG")
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::NativeRequired)?;
    let raw = read(Path::new(native_path))?;
    let native: Value = if Path::new(native_path)
        .extension()
        .is_some_and(|e| e == "json")
    {
        serde_json::from_slice(&raw).map_err(|_| ConfigError::Format)?
    } else {
        serde_yaml::from_slice(&raw).map_err(|_| ConfigError::Format)?
    };
    if native.get("schema_version").is_some() || native.get("native").is_some() {
        let source: versioned::ConfigurationSource =
            serde_json::from_value(native).map_err(|_| ConfigError::Format)?;
        source.resolve(environment)
    } else {
        document(environment, native)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn env(items: &[(&str, &str)]) -> Environment {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    #[test]
    fn source_defaults() {
        let actual = resolve_settings(&env(&[])).unwrap();
        let expected: Value = serde_json::from_str(include_str!("settings_defaults.json")).unwrap();
        assert_eq!(actual, expected);
    }
    #[test]
    fn nested_overrides_json() {
        let v = resolve_settings(&env(&[
            ("GATEWAY_DATABASE", r#"{"enabled":false,"pool_min_size":4}"#),
            ("GATEWAY_DATABASE__ENABLED", "yes"),
            ("GATEWAY_DATABASE__POOL_MAX_SIZE", "20"),
            ("DEFAULT_MODEL", "test-model"),
        ]))
        .unwrap();
        assert_eq!(v["database"]["enabled"], true);
        assert_eq!(v["database"]["pool_min_size"], 4);
        assert_eq!(v["database"]["pool_max_size"], 20);
        assert_eq!(v["default_model"], "test-model");
    }
    #[test]
    fn complex_types() {
        let v = resolve_settings(&env(&[
            ("GATEWAY_PROXY__TIMEOUT_READ", "12.5"),
            ("GATEWAY_KNOWLEDGE__ENABLED", "false"),
            ("GATEWAY_AUTHENTICATION__JWT__ALGORITHMS", r#"["HS256"]"#),
            ("GATEWAY_STORAGE__S3__SECRET_KEY", "00001234"),
            ("GATEWAY_LANGGRAPH__INSTANCE_URLS", "http://one,http://two"),
        ]))
        .unwrap();
        assert_eq!(v["proxy"]["timeout_read"], 12.5);
        assert_eq!(v["knowledge"]["enabled"], false);
        assert_eq!(v["storage"]["s3"]["secret_key"], "00001234");
    }
    #[test]
    fn invalid_settings_hide_values() {
        let err = resolve_settings(&env(&[("GATEWAY_PORT", "secret-fixture")]))
            .unwrap_err()
            .to_string();
        assert!(!err.contains("secret-fixture"));
        assert!(resolve_settings(&env(&[("MODEL_SETUP_MODE", "bad")])).is_err());
        assert!(resolve_settings(&env(&[("GATEWAY_ENV", "production")])).is_err());
    }
    #[test]
    fn source_factories() {
        let v = resolve_settings(&env(&[
            ("CORS_ALLOWED_ORIGINS", "https://a, https://b"),
            ("RATE_LIMIT_ANONYMOUS_LIMIT", "123"),
            ("GATEWAY_KNOWLEDGE__OCR_LANGUAGES", ""),
        ]))
        .unwrap();
        assert_eq!(
            v["cors"]["allow_origins"],
            json!(["https://a", "https://b"])
        );
        assert_eq!(v["langgraph"]["tier_limits"]["anonymous"]["requests"], 123);
        assert_eq!(v["knowledge"]["ocr_languages"], "eng+ara");
    }
}
#[cfg(test)]
mod parity {
    use super::*;
    #[test]
    fn source_settings_goldens() {
        let cases: Value = serde_json::from_str(include_str!("settings_goldens.json")).unwrap();
        for (index, case) in cases.as_array().unwrap().iter().enumerate() {
            let environment: Environment =
                serde_json::from_value(case["environment"].clone()).unwrap();
            assert_eq!(
                resolve_settings(&environment).unwrap(),
                case["expected"],
                "fixture {index}"
            );
        }
    }
}
