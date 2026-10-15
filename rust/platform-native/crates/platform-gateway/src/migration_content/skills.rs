//! Immutable, tenant-owned instruction artifacts; never dispatches tenant code.
use super::{ContentAccess, ContentDatabase, ContentError, ContentIdentity, sha256, string};
use axum::{
    Json, Router,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

fn policy(code: &str, message: &str, field: &str) -> ContentError {
    ContentError::new(422, json!({"code":code,"message":message,"field":field}))
}
fn unavailable(code: &str) -> ContentError {
    ContentError::code(
        if code == "SKILL_VERSION_UNAVAILABLE" {
            409
        } else {
            503
        },
        code,
        "Skill artifact is unavailable",
    )
}
fn conflict(code: &str) -> ContentError {
    ContentError::code(409, code, "Skill artifact conflicts with current state")
}
fn not_found() -> ContentError {
    ContentError::code(404, "SKILL_NOT_FOUND", "Skill not found")
}
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}
fn py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}
fn py_trim(s: &str) -> &str {
    s.trim_matches(py_space)
}
fn py_trim_end(s: &str) -> &str {
    s.trim_end_matches(py_space)
}
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    static NONPRINT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = NONPRINT.get_or_init(|| {
        regex::Regex::new(r"[\p{C}\p{Z}]").expect("static Python printable category pattern")
    });
    !regex.is_match(c.encode_utf8(&mut [0; 4]))
}
fn py_repr_string(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c)
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n <= 255 {
                    out.push_str(&format!("\\x{n:02x}"))
                } else if n <= 65535 {
                    out.push_str(&format!("\\u{n:04x}"))
                } else {
                    out.push_str(&format!("\\U{n:08x}"))
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_string(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_string(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) if n.is_f64() => {
            crate::assistant_catalog::python_float(n.as_f64().expect("finite JSON number"))
        }
        _ => v.to_string(),
    }
}
fn py_string(v: &Value) -> String {
    if let Value::String(s) = v {
        s.clone()
    } else {
        py_repr(v)
    }
}
/// SKILL.md uses Python int(), not Pydantic's lossless integer coercion.
fn py_int(v: &Value) -> Result<i64, ContentError> {
    let invalid = || {
        policy(
            "SKILL_MANIFEST_INVALID",
            "max_context_tokens must be an integer",
            "file",
        )
    };
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            let f = n.as_f64().ok_or_else(invalid)?;
            if !f.is_finite() || f < i64::MIN as f64 || f >= 9223372036854775808.0 {
                return Err(invalid());
            }
            Ok(f.trunc() as i64)
        }
        Value::String(s) => {
            let s = py_trim(s);
            let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
            if digits.is_empty()
                || !digits.bytes().enumerate().all(|(i, c)| {
                    c.is_ascii_digit()
                        || (c == b'_'
                            && i > 0
                            && digits.as_bytes()[i - 1].is_ascii_digit()
                            && digits.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit))
                })
            {
                return Err(invalid());
            }
            s.replace('_', "").parse().map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }
}
fn py_list(v: &Value) -> Result<Value, ContentError> {
    match v {
        Value::Array(_) => Ok(v.clone()),
        Value::String(s) => Ok(json!(s.chars().map(|c| c.to_string()).collect::<Vec<_>>())),
        Value::Object(m) => Ok(json!(m.keys().collect::<Vec<_>>())),
        _ => Err(policy(
            "SKILL_MANIFEST_INVALID",
            "Skill list metadata is not iterable",
            "manifest",
        )),
    }
}
fn py_dict(v: &Value) -> Result<Value, ContentError> {
    if v.is_object() {
        return Ok(v.clone());
    }
    if let Some(items) = v.as_array() {
        let mut map = Map::new();
        for item in items {
            let pair = py_list(item)?;
            let pair = pair.as_array().filter(|p| p.len() == 2).ok_or_else(|| {
                policy(
                    "SKILL_MANIFEST_INVALID",
                    "Skill dictionary metadata requires key/value pairs",
                    "manifest",
                )
            })?;
            let key = match &pair[0] {
                Value::String(s) => s.clone(),
                Value::Bool(b) => if *b { "true" } else { "false" }.into(),
                Value::Null => "null".into(),
                Value::Number(n) => n.to_string(),
                _ => {
                    return Err(policy(
                        "SKILL_MANIFEST_INVALID",
                        "Skill dictionary key must be scalar",
                        "manifest",
                    ));
                }
            };
            map.insert(key, pair[1].clone());
        }
        return Ok(Value::Object(map));
    }
    Err(policy(
        "SKILL_MANIFEST_INVALID",
        "Skill dictionary metadata is not a mapping",
        "manifest",
    ))
}
fn defaulted(v: &Value, k: &str, d: Value) -> Value {
    v.get(k).cloned().unwrap_or(d)
}
fn mapping(v: &Value) -> Value {
    if v.is_object() {
        v.clone()
    } else if let Some(s) = v.as_str() {
        serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(json!({}))
    } else {
        json!({})
    }
}
fn strings(v: &Value) -> Value {
    let v = if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or(Value::Null)
    } else {
        v.clone()
    };
    json!(
        v.as_array()
            .map(|a| a
                .iter()
                .filter(|v| v.is_string())
                .cloned()
                .collect::<Vec<_>>())
            .unwrap_or_default()
    )
}
pub fn normalize_skill_md(content: &str) -> Result<String, ContentError> {
    if content.contains('\0') {
        return Err(policy(
            "SKILL_CONTENT_INVALID",
            "SKILL.md must not contain NUL bytes",
            "file",
        ));
    }
    Ok(format!(
        "{}\n",
        py_trim_end(&content.replace("\r\n", "\n").replace('\r', "\n"))
    ))
}
pub fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    static FRONT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = FRONT.get_or_init(|| {
        regex::Regex::new(r"(?s)^---[\s\x1c-\x1f]*\n(.*?)\n---[\s\x1c-\x1f]*\n?(.*)")
            .expect("static source frontmatter pattern")
    });
    let captures = regex.captures(content)?;
    let front = captures.get(1)?.as_str();
    if front.is_empty() {
        return None;
    }
    Some((front, captures.get(2)?.as_str()))
}
pub fn activation_requirements(m: &Value) -> Value {
    let review = &m["review"];
    let verdict = review
        .get("verdict")
        .filter(|v| truth(v))
        .or_else(|| review.get("critic_verdict"))
        .map(py_string)
        .unwrap_or_default()
        .to_lowercase();
    let independent_critic = ["critic_artifact", "critic_report", "artifact"].iter().any(|k| truth(&review[*k]))
        && (verdict.is_empty() || ["approved", "approve", "pass", "passed"].contains(&verdict.as_str()));
    let eval_evidence = ["evidence", "eval_artifact", "test_command"].iter().any(|k| truth(&m["evaluation"][*k])) || m["evaluation"]["passed"] == true;
    let rollback_metadata = ["previous_version", "strategy", "rollback_artifact", "restore_from"].iter().any(|k| truth(&m["rollback"][*k]));
    json!({"independent_critic": independent_critic, "eval_evidence": eval_evidence, "rollback_metadata": rollback_metadata})
}
pub fn review_required(m: &Value) -> bool {
    truth(&m["generated"])
        && !activation_requirements(m)
            .as_object()
            .unwrap()
            .values()
            .all(truth)
}
pub fn validate_manifest(m: &Value) -> Vec<String> {
    let mut errors = vec![];
    for key in ["name", "entrypoint", "description"] {
        if py_trim(string(m, key)).is_empty() {
            errors.push(format!("{key} is required"));
        }
    }
    if let Some(permissions) = m["permissions"].as_array() {
        for p in permissions.iter().filter_map(Value::as_str) {
            if ["os:", "exec:", "filesystem:write", "network:raw"]
                .iter()
                .any(|prefix| p.starts_with(prefix))
            {
                errors.push(format!(
                    "permission '{p}' requires explicit approval via tool approval workflow"
                ));
            }
        }
    }
    errors
}
pub fn parse_skill_md(content: &str) -> Result<Value, ContentError> {
    if content.chars().count() > 50000 {
        return Err(policy(
            "SKILL_MANIFEST_INVALID",
            &format!(
                "SKILL.md too large ({} bytes, max 50000)",
                content.chars().count()
            ),
            "file",
        ));
    }
    let (front, body) = split_frontmatter(content).ok_or_else(|| {
        policy(
            "SKILL_MANIFEST_INVALID",
            "SKILL.md must start with YAML frontmatter (--- ... ---)",
            "file",
        )
    })?;
    let meta: Value = serde_yaml::from_str(front).map_err(|_| {
        policy(
            "SKILL_MANIFEST_INVALID",
            "YAML frontmatter must be a mapping",
            "file",
        )
    })?;
    if !meta.is_object() {
        return Err(policy(
            "SKILL_MANIFEST_INVALID",
            "YAML frontmatter must be a mapping",
            "file",
        ));
    }
    let name = py_trim(&py_string(&defaulted(&meta, "name", json!("")))).to_owned();
    let description = py_trim(&py_string(&defaulted(&meta, "description", json!("")))).to_owned();
    if name.is_empty() || description.is_empty() {
        return Err(policy(
            "SKILL_MANIFEST_INVALID",
            if name.is_empty() {
                "'name' is required in frontmatter"
            } else {
                "'description' is required in frontmatter"
            },
            "file",
        ));
    }
    if !(3..=50).contains(&name.len())
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(policy(
            "SKILL_MANIFEST_INVALID",
            "name must be kebab-case (a-z, 0-9, hyphens), 3-50 chars",
            "file",
        ));
    }
    let source = py_string(&defaulted(&meta, "source", json!("user"))).to_lowercase();
    let source = if ["user", "builtin", "marketplace"].contains(&source.as_str()) {
        source
    } else {
        "user".into()
    };
    let generated = truth(&defaulted(&meta, "generated", json!(source == "user")));
    let status = meta
        .get("lifecycle_status")
        .filter(|v| truth(v))
        .or_else(|| meta.get("status").filter(|v| truth(v)))
        .map(py_string)
        .unwrap_or_else(|| if generated { "proposed" } else { "active" }.into());
    let mut m = json!({"name":name,"title":py_trim(&py_string(&defaulted(&meta,"title",json!(name)))),"description":description,"summary":py_string(&defaulted(&meta,"summary",json!(description.chars().take(180).collect::<String>()))),"entrypoint":py_string(&defaulted(&meta,"entrypoint",json!(format!("md://{name}")))),"version":py_string(&defaulted(&meta,"version",json!("1.0.0"))),"source":source,"enabled":meta.get("enabled").filter(|v|!v.is_null()).map(truth).unwrap_or(status=="active"),"generated":generated,"lifecycle_status":status,"instructions":py_trim(body),"tags":defaulted(&meta,"tags",json!([])),"permissions":defaulted(&meta,"permissions",json!([])),"config":defaulted(&meta,"config",json!({})),"tool_schema":defaulted(&meta,"tool_schema",Value::Null),"max_context_tokens":meta.get("max_context_tokens").map(py_int).transpose()?.unwrap_or(2000),"author":py_string(&defaulted(&meta,"author",json!(""))),"review":mapping(&meta["review"]),"evaluation":mapping(&meta["evaluation"]),"rollback":mapping(&meta["rollback"]),"trigger":if meta["trigger"].is_object(){json!({"patterns":defaulted(&meta["trigger"],"patterns",json!([])),"auto":truth(&meta["trigger"]["auto"])})}else{Value::Null},"artifact_type":"bundled","skill_id":null,"version_id":null,"content_hash":null});
    if review_required(&m) {
        m["enabled"] = json!(false);
        m["lifecycle_status"] = json!("proposed");
    }
    Ok(m)
}
pub fn parse_user_skill_md(
    content: &str,
    reserved: &BTreeSet<String>,
) -> Result<(String, Value), ContentError> {
    let normalized = normalize_skill_md(content)?;
    let (front, _) = split_frontmatter(&normalized).ok_or_else(|| {
        policy(
            "SKILL_FRONTMATTER_REQUIRED",
            "SKILL.md must start with YAML frontmatter",
            "file",
        )
    })?;
    let meta: Value = serde_yaml::from_str(front).map_err(|_| {
        policy(
            "SKILL_FRONTMATTER_INVALID",
            "YAML frontmatter must be a mapping",
            "file",
        )
    })?;
    let fields = meta.as_object().ok_or_else(|| {
        policy(
            "SKILL_FRONTMATTER_INVALID",
            "YAML frontmatter must be a mapping",
            "file",
        )
    })?;
    for key in fields.keys() {
        let field = py_trim(key).to_lowercase().replace('-', "_");
        let canonical: String = field
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        if [
            "artifacttype",
            "builtin",
            "command",
            "entrypoint",
            "executable",
            "handler",
            "module",
            "path",
            "script",
            "skillid",
            "source",
            "url",
            "versionid",
        ]
        .contains(&canonical.as_str())
        {
            return Err(policy(
                "SKILL_ARTIFACT_FIELD_FORBIDDEN",
                &format!("'{field}' is server-owned for tenant Skills"),
                &field,
            ));
        }
    }
    let mut m = parse_skill_md(&normalized)?;
    if reserved.contains(string(&m, "name")) {
        return Err(policy(
            "SKILL_NAME_RESERVED",
            "Skill name is reserved by a platform artifact",
            "name",
        ));
    }
    m["source"] = json!("user");
    m["entrypoint"] = json!("db://pending/pending");
    m["artifact_type"] = json!("tenant_instruction");
    Ok((normalized, m))
}
pub fn serialize_user_skill_md(m: &Value) -> Result<String, ContentError> {
    let mut meta = serde_yaml::Mapping::new();
    for key in [
        "name",
        "title",
        "description",
        "summary",
        "version",
        "tags",
        "permissions",
        "enabled",
        "generated",
        "lifecycle_status",
        "config",
        "max_context_tokens",
        "author",
        "review",
        "evaluation",
        "rollback",
        "trigger",
        "tool_schema",
    ] {
        if matches!(key, "trigger" | "tool_schema") && m[key].is_null() {
            continue;
        }
        let value = match key {
            "tags" | "permissions" => py_list(&m[key])?,
            "config" | "review" | "evaluation" | "rollback" | "tool_schema" => py_dict(&m[key])?,
            "trigger" => json!({"patterns":py_list(&m[key]["patterns"])?,"auto":m[key]["auto"]}),
            _ => m[key].clone(),
        };
        meta.insert(
            serde_yaml::Value::String(key.into()),
            serde_yaml::to_value(value).map_err(|_| {
                policy(
                    "SKILL_MANIFEST_INVALID",
                    "Invalid Skill metadata",
                    "manifest",
                )
            })?,
        );
    }
    let front = serde_yaml::to_string(&meta).map_err(|_| {
        policy(
            "SKILL_MANIFEST_INVALID",
            "Invalid Skill metadata",
            "manifest",
        )
    })?;
    normalize_skill_md(&format!(
        "---\n{}\n---\n{}\n",
        py_trim_end(&front),
        py_trim_end(string(m, "instructions"))
    ))
}
pub fn manifest_from_artifact(record: &Value) -> Result<Value, ContentError> {
    let content = normalize_skill_md(string(record, "content"))?;
    let hash = string(record, "content_hash");
    if hash.is_empty() || sha256(content.as_bytes()) != hash {
        return Err(unavailable("SKILL_VERSION_HASH_MISMATCH"));
    }
    let mut m =
        parse_skill_md(&content).map_err(|_| unavailable("SKILL_VERSION_CONTENT_INVALID"))?;
    let meta = mapping(&record["manifest"]);
    let skill = record
        .get("skill_id")
        .filter(|v| truth(v))
        .or_else(|| meta.get("skill_id"))
        .map(py_string)
        .unwrap_or_default();
    let version = record
        .get("version_id")
        .filter(|v| truth(v))
        .or_else(|| meta.get("version_id"))
        .map(py_string)
        .unwrap_or_default();
    let entry = string(record, "entrypoint");
    if skill.is_empty() || version.is_empty() || entry != format!("db://{skill}/{version}") {
        return Err(unavailable("SKILL_VERSION_ENTRYPOINT_INVALID"));
    }
    for key in ["name", "title", "description", "version"] {
        if let Some(v) = meta
            .get(key)
            .filter(|v| truth(v))
            .or_else(|| record.get(key).filter(|v| truth(v)))
        {
            m[key] = json!(py_string(v));
        }
    }
    for key in ["summary", "author"] {
        if let Some(v) = meta.get(key).filter(|v| truth(v)) {
            m[key] = json!(py_string(v));
        }
    }
    if let Some(v) = meta.get("max_context_tokens").filter(|v| truth(v)) {
        m["max_context_tokens"] = json!(py_int(v)?);
    }
    for key in ["tags", "permissions"] {
        m[key] = strings(meta.get(key).unwrap_or(&record[key]));
    }
    for key in ["config", "review", "evaluation", "rollback"] {
        if meta[key].is_object() {
            m[key] = meta[key].clone();
        } else if key != "config" {
            m[key] = json!({});
        }
    }
    m["tool_schema"] = if meta["tool_schema"].is_object() {
        meta["tool_schema"].clone()
    } else {
        Value::Null
    };
    if meta["trigger"].is_object() {
        m["trigger"] = json!({"patterns":strings(&meta["trigger"]["patterns"]),"auto":truth(&meta["trigger"]["auto"])});
    }
    m["enabled"] = json!(
        record
            .get("enabled")
            .map(truth)
            .unwrap_or_else(|| meta.get("enabled").map(truth).unwrap_or(true))
    );
    if let Some(g) = meta.get("generated") {
        m["generated"] = json!(truth(g));
    }
    m["lifecycle_status"] = record
        .get("status")
        .filter(|v| truth(v))
        .or_else(|| meta.get("lifecycle_status").filter(|v| truth(v)))
        .cloned()
        .unwrap_or(json!("active"));
    m["entrypoint"] = json!(entry);
    m["source"] = json!("user");
    m["skill_id"] = json!(skill);
    m["version_id"] = json!(version);
    m["content_hash"] = json!(hash);
    m["artifact_type"] = json!("tenant_instruction");
    if ["name", "entrypoint", "description"]
        .iter()
        .any(|k| string(&m, k).trim().is_empty())
    {
        return Err(unavailable("SKILL_VERSION_MANIFEST_INVALID"));
    }
    Ok(m)
}
const SELECT: &str = "SELECT to_jsonb(selected) FROM (SELECT skill.skill_id,skill.tenant_id,skill.user_id,skill.name,skill.title,skill.description,skill.tags,skill.permissions,skill.enabled,skill.status AS skill_status,skill.artifact_type AS skill_artifact_type,skill.disabled_at,skill.deleted_at,version.version_id,version.version,version.revision,version.manifest,version.entrypoint,version.content,version.content_hash,version.status,version.artifact_type,version.created_at,EXISTS(SELECT 1 FROM assistant.assistant_skill_version_revocations revoked WHERE revoked.tenant_id=version.tenant_id AND revoked.version_id=version.version_id) AS revoked FROM assistant.assistant_skill_versions version JOIN assistant.assistant_skills skill ON skill.tenant_id=version.tenant_id AND skill.skill_id=version.skill_id ";
fn public_record(record: &Value) -> Result<Value, ContentError> {
    let mut m = manifest_from_artifact(record)?;
    for key in ["revision", "content", "created_at"] {
        m[key] = record[key].clone();
    }
    m["status"] = record
        .get("skill_status")
        .filter(|v| truth(v))
        .or_else(|| record.get("status").filter(|v| truth(v)))
        .cloned()
        .unwrap_or(json!("active"));
    m["revoked"] = json!(truth(&record["revoked"]));
    Ok(m)
}
#[derive(Clone)]
pub struct SkillRepository {
    pub database: ContentDatabase,
}
impl SkillRepository {
    pub async fn create_version(
        &self,
        tenant: &str,
        user: &str,
        content: &str,
        mut manifest: Value,
        creator: &str,
    ) -> Result<Value, ContentError> {
        let normalized = normalize_skill_md(content)?;
        if string(&manifest, "source") != "user"
            || string(&manifest, "artifact_type") != "tenant_instruction"
        {
            return Err(conflict("SKILL_ARTIFACT_TYPE_FORBIDDEN"));
        }
        if review_required(&manifest) {
            manifest["enabled"] = json!(false);
            manifest["lifecycle_status"] = json!("proposed");
        }
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        let creator = creator.to_owned();
        let hash = sha256(normalized.as_bytes());
        let record=self.database.transaction(move|c|Box::pin(async move{let existing:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant.assistant_skills s WHERE tenant_id=$1 AND user_id=$2 AND name=$3 FOR UPDATE").bind(&tenant).bind(&user).bind(string(&manifest,"name")).fetch_optional(&mut *c).await?;if let Some(s)=&existing{if !["tenant_instruction","legacy"].contains(&string(s,"artifact_type"))||!s["deleted_at"].is_null()||s["status"]=="deleted"{return Ok(Err(conflict("SKILL_NAME_UNAVAILABLE")))}}
        let skill=existing.as_ref().map(|v|string(v,"skill_id").to_owned()).unwrap_or_else(||uuid::Uuid::new_v4().to_string());let enabled=truth(&manifest["enabled"]);let status=if enabled{"active".into()}else{string(&manifest,"lifecycle_status").to_owned()};
        if existing.is_none(){sqlx::query("INSERT INTO assistant.assistant_skills(skill_id,tenant_id,user_id,name,title,description,tags,permissions,enabled,status,artifact_type,created_by,created_at,updated_at) VALUES($1::text::uuid,$2,$3,$4,$5,$6,$7,$8,$9,$10,'tenant_instruction',$11,NOW(),NOW())").bind(&skill).bind(&tenant).bind(&user).bind(string(&manifest,"name")).bind(string(&manifest,"title")).bind(string(&manifest,"description")).bind(&manifest["tags"]).bind(&manifest["permissions"]).bind(enabled).bind(&status).bind(&creator).execute(&mut *c).await?;}
        let revision:i32=sqlx::query_scalar("SELECT (COALESCE(MAX(revision),0)+1)::integer FROM assistant.assistant_skill_versions WHERE skill_id=$1::text::uuid").bind(&skill).fetch_one(&mut *c).await?;let version=uuid::Uuid::new_v4().to_string();let entry=format!("db://{skill}/{version}");manifest["entrypoint"]=json!(entry);manifest["skill_id"]=json!(skill);manifest["version_id"]=json!(version);manifest["content_hash"]=json!(hash);
        sqlx::query("INSERT INTO assistant.assistant_skill_versions(version_id,skill_id,tenant_id,user_id,version,revision,manifest,entrypoint,content,content_hash,artifact_type,status,created_by,created_at) VALUES($1::text::uuid,$2::text::uuid,$3,$4,$5,$6,$7,$8,$9,$10,'tenant_instruction',$11,$12,NOW())").bind(&version).bind(&skill).bind(&tenant).bind(&user).bind(string(&manifest,"version")).bind(revision).bind(&manifest).bind(&entry).bind(&normalized).bind(&hash).bind(&status).bind(&creator).execute(&mut *c).await?;
        sqlx::query("UPDATE assistant.assistant_skills SET title=$4,description=$5,tags=$6,permissions=$7,enabled=$8,status=$9,artifact_type='tenant_instruction',disabled_at=CASE WHEN $8 THEN NULL ELSE NOW() END,updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND skill_id=$3::text::uuid").bind(&tenant).bind(&user).bind(&skill).bind(string(&manifest,"title")).bind(string(&manifest,"description")).bind(&manifest["tags"]).bind(&manifest["permissions"]).bind(enabled).bind(&status).execute(&mut *c).await?;
        let query=format!("{SELECT} WHERE version.tenant_id=$1 AND version.version_id=$2::text::uuid) selected");let row:Value=sqlx::query_scalar(&query).bind(&tenant).bind(&version).fetch_one(c).await?;Ok(Ok(row))})).await?;
        public_record(&record)
    }
    pub async fn list_for_actor(
        &self,
        tenant: &str,
        user: &str,
        enabled_only: bool,
    ) -> Result<Vec<Value>, ContentError> {
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        let rows:Vec<Value>=self.database.run(move|c|Box::pin(async move{let query=format!("{SELECT} JOIN LATERAL(SELECT candidate.version_id FROM assistant.assistant_skill_versions candidate WHERE candidate.skill_id=skill.skill_id ORDER BY candidate.revision DESC LIMIT 1) latest ON latest.version_id=version.version_id WHERE skill.tenant_id=$1 AND skill.user_id=$2 AND skill.artifact_type='tenant_instruction' AND version.artifact_type='tenant_instruction' AND skill.deleted_at IS NULL AND skill.status<>'deleted' AND (NOT $3::boolean OR (skill.enabled=TRUE AND skill.status='active')) ORDER BY skill.name,skill.user_id) selected");sqlx::query_scalar(&query).bind(tenant).bind(user).bind(enabled_only).fetch_all(c).await})).await?;
        rows.iter().map(public_record).collect()
    }
    pub async fn get_for_actor(
        &self,
        tenant: &str,
        user: &str,
        name: &str,
    ) -> Result<Value, ContentError> {
        self.list_for_actor(tenant, user, false)
            .await?
            .into_iter()
            .find(|v| v["name"] == name)
            .ok_or_else(not_found)
    }
    pub async fn authorize_version(
        &self,
        tenant: &str,
        user: &str,
        version: &str,
        admin: bool,
    ) -> Result<Value, ContentError> {
        let version = uuid::Uuid::parse_str(version)
            .map_err(|_| unavailable("SKILL_VERSION_UNAVAILABLE"))?
            .to_string();
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        let row:Option<Value>=self.database.run(move|c|Box::pin(async move{let query=format!("{SELECT} WHERE version.tenant_id=$1 AND version.version_id=$2::text::uuid AND ($4::boolean OR skill.user_id=$3 OR skill.user_id='*')) selected");sqlx::query_scalar(&query).bind(tenant).bind(version).bind(user).bind(admin).fetch_optional(c).await})).await?;
        let row = row.ok_or_else(|| unavailable("SKILL_VERSION_UNAVAILABLE"))?;
        if !truth(&row["enabled"])
            || row["skill_status"] != "active"
            || row["status"] != "active"
            || !row["deleted_at"].is_null()
            || truth(&row["revoked"])
            || row["artifact_type"] != "tenant_instruction"
        {
            return Err(unavailable("SKILL_VERSION_UNAVAILABLE"));
        }
        public_record(&row)
    }
    pub async fn load_versions(
        &self,
        tenant: &str,
        user: &str,
        versions: &BTreeSet<String>,
    ) -> Result<Vec<Value>, ContentError> {
        let mut out = vec![];
        for version in versions {
            out.push(manifest_from_artifact(
                &self.authorize_version(tenant, user, version, false).await?,
            )?);
        }
        Ok(out)
    }
    pub async fn set_enabled(
        &self,
        tenant: &str,
        user: &str,
        name: &str,
        enabled: bool,
    ) -> Result<Value, ContentError> {
        let current = self.get_for_actor(tenant, user, name).await?;
        if enabled && review_required(&manifest_from_artifact(&current)?) {
            return Err(conflict("SKILL_ACTIVATION_GATES_REQUIRED"));
        }
        let (t, u, n) = (tenant.to_owned(), user.to_owned(), name.to_owned());
        let changed=self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.assistant_skills SET enabled=$4,status=CASE WHEN $4 THEN 'active' ELSE 'disabled' END,disabled_at=CASE WHEN $4 THEN NULL ELSE NOW() END,updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND name=$3 AND artifact_type='tenant_instruction' AND deleted_at IS NULL AND status<>'deleted'").bind(t).bind(u).bind(n).bind(enabled).execute(c).await.map(|r|r.rows_affected())})).await?;
        if changed != 1 {
            return Err(not_found());
        }
        self.get_for_actor(tenant, user, name).await
    }
    pub async fn delete(&self, tenant: &str, user: &str, name: &str) -> Result<(), ContentError> {
        let (t, u, n) = (tenant.to_owned(), user.to_owned(), name.to_owned());
        let changed=self.database.run(move|c|Box::pin(async move{sqlx::query("UPDATE assistant.assistant_skills SET enabled=FALSE,status='deleted',deleted_at=NOW(),disabled_at=COALESCE(disabled_at,NOW()),updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND name=$3 AND artifact_type='tenant_instruction' AND deleted_at IS NULL").bind(t).bind(u).bind(n).execute(c).await.map(|r|r.rows_affected())})).await?;
        if changed != 1 {
            Err(not_found())
        } else {
            Ok(())
        }
    }
    pub async fn update_metadata(
        &self,
        tenant: &str,
        user: &str,
        name: &str,
        changes: &Value,
    ) -> Result<Value, ContentError> {
        let current = self.get_for_actor(tenant, user, name).await?;
        if current["artifact_type"] != "tenant_instruction" || current["user_id"] == "*" {
            return Err(not_found());
        }
        let mut m = manifest_from_artifact(&current)?;
        for key in ["title", "description", "tags", "permissions", "enabled"] {
            if let Some(v) = changes.get(key) {
                if v.is_null() {
                    return Err(policy(
                        "SKILL_MANIFEST_INVALID",
                        "Skill metadata fields cannot be null",
                        key,
                    ));
                }
                m[key] = v.clone();
            }
        }
        if truth(&m["enabled"]) && review_required(&m) {
            return Err(conflict("SKILL_ACTIVATION_GATES_REQUIRED"));
        }
        let errors = validate_manifest(&m);
        if !errors.is_empty() {
            return Err(policy(
                "SKILL_MANIFEST_INVALID",
                &errors.join("; "),
                "manifest",
            ));
        }
        self.create_version(tenant, user, &serialize_user_skill_md(&m)?, m, user)
            .await
    }
}
pub fn execute_instructions(m: &Value) -> Value {
    let entry = string(m, "entrypoint");
    if entry.starts_with("db://")
        && (string(m, "skill_id").is_empty()
            || string(m, "version_id").is_empty()
            || entry != format!("db://{}/{}", string(m, "skill_id"), string(m, "version_id")))
    {
        return json!({"success":false,"error":"Skill artifact identity is invalid"});
    }
    if !entry.starts_with("db://") && !entry.starts_with("md://") {
        return json!({"success":false,"error":format!("Unknown entrypoint type: {entry}")});
    }
    json!({"success":true,"type":"skill_instructions","result":if string(m,"instructions").is_empty(){format!("Skill '{}' activated. Follow the skill description to proceed.",string(m,"name"))}else{string(m,"instructions").into()}})
}
pub fn platform_manifest() -> Value {
    json!({"name":"skill-create","title":"Skill Creator","description":"Create, edit, and manage custom skills for this AI assistant","entrypoint":"builtin://skill_create","summary":"Help users create new skills through conversation","version":"1.0.0","tags":["meta","creation","management"],"permissions":["skill:create","skill:edit","llm:generate"],"enabled":true,"skill_id":null,"version_id":null,"content_hash":null,"artifact_type":"bundled","instructions":r#"# Skill Creator

## Instructions
Help the user create a new skill by gathering requirements and generating a
reviewable SKILL.md file. Follow the propose-review-test-enable loop.

### Workflow
1. Ask what the skill should do (purpose, trigger scenarios)
2. Ask what tools/permissions it needs (kb:read, llm:generate, web:search, etc.)
3. Ask for example inputs/outputs
4. Generate the SKILL.md with proper YAML frontmatter + markdown instructions
5. Mark generated skills as proposed and disabled by default
6. Validate the manifest (check for dangerous permissions)
7. Require independent critic evidence, eval evidence, and rollback metadata
8. Enable only after review approves the exact manifest version

### Validation Rules
- name: kebab-case, 3-50 chars, unique
- permissions: no os:*, exec:*, filesystem:write*
- instructions: < 5000 tokens
- config values: must be JSON-serializable
- generated skills must include generated: true, enabled: false, and
  lifecycle_status: proposed until activation gates pass

### Output
Return the complete proposed SKILL.md content that the user can review. Do not
register or enable the skill during creation.
"#,"trigger":null,"config":{},"source":"builtin","tool_schema":null,"max_context_tokens":2000,"author":"","generated":false,"lifecycle_status":"active","review":{},"evaluation":{},"rollback":{},"status":"active","revision":0,"revoked":false})
}
#[derive(Clone)]
pub struct SkillsApi {
    pub repository: SkillRepository,
    pub access: Arc<dyn ContentAccess>,
    pub platform_manifest: Value,
}
impl SkillsApi {
    pub fn new(repository: SkillRepository, access: Arc<dyn ContentAccess>) -> Self {
        Self {
            repository,
            access,
            platform_manifest: platform_manifest(),
        }
    }
    fn reserved(&self) -> BTreeSet<String> {
        BTreeSet::from([string(&self.platform_manifest, "name").into()])
    }
    fn reject_platform(&self, name: &str) -> Result<(), ContentError> {
        if self.reserved().contains(name) {
            Err(not_found())
        } else {
            Ok(())
        }
    }
    pub async fn upload(
        &self,
        user: &ContentIdentity,
        filename: &str,
        raw: &[u8],
    ) -> Result<Value, ContentError> {
        self.access
            .require_capability(user, "GatewaySkillWrite")
            .await?;
        if !filename.ends_with(".md") {
            return Err(ContentError::code(
                400,
                "SKILL_FILE_TYPE_INVALID",
                "File must be a .md SKILL.md artifact",
            ));
        }
        if raw.len() > 50000 {
            return Err(ContentError::code(
                400,
                "SKILL_FILE_TOO_LARGE",
                "SKILL.md exceeds the 50KB limit",
            ));
        }
        let text = std::str::from_utf8(raw).map_err(|_| {
            ContentError::code(
                400,
                "SKILL_ENCODING_INVALID",
                "SKILL.md must be valid UTF-8",
            )
        })?;
        let (text, m) = parse_user_skill_md(text, &self.reserved())?;
        let errors = validate_manifest(&m);
        if !errors.is_empty() {
            return Err(policy(
                "SKILL_MANIFEST_INVALID",
                &errors.join("; "),
                "manifest",
            ));
        }
        self.repository
            .create_version(&user.tenant_id, &user.user_id, &text, m, &user.user_id)
            .await
    }
}
fn catalog(mut item: Value, content: bool) -> Value {
    if !content {
        if let Some(m) = item.as_object_mut() {
            m.remove("content");
            m.remove("instructions");
        }
    }
    item
}
pub fn router(api: Arc<SkillsApi>) -> Router {
    Router::new()
        .route("/skills", get(list))
        .route("/skills/upload", post(upload))
        .route("/skills/{name}", get(get_one).patch(update).delete(delete))
        .route("/skills/{name}/test", post(test))
        .route("/skills/{name}/enable", post(enable))
        .route("/skills/{name}/disable", post(disable))
        .with_state(api)
}
async fn actor(
    api: &SkillsApi,
    headers: &HeaderMap,
    write: bool,
) -> Result<ContentIdentity, ContentError> {
    let u = api.access.resolve(headers).await?;
    api.access
        .require_capability(
            &u,
            if write {
                "GatewaySkillWrite"
            } else {
                "GatewaySkillRead"
            },
        )
        .await?;
    Ok(u)
}
async fn list(
    State(api): State<Arc<SkillsApi>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, false).await?;
    let enabled = q
        .get("enabled_only")
        .map(|s| {
            crate::rate_config_api::boolean(&json!(s))
                .map_err(|_| ContentError::new(422, "enabled_only must be a boolean"))
        })
        .transpose()?
        .unwrap_or(true);
    let mut rows = vec![catalog(api.platform_manifest.clone(), false)];
    rows.extend(
        api.repository
            .list_for_actor(&u.tenant_id, &u.user_id, enabled)
            .await?
            .into_iter()
            .map(|v| catalog(v, false)),
    );
    Ok(Json(json!({"total":rows.len(),"skills":rows})))
}
async fn get_one(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, false).await?;
    Ok(Json(if api.reserved().contains(&name) {
        api.platform_manifest.clone()
    } else {
        api.repository
            .get_for_actor(&u.tenant_id, &u.user_id, &name)
            .await?
    }))
}
async fn update(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, true).await?;
    api.reject_platform(&name)?;
    for key in ["title", "description"] {
        if body
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(policy(
                "SKILL_MANIFEST_INVALID",
                "Skill metadata field must be a string",
                key,
            ));
        }
    }
    for key in ["tags", "permissions"] {
        if body.get(key).is_some_and(|v| {
            !v.is_null()
                && !v
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_string))
        }) {
            return Err(policy(
                "SKILL_MANIFEST_INVALID",
                "Skill metadata field must be a list of strings",
                key,
            ));
        }
    }
    if let Some(enabled) = body.get("enabled").filter(|v| !v.is_null()) {
        body["enabled"] = json!(crate::rate_config_api::boolean(enabled).map_err(|_| policy(
            "SKILL_MANIFEST_INVALID",
            "enabled must be a boolean",
            "enabled"
        ))?);
    }
    let mut changes = Map::new();
    for k in ["title", "description", "tags", "permissions"] {
        if let Some(v) = body.get(k) {
            changes.insert(k.into(), v.clone());
        }
    }
    let enabled = body.get("enabled").filter(|v| !v.is_null());
    let row = if changes.is_empty() {
        if let Some(enabled) = enabled {
            api.repository
                .set_enabled(&u.tenant_id, &u.user_id, &name, truth(enabled))
                .await?
        } else {
            api.repository
                .get_for_actor(&u.tenant_id, &u.user_id, &name)
                .await?
        }
    } else {
        if let Some(enabled) = enabled {
            changes.insert("enabled".into(), enabled.clone());
        }
        api.repository
            .update_metadata(&u.tenant_id, &u.user_id, &name, &Value::Object(changes))
            .await?
    };
    Ok(Json(row))
}
async fn delete(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, true).await?;
    api.reject_platform(&name)?;
    api.repository
        .delete(&u.tenant_id, &u.user_id, &name)
        .await?;
    Ok(Json(json!({"deleted":true,"name":name})))
}
async fn test(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, true).await?;
    let input = super::required(&body, "input")?;
    if api.reserved().contains(&name) {
        return Ok(Json(
            json!({"success":true,"type":"skill_instructions","result":format!("The user wants to create a skill: {input}\n\nFollow the Skill Creator propose-review-test-enable workflow:\n1. Ask clarifying questions about the skill's purpose and triggers\n2. Determine required permissions\n3. Generate a complete proposed SKILL.md file with generated: true, enabled: false, and lifecycle_status: proposed\n4. Present it to the user for review\n5. Do not register or enable it until independent critic evidence, eval evidence, and rollback metadata are recorded\n\nOutput the proposed SKILL.md in a code block when ready.")}),
        ));
    }
    let row = api
        .repository
        .get_for_actor(&u.tenant_id, &u.user_id, &name)
        .await?;
    Ok(Json(execute_instructions(&manifest_from_artifact(&row)?)))
}
async fn enabled(
    api: Arc<SkillsApi>,
    headers: HeaderMap,
    name: String,
    value: bool,
) -> Result<Json<Value>, ContentError> {
    let u = actor(&api, &headers, true).await?;
    api.reject_platform(&name)?;
    Ok(Json(catalog(
        api.repository
            .set_enabled(&u.tenant_id, &u.user_id, &name, value)
            .await?,
        false,
    )))
}
async fn enable(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    enabled(api, headers, name, true).await
}
async fn disable(
    State(api): State<Arc<SkillsApi>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    enabled(api, headers, name, false).await
}

async fn upload(
    State(api): State<Arc<SkillsApi>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Value>), ContentError> {
    let u = actor(&api, &headers, true).await?;
    while let Some(mut field) = multipart.next_field().await.map_err(|_| {
        policy(
            "SKILL_FILE_TYPE_INVALID",
            "Invalid multipart upload",
            "file",
        )
    })? {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().unwrap_or("").to_owned();
        let mut raw = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| policy("SKILL_CONTENT_INVALID", "Invalid upload", "file"))?
        {
            if raw.len() + chunk.len() > 50000 {
                return Err(ContentError::code(
                    400,
                    "SKILL_FILE_TOO_LARGE",
                    "SKILL.md exceeds the 50KB limit",
                ));
            }
            raw.extend_from_slice(&chunk);
        }
        return Ok((
            StatusCode::CREATED,
            Json(api.upload(&u, &name, &raw).await?),
        ));
    }
    Err(policy(
        "SKILL_FILE_TYPE_INVALID",
        "File is required",
        "file",
    ))
}
