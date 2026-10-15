use super::*;
const KEYS: &[&str] = &[
    "schema_version",
    "id",
    "name",
    "version",
    "description",
    "schema_hash",
    "input_schema",
    "output_schema",
    "effect",
    "approval_policy",
    "execution_mode",
    "timeout_ms",
    "tags",
    "protocol",
    "connector_binding",
];
pub(super) struct Descriptor {
    pub value: Value,
    pub kind: String,
    pub category: String,
    pub required: BTreeSet<String>,
}
fn sensitive(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| {
            let k: String = k
                .to_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            [
                "apikey",
                "apitoken",
                "authorization",
                "bearertoken",
                "clientsecret",
                "credential",
                "credentialref",
                "dsn",
                "password",
                "privatekey",
                "refreshtoken",
                "secret",
                "secretref",
                "token",
                "tokenref",
            ]
            .contains(&k.as_str())
                || sensitive(v)
        }),
        Value::Array(a) => a.iter().any(sensitive),
        _ => false,
    }
}
impl Descriptor {
    pub(super) fn parse(raw: &Value, record: Option<&Value>) -> Result<Self, AgentError> {
        let invalid = || error("CAPABILITY_CATALOG_INVALID");
        let map = raw.as_object().ok_or_else(invalid)?;
        if map.keys().any(|k| !KEYS.contains(&k.as_str()))
            || KEYS
                .iter()
                .filter(|k| **k != "connector_binding")
                .any(|k| !map.contains_key(*k))
            || raw["schema_version"] != "capability-descriptor/v2"
        {
            return Err(invalid());
        }
        let name = text(raw, "name");
        let identifier = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,159}$").unwrap();
        let description = text(raw, "description");
        let version = text(raw, "version");
        let protocol = text(raw, "protocol");
        let tags = raw["tags"].as_array().ok_or_else(invalid)?;
        let hash = format!(
            "sha256:{:x}",
            Sha256::digest(
                crate::migration_content::python_json(&raw["input_schema"], false, false)
                    .as_bytes()
            )
        );
        if !identifier.is_match(name)
            || raw["id"] != name
            || version.is_empty()
            || version.chars().count() > 128
            || description.is_empty()
            || description.chars().count() > 20000
            || description.chars().any(|c| (c as u32) < 32 && c != '\n')
            || !raw["input_schema"].is_object()
            || !raw["output_schema"].is_object()
            || raw["schema_hash"] != hash
            || !["read", "write", "unknown"].contains(&text(raw, "effect"))
            || !["never", "on_request", "always"].contains(&text(raw, "approval_policy"))
            || !["inline", "stream", "job"].contains(&text(raw, "execution_mode"))
            || !raw["timeout_ms"]
                .as_i64()
                .is_some_and(|n| n > 0 && n <= 300000)
            || tags.len() > 128
            || tags.iter().any(|v| {
                v.as_str().is_none_or(|s| {
                    s.is_empty() || s.chars().count() > 160 || s.chars().any(|c| (c as u32) < 32)
                })
            })
            || protocol.is_empty()
            || protocol.chars().count() > 128
            || sensitive(raw)
        {
            return Err(invalid());
        }
        let tags: BTreeSet<String> = tags.iter().map(|v| v.as_str().unwrap().into()).collect();
        if tags.len() != raw["tags"].as_array().unwrap().len()
            || (raw["effect"] == "read") != (raw["approval_policy"] == "never")
            || raw
                .get("connector_binding")
                .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(invalid());
        }
        let kinds = [
            "knowledge",
            "mcp",
            "connector",
            "office_read",
            "platform_tool_discovery",
            "tool",
        ];
        let kind = record
            .map(|r| text(r, "kind"))
            .filter(|k| kinds.contains(k))
            .map(str::to_owned)
            .or_else(|| {
                kinds
                    .iter()
                    .find(|k| tags.contains(&format!("kind:{k}")))
                    .map(|s| s.to_string())
            })
            .or_else(|| tags.contains("fixture").then(|| "tool".into()))
            .ok_or_else(invalid)?;
        // A Worker activation is not inferred from the echoed catalog revision.
        // Both sides must select the same immutable deployment generation. This
        // also rejects pack descriptors missing from Gateway's local catalog.
        let pack_tagged = tags.iter().any(|tag| tag.starts_with("plugin-"));
        let pack_record = record.is_some_and(|r| r["plugin_metadata"].get("activation_id").is_some()
            || r["plugin_metadata"].get("artifact_sha256").is_some());
        if pack_tagged || pack_record {
            let r = record.ok_or_else(invalid)?;
            if r["plugin_metadata"]["worker_active"] == false {
                return Err(error("CAPABILITY_PLUGIN_ACTIVATION_MISMATCH"));
            }
            let id = text(r, "plugin_id");
            let version = text(r, "plugin_version");
            let artifact = text(&r["plugin_metadata"], "artifact_sha256");
            let activation = text(&r["plugin_metadata"], "activation_id");
            if id.is_empty() || version.is_empty()
                || artifact != text(r, "content_hash")
                || !artifact.strip_prefix("sha256:").is_some_and(|v| v.len() == 64
                    && v.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
                || uuid::Uuid::parse_str(activation).is_err()
                || !tags.contains(&format!("plugin-id:{id}"))
                || !tags.contains(&format!("plugin-version:{version}"))
                || !tags.contains(&format!("plugin-pack:{}", artifact.strip_prefix("sha256:").unwrap_or("")))
                || !tags.contains(&format!("plugin-activation:{activation}"))
                || tags.iter().filter(|tag| tag.starts_with("plugin-")).count() != 4
            {
                return Err(error("CAPABILITY_PLUGIN_ACTIVATION_MISMATCH"));
            }
        }
        let mut required = BTreeSet::new();
        let mut category = String::new();
        if let Some(r) = record {
            let expected = if r["version"].is_null() {
                "null".into()
            } else {
                pystr(&r["version"])
            };
            if version != expected
                || raw["description"] != r["description"]
                || raw["input_schema"] != r["input_schema"]
                || raw["schema_hash"] != r["schema_hash"]
                || raw["effect"] != r["effect"]
                || raw["approval_policy"] != r["approval"]
                || raw["timeout_ms"] != r["timeout_ms"]
                || raw["protocol"] != r["protocol"]
                || !tags.contains(&format!("kind:{}", text(r, "kind")))
            {
                return Err(invalid());
            }
            category = text(r, "category").into();
            required.extend(
                r["required_permissions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(pystr),
            );
        }
        for tag in raw["tags"].as_array().unwrap() {
            let tag = tag.as_str().unwrap();
            if let Some(v) = tag.strip_prefix("permission:") {
                required.insert(v.into());
            }
            if let Some(v) = tag.strip_prefix("category:") {
                category = v.into();
            }
        }
        Ok(Self {
            value: raw.clone(),
            kind,
            category,
            required,
        })
    }
    pub(super) fn project(&self, tenant: &str, revision: i64) -> Value {
        let v = &self.value;
        let mut result = json!({"name":v["name"],"id":v["id"],"version":if v["version"]=="null"{Value::Null}else{v["version"].clone()},"schema_hash":v["schema_hash"],"description":v["description"],"schema":v["input_schema"],"output_schema":v["output_schema"],"source":"capability_worker","kind":self.kind,"read_only":v["effect"]=="read","effect":v["effect"],"approval_policy":v["approval_policy"],"execution_mode":v["execution_mode"],"timeout_ms":v["timeout_ms"],"tags":v["tags"],"protocol":v["protocol"],"tenant_id":tenant,"capability_revision":revision});
        if v["connector_binding"].is_object() {
            result["connector_binding"] = v["connector_binding"].clone();
        }
        result
    }
}
pub(super) fn permitted(subjects: &BTreeSet<String>, required: &BTreeSet<String>) -> bool {
    if required.is_empty() {
        return true;
    }
    let admin = subjects.contains("admin");
    let tiers = ["anonymous", "normal", "premium", "enterprise", "admin"];
    let tier = subjects
        .iter()
        .find_map(|s| s.strip_prefix("tier:"))
        .unwrap_or("normal");
    let tier = if tier.is_empty() { "anonymous".to_owned() } else { tier.to_lowercase() };
    // Match the Assistant registry's permission lattice: role admin may bypass
    // roles/named permissions, but never an explicit tier requirement. Unknown
    // caller tiers fail closed before any admin shortcut.
    if !tiers.contains(&tier.as_str()) {
        return false;
    }
    required.iter().all(|p| {
        if let Some(role) = p.strip_prefix("role:") {
            admin || subjects.contains(role.trim())
        } else if let Some(t) = p.strip_prefix("tier:") {
            let t = t.trim().to_lowercase();
            match (
                tiers.iter().position(|v| *v == tier),
                tiers.iter().position(|v| *v == t),
            ) {
                (Some(a), Some(b)) => a >= b,
                _ => false,
            }
        } else {
            admin || subjects.contains(p)
        }
    })
}
pub(super) fn policy_set(v: &Value) -> Result<BTreeSet<String>, AgentError> {
    if v.is_null() {
        return Ok(BTreeSet::new());
    }
    let a = v
        .as_array()
        .filter(|a| a.len() <= 256)
        .ok_or_else(|| error("CAPABILITY_CATALOG_POLICY_UNAVAILABLE"))?;
    a.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| {
                    !s.is_empty() && s.chars().count() <= 160 && !s.chars().any(|c| (c as u32) < 32)
                })
                .map(str::to_owned)
                .ok_or_else(|| error("CAPABILITY_CATALOG_POLICY_UNAVAILABLE"))
        })
        .collect()
}
