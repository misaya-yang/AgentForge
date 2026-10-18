//! Versioned strict settings, explicit file-secret resolution, and public schema.
//! This boundary performs file I/O only on explicit operator-supplied paths.
use super::{ConfigError, Environment, invalid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
pub const VERSION: u32 = 1;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "u32", into = "u32")]
pub struct SchemaVersion;
impl TryFrom<u32> for SchemaVersion {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == VERSION {
            Ok(Self)
        } else {
            Err("gateway_schema_version")
        }
    }
}
impl From<SchemaVersion> for u32 {
    fn from(_: SchemaVersion) -> u32 {
        VERSION
    }
}
impl Default for SchemaVersion {
    fn default() -> Self {
        Self
    }
}
pub fn schema() -> Value {
    serde_json::from_str(include_str!("gateway_document_schema.json"))
        .expect("bundled Gateway schema is valid JSON")
}
pub fn validate_document(document: &Value) -> Result<(), ConfigError> {
    let schema = schema();
    let validator = jsonschema::validator_for(&schema).map_err(|_| ConfigError::Format)?;
    validator
        .validate(document)
        .map_err(|error| invalid(&format!("document{}", error.instance_path)))
}
pub fn validate_environment_settings(environment: &Environment) -> Result<(), ConfigError> {
    let schema = super::schema()?;
    for key in environment.keys() {
        let lower = key.to_ascii_lowercase();
        if let Some(path) = lower.strip_prefix("gateway_") {
            let parts = path.split("__").collect::<Vec<_>>();
            if parts.len() > 1 && super::field_schema(&schema, &parts, &schema).is_none() {
                let value = &environment[key];
                let valid = match key.as_str() {
                    "GATEWAY_AUTHENTICATION__API_KEY__STATIC_ROLE" => {
                        ["user", "admin", "operator", "developer"].contains(&value.as_str())
                    }
                    "GATEWAY_STORAGE__S3__SERVER_SIDE_ENCRYPTION" => {
                        ["", "AES256", "aws:kms"].contains(&value.as_str())
                    }
                    "GATEWAY_STORAGE__S3__KMS_KEY_ID" => value.len() <= 2048,
                    _ => false,
                };
                if !valid {
                    return Err(invalid(key));
                }
            }
        }
    }
    Ok(())
}
pub fn validate_known_settings(settings: &Value) -> Result<(), ConfigError> {
    fn visit(value: &Value, field: &Value, root: &Value, path: &str) -> Result<(), ConfigError> {
        let mut field = super::schema_ref(field, root);
        if let Some(options) = field["anyOf"].as_array() {
            if let Some(candidate) = options
                .iter()
                .find(|f| super::schema_ref(f, root).get("properties").is_some())
            {
                field = super::schema_ref(candidate, root);
            }
        }
        if let Some(items) = value.as_array() {
            for (i, item) in items.iter().enumerate() {
                visit(item, &field["items"], root, &format!("{path}[{i}]"))?;
            }
        }
        if let Some(object) = value.as_object() {
            if let Some(properties) = field["properties"].as_object() {
                for (key, child) in object {
                    let child_field = properties
                        .get(key)
                        .ok_or_else(|| invalid(&format!("{path}.{key}")))?;
                    visit(child, child_field, root, &format!("{path}.{key}"))?;
                }
            }
        }
        Ok(())
    }
    let schema = super::schema()?;
    visit(settings, &schema, &schema, "settings")
}
pub fn validate_settings(settings: &Value) -> Result<(), ConfigError> {
    let mut schema = schema();
    schema["$ref"] = json!("#/$defs/Settings");
    // Retain local definitions but remove the document's root constraints.
    for key in ["type", "properties", "required", "additionalProperties"] {
        schema.as_object_mut().unwrap().remove(key);
    }
    let validator = jsonschema::validator_for(&schema).map_err(|_| ConfigError::Format)?;
    validator
        .validate(settings)
        .map_err(|error| invalid(&format!("settings{}", error.instance_path)))
}
fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.ends_with("_PASSWORD")
        || name.ends_with("_SECRET")
        || name.ends_with("_TOKEN")
        || name.ends_with("_API_KEY")
        || name.ends_with("_PRIVATE_KEY")
        || name.ends_with("_ENCRYPTION_KEY")
        || name == "INTERNAL_AUTH_KEYS"
        || name == "DATABASE_URL"
        || name == "GATEWAY_DATABASE__DSN"
}
fn secret_file(path: &Path, name: &str) -> Result<String, ConfigError> {
    if !path.is_absolute() {
        return Err(invalid(name));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|_| invalid(name))?;
    let metadata = file.metadata().map_err(|_| invalid(name))?;
    if !metadata.is_file() || metadata.len() > 65536 {
        return Err(invalid(name));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(invalid(name));
        }
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid(name))?;
    if bytes.len() > 65536 {
        return Err(invalid(name));
    }
    let mut value = String::from_utf8(bytes).map_err(|_| invalid(name))?;
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    if value.is_empty() || value.contains('\0') {
        return Err(invalid(name));
    }
    Ok(value)
}
/// Resolve only secret-valued `_FILE` keys. CA/certificate paths and the existing
/// Valkey/Data connection password-file contracts stay paths: their owning
/// resource adapters validate private permissions, TLS and role bindings.
pub fn resolve_secret_files(mut environment: Environment) -> Result<Environment, ConfigError> {
    let files = environment
        .iter()
        .filter_map(|(key, path)| {
            let base = key.strip_suffix("_FILE")?;
            (secret_name(base)
                && !base.starts_with("AI_PLATFORM_VALKEY_")
                && !base.starts_with("DATA_"))
            .then(|| (key.clone(), base.to_owned(), path.clone()))
        })
        .collect::<Vec<_>>();
    for (key, base, path) in files {
        if environment.get(&base).is_some_and(|v| !v.is_empty()) {
            return Err(invalid(&base));
        }
        let value = secret_file(Path::new(&path), &base)?;
        environment.remove(&key);
        environment.insert(base, value);
    }
    Ok(environment)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSource {
    #[serde(default)]
    pub schema_version: SchemaVersion,
    #[serde(default)]
    pub settings: Option<Value>,
    #[serde(default)]
    pub environment: Environment,
    pub native: Value,
    #[serde(default)]
    pub secret_files: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub control_plane: Option<Value>,
}
impl ConfigurationSource {
    pub fn resolve(self, mut environment: Environment) -> Result<Value, ConfigError> {
        let mut source = self.environment;
        source.append(&mut environment);
        let environment = resolve_secret_files(source)?;
        validate_environment_settings(&environment)?;
        let settings = super::resolve_settings_with_base(&environment, self.settings)?;
        let mut native = self.native;
        if let Some(revision) = environment
            .get("AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION")
            .filter(|s| !s.trim().is_empty())
        {
            native["kernel_revision"] = json!(revision.trim());
        }
        if let Some(value) = environment.get("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED") {
            native["worker_writes_enabled"] = super::coerce(
                json!(value),
                &json!({"type":"boolean"}),
                &json!({}),
                "worker_writes_enabled",
            )?;
        }
        let mut document = json!({"schema_version":VERSION,"settings":settings,"environment":environment,"native":native});
        if let Some(config) = self.control_plane {
            document["control_plane"] = config;
        }
        for (pointer, path) in self.secret_files {
            // Pointer targets must already be typed, string-valued fields.
            let target = document
                .pointer_mut(&pointer)
                .ok_or_else(|| invalid("secret_files"))?;
            if !target.is_string() {
                return Err(invalid("secret_files"));
            }
            let field = pointer.rsplit('/').next().unwrap_or("");
            if !secret_name(&format!("GATEWAY_{}", field))
                && !matches!(field, "secret" | "password" | "token" | "dsn")
            {
                return Err(invalid("secret_files"));
            }
            if target.as_str().is_some_and(|s| !s.is_empty()) {
                return Err(invalid("secret_files_conflict"));
            }
            *target = json!(secret_file(&path, "secret_files")?);
        }
        validate_document(&document)?;
        Ok(document)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_versions_and_unknown_nested_keys_are_rejected() {
        assert!(SchemaVersion::try_from(2).is_err());
        let mut settings = super::super::resolve_settings(&Environment::new()).unwrap();
        settings["database"]["pool_max_szie"] = json!(99);
        assert!(validate_settings(&settings).is_err());
    }
    #[test]
    fn file_secrets_preserve_bytes_and_never_disclose_path_or_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-marker");
        std::fs::write(&path, b" leading value \r\n").unwrap();
        let env = Environment::from([(
            "JWT_SECRET_FILE".into(),
            path.to_string_lossy().into_owned(),
        )]);
        let resolved = resolve_secret_files(env.clone()).unwrap();
        assert_eq!(resolved["JWT_SECRET"], " leading value ");
        assert!(!resolved.contains_key("JWT_SECRET_FILE"));
        let mut conflict = env;
        conflict.insert("JWT_SECRET".into(), "inline-marker".into());
        let error = resolve_secret_files(conflict).unwrap_err().to_string();
        assert!(!error.contains("private-marker"));
        assert!(!error.contains("inline-marker"));
        assert!(
            resolve_secret_files(Environment::from([(
                "JWT_SECRET_FILE".into(),
                "relative".into()
            )]))
            .is_err()
        );
    }
    #[test]
    fn ca_and_resource_owned_file_contracts_are_not_reinterpreted() {
        let env = Environment::from([
            (
                "AI_PLATFORM_VALKEY_COORD_PASSWORD_FILE".into(),
                "/not-read".into(),
            ),
            ("SDK_AGENT_WORKER_TLS_CA_FILE".into(), "/not-read".into()),
            ("DATA_GATEWAY_DSN_FILE".into(), "/not-read".into()),
            ("DATA_GATEWAY_PASSWORD_FILE".into(), "/not-read".into()),
            (
                "DATA_GATEWAY_CONTROL_PASSWORD_FILE".into(),
                "/not-read".into(),
            ),
        ]);
        assert_eq!(resolve_secret_files(env.clone()).unwrap(), env);
    }
}

#[cfg(test)]
mod strict_document_tests {
    use super::*;
    fn native() -> Value {
        json!({"kernel_revision":"test-runtime","worker_writes_enabled":false,"local_utc_offset_seconds":0,"allow_private_knowledge_http":false,"pdf":{"pdfinfo":"/usr/bin/pdfinfo","pdftotext":"/usr/bin/pdftotext","scratch_root":"/tmp/pdf","allow_attachment_alternative":false},"local_channel":"disabled","confluence_registration":"disabled","model_publication":"absent","provider_publication":"absent"})
    }
    #[test]
    fn partial_versioned_source_is_strict_and_process_override_order_is_retained() {
        let source:ConfigurationSource=serde_json::from_value(json!({"schema_version":1,"settings":{"port":8089},"environment":{"GATEWAY_PORT":"8090"},"native":native()})).unwrap();
        let document = source
            .resolve(Environment::from([
                ("GATEWAY_PORT".into(), "8091".into()),
                (
                    "AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION".into(),
                    "actual".into(),
                ),
            ]))
            .unwrap();
        assert_eq!(document["settings"]["port"], 8091);
        assert_eq!(document["native"]["kernel_revision"], "actual");
        assert!(
            validate_environment_settings(&Environment::from([(
                "GATEWAY_DATABASE__POOL_MAX_SZIE".into(),
                "1".into()
            )]))
            .is_err()
        );
        assert!(
            validate_environment_settings(&Environment::from([(
                "GATEWAY_AUTHENTICATION__API_KEY__STATIC_ROLE".into(),
                "admin".into()
            )]))
            .is_ok()
        );
    }
    #[test]
    fn pointer_secrets_are_resolved_before_validation_and_never_override_inline_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "a-test-secret-that-is-at-least-32-bytes\n").unwrap();
        let source:ConfigurationSource=serde_json::from_value(json!({"schema_version":1,"settings":{"authentication":{"jwt":{"secret":""}}},"secret_files":{"/settings/authentication/jwt/secret":path},"native":native()})).unwrap();
        let resolved = source.resolve(Environment::new()).unwrap();
        assert_eq!(
            resolved["settings"]["authentication"]["jwt"]["secret"],
            "a-test-secret-that-is-at-least-32-bytes"
        );
        let source:ConfigurationSource=serde_json::from_value(json!({"settings":{"authentication":{"jwt":{"secret":"inline"}}},"secret_files":{"/settings/authentication/jwt/secret":path},"native":native()})).unwrap();
        assert!(source.resolve(Environment::new()).is_err());
    }
    #[test]
    fn schema_errors_keep_document_settings_and_array_instance_paths_without_values() {
        let error_path = |result: Result<(), ConfigError>| match result {
            Err(ConfigError::Setting(path)) => path,
            other => panic!("expected schema instance path, got {other:?}"),
        };
        assert_eq!(error_path(validate_document(&Value::Null)), "document");
        assert_eq!(error_path(validate_settings(&Value::Null)), "settings");
        // The standalone Settings reference must not inherit the document's
        // required native/settings fields after the jsonschema API adaptation.
        assert!(validate_settings(&json!({})).is_ok());
        assert_eq!(
            error_path(validate_settings(&json!({"port":"private-invalid-value"}))),
            "settings/port"
        );
        assert_eq!(
            error_path(validate_settings(
                &json!({"authentication":{"jwt":{"algorithms":["HS256",17]}}})
            )),
            "settings/authentication/jwt/algorithms/1"
        );
        let mut document = super::super::document(Environment::new(), native()).unwrap();
        document["schema_version"] = json!(2);
        assert_eq!(
            error_path(validate_document(&document)),
            "document/schema_version"
        );
        document["schema_version"] = json!(VERSION);
        document["environment"] = json!({"private/key~name":false});
        assert_eq!(
            error_path(validate_document(&document)),
            "document/environment/private~1key~0name"
        );
        // Keep parent-instance paths for additionalProperties failures. Schema
        // diagnostics must not include the full rejected settings or secret DSN.
        let error = validate_settings(
            &json!({"database":{"dsn":"private-secret-marker","pool_max_szie":1}}),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid gateway setting: settings/database"
        );
    }
    #[test]
    fn every_typed_document_section_rejects_unknown_fields() {
        let mut document = super::super::document(Environment::new(), native()).unwrap();
        document["native"]["pdf"]["scratch_rooot"] = json!("typo");
        assert!(validate_document(&document).is_err());
        document["native"]["pdf"]
            .as_object_mut()
            .unwrap()
            .remove("scratch_rooot");
        document["control_plane"] = json!({"mcp":{"resource":"https://gateway.test/mcp","authorization_servers":["https://gateway.test"],"sesssions":true}});
        assert!(validate_document(&document).is_err());
    }
}

#[cfg(test)]
mod resource_policy_schema_tests {
    #[test]
    fn published_schema_accepts_only_named_resource_flips_and_explicit_digest_fields() {
        let full: serde_json::Value =
            serde_json::from_str(include_str!("gateway_document_schema.json")).unwrap();
        let schema = &full["properties"]["control_plane"]["properties"]["cedar"];
        let valid = serde_json::json!({"mode":"enforce","resources":{"enforce":["tenant","user","dataset","agent","tool","model"],"expected_policy_sha256":"a".repeat(64),"expected_matrix_sha256":"b".repeat(64)}});
        assert!(jsonschema::is_valid(schema, &valid));
        for invalid in [
            serde_json::json!({"resources":{"enforce":["all"]}}),
            serde_json::json!({"resources":{"enforce":["dataset","dataset"]}}),
            serde_json::json!({"resources":{"allow_legacy":true}}),
        ] {
            assert!(!jsonschema::is_valid(schema, &invalid));
        }
    }
}
