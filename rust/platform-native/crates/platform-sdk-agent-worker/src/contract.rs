//! Private launch contract. Credentials never implement Debug or Serialize.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
pub const PROTOCOL: &str = "ai-platform/sdk-agent/v1";
pub const MAX_BODY: usize = 393216;
pub const MAX_EVENT: usize = 1048576;
pub const MAX_OUTPUT: usize = 4194304;
pub type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone)]
pub struct Launch {
    pub body: Value,
    pub model_plane: Option<crate::model_plane::ModelPlane>,
    pub scope: String,
}
pub fn exact(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
// Python json.dumps defaults to ensure_ascii=True. Scope/receipt compatibility includes
// surrogate-pair escaping and recursive sorting, not just a Rust JSON serialization.
pub fn canonical(v: &Value, sort: bool) -> String {
    match v {
        Value::String(s) => {
            let encoded = serde_json::to_string(s).unwrap();
            let mut out = String::new();
            for c in encoded.chars() {
                if c <= '~' {
                    out.push(c);
                } else {
                    let mut units = [0; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{u:04x}"));
                    }
                }
            }
            out
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|x| canonical(x, sort))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            if sort {
                keys.sort();
            }
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| format!("{}:{}", canonical(&json!(k), sort), canonical(&o[*k], sort)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Number(number) if number.is_f64() => {
            let value = number.as_f64().unwrap();
            if value != 0.0 && (value.abs() < 1e-4 || value.abs() >= 1e16) {
                let scientific = format!("{value:e}");
                let (mantissa, exponent) = scientific.split_once('e').unwrap();
                let exponent: i32 = exponent.parse().unwrap();
                format!(
                    "{mantissa}e{}{exponent_abs:02}",
                    if exponent < 0 { "-" } else { "+" },
                    exponent_abs = exponent.unsigned_abs()
                )
            } else {
                let value = value.to_string();
                if value.contains('.') {
                    value
                } else {
                    format!("{value}.0")
                }
            }
        }
        _ => v.to_string(),
    }
}
pub fn identity(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        !s.trim().is_empty() && s.chars().count() <= 256 && !s.chars().any(|c| c < ' ')
    })
}
pub fn scope(body: &Value) -> Result<String> {
    let fields = [
        "tenant_id",
        "user_id",
        "service_id",
        "framework",
        "session_id",
    ];
    if !fields.iter().all(|k| identity(&body[*k]))
        || !matches!(
            body["framework"].as_str(),
            Some("openai_agents" | "claude_agent_sdk")
        )
    {
        return Err("sdk_request_invalid");
    }
    Ok(hash(
        canonical(
            &Value::Array(fields.iter().map(|k| body[*k].clone()).collect()),
            false,
        )
        .as_bytes(),
    ))
}
fn hex(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
impl Launch {
    pub fn parse(body: Value) -> Result<Self> {
        let keys = [
            "protocol",
            "framework",
            "service_id",
            "tenant_id",
            "user_id",
            "request_id",
            "session_id",
            "agent",
            "input",
            "stream",
        ];
        let o = body.as_object().ok_or("sdk_request_invalid")?;
        if keys.iter().any(|k| !o.contains_key(*k))
            || o.keys().any(|k| {
                !keys.contains(&k.as_str()) && k != "read_bridge" && k != "capability_bridge"
            })
            || body["protocol"] != PROTOCOL
        {
            return Err("sdk_request_invalid");
        }
        let scope = scope(&body)?;
        if !identity(&body["request_id"])
            || !body["stream"].is_boolean()
            || !exact(&body["agent"], &["model", "instructions", "max_turns"])
        {
            return Err("sdk_request_invalid");
        }
        if !body["agent"]["max_turns"]
            .as_u64()
            .is_some_and(|n| (1..=32).contains(&n))
        {
            return Err("sdk_request_invalid");
        }
        for (v, limit, blank) in [
            (&body["agent"]["model"], 160, false),
            (&body["agent"]["instructions"], 32768, true),
            (&body["input"], 262144, false),
        ] {
            if !v.as_str().is_some_and(|s| {
                (blank && s.trim().is_empty()) || (s.len() <= limit && !s.trim().is_empty())
            }) {
                return Err("sdk_request_invalid");
            }
        }
        if let Some(b) = body.get("read_bridge").filter(|v| !v.is_null()) {
            if !exact(b, &["grant", "dataset_ids"]) || !hex(&b["grant"]) {
                return Err("sdk_request_invalid");
            }
            let ids = b["dataset_ids"].as_array().ok_or("sdk_request_invalid")?;
            let mut seen = HashSet::new();
            if !(1..=16).contains(&ids.len())
                || ids.iter().any(|id| {
                    id.as_str()
                        .is_none_or(|s| uuid::Uuid::parse_str(s).is_err() || !seen.insert(s))
                })
            {
                return Err("sdk_request_invalid");
            }
        }
        if let Some(b) = body.get("capability_bridge").filter(|v| !v.is_null()) {
            if !exact(b, &["grant", "run_id", "tools", "timeout_seconds"])
                || !hex(&b["grant"])
                || b["run_id"]
                    .as_str()
                    .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
            {
                return Err("sdk_request_invalid");
            }
            let tools = b["tools"].as_array().ok_or("sdk_request_invalid")?;
            if tools.len() > 16
                || b["timeout_seconds"].as_u64() != Some(if tools.is_empty() { 300 } else { 1800 })
                || (!tools.is_empty() && body["stream"] != true)
            {
                return Err("sdk_request_invalid");
            }
            let mut names = HashSet::new();
            let mut ids = HashSet::new();
            for t in tools {
                if !exact(
                    t,
                    &[
                        "name",
                        "tool",
                        "namespace",
                        "description",
                        "input_schema",
                        "id",
                        "schema_hash",
                        "effect",
                    ],
                ) {
                    return Err("sdk_request_invalid");
                }
                let name = t["name"].as_str().ok_or("sdk_request_invalid")?;
                let suffix = name
                    .strip_prefix("platform_capability_")
                    .ok_or("sdk_request_invalid")?;
                if suffix.is_empty()
                    || !suffix.bytes().all(|b| b.is_ascii_digit())
                    || !names.insert(name)
                    || !ids.insert((
                        t["namespace"].clone().to_string(),
                        t["tool"].clone().to_string(),
                    ))
                {
                    return Err("sdk_request_invalid");
                }
                if t["tool"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.chars().count() > 160)
                    || !t["id"].is_string()
                    || (!t["namespace"].is_null()
                        && t["namespace"]
                            .as_str()
                            .is_none_or(|s| s.is_empty() || s.chars().count() > 64))
                    || t["description"]
                        .as_str()
                        .is_none_or(|s| s.chars().count() > 16384)
                    || !t["input_schema"].is_object()
                    || canonical(&t["input_schema"], false).len() > 65536
                    || !matches!(t["effect"].as_str(), Some("read" | "write" | "unknown"))
                    || t["schema_hash"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("sha256:"))
                        .is_none_or(|s| !hex(&json!(s)))
                {
                    return Err("sdk_request_invalid");
                }
            }
        }
        if serde_json::to_vec(&json!({"request":body}))
            .map_err(|_| "sdk_request_invalid")?
            .len()
            + 1
            > 512 * 1024
        {
            return Err("sdk_request_too_large");
        }
        Ok(Self {
            body,
            model_plane: None,
            scope,
        })
    }
    pub fn bridge(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|v| !v.is_null())
    }
    pub fn timeout(&self) -> u64 {
        self.body["capability_bridge"]["timeout_seconds"]
            .as_u64()
            .unwrap_or(300)
    }
    pub fn fingerprint(&self) -> String {
        let mut b = self.body.clone();
        for key in ["read_bridge", "capability_bridge"] {
            if let Some(o) = b.get_mut(key).and_then(Value::as_object_mut) {
                o.remove("grant");
            }
        }
        hash(canonical(&b, true).as_bytes())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    pub fn body() -> Value {
        json!({"protocol":PROTOCOL,"framework":"openai_agents","service_id":"svc","tenant_id":"租户","user_id":"u","request_id":"r","session_id":"s","agent":{"model":"m","instructions":"","max_turns":2},"input":"hello","stream":true})
    }
    #[test]
    fn strict_launch() {
        assert!(Launch::parse(body()).is_ok());
        let mut b = body();
        b["agent"]["max_turns"] = json!(true);
        assert!(Launch::parse(b).is_err());
    }
    #[test]
    fn ascii_canonical() {
        assert_eq!(
            canonical(&json!(["中", "😀"]), false),
            "[\"\\u4e2d\",\"\\ud83d\\ude00\"]"
        );
    }
    #[test]
    fn python_float_and_del_fingerprint_encoding() {
        assert_eq!(
            canonical(&json!([1e-7, 1e-4, 1e16, 1e15, -0.0, "\u{7f}"]), true),
            "[1e-07,0.0001,1e+16,1000000000000000.0,-0.0,\"\\u007f\"]"
        );
    }
    #[test]
    fn grant_not_fingerprint() {
        let mut a = body();
        a["read_bridge"] =
            json!({"grant":"a".repeat(64),"dataset_ids":["00000000-0000-0000-0000-000000000001"]});
        let first = Launch::parse(a.clone()).unwrap().fingerprint();
        a["read_bridge"]["grant"] = json!("b".repeat(64));
        assert_eq!(first, Launch::parse(a).unwrap().fingerprint());
    }
}
